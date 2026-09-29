//! src/watcher/inotify_poc.rs — 裸 libc inotify 实证（测试专用）
//!
//! 用 `libc::inotify_init1 / inotify_add_watch / read` 直接观察 Linux 内核事件，
//! 不引 notify / inotify / tokio 任何依赖。
//!
//! 三个测试：
//!   · poc_rename_over_reports_create_on_original_path —— 【第 1 部分】rename 覆盖到底报什么
//!   · suppressed_batch_never_yields_new_file          —— 【第 4 部分】带抑制的 50 次写回
//!   · unsuppressed_batch_does_yield_new_file          —— 【第 4 部分】对照组：关掉抑制
//!
//! 打印内容用 `--nocapture` 可见，已加 `EVENT` / `事件序列` 关键字便于 grep。

use super::classify::{classify, watch_kind_from_mask, IgnoreReason, WatchDecision};
use super::suppress::{SelfWriteRegistry, DEFAULT_TTL};
use super::test_support::TempDir;

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 一次 inotify 事件（裸 inotify_event 的四个字段 + 文件名）
#[derive(Debug, Clone)]
struct RawEvent {
    wd: i32,
    mask: u32,
    cookie: u32,
    name: String,
}

/// watch 的掩码：覆盖规格要求的全部七类
fn watch_mask() -> u32 {
    libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_CLOSE_WRITE
        | libc::IN_MODIFY
        | libc::IN_ATTRIB
}

/// 位掩码 → 可读名字（打印用）
fn mask_names(mask: u32) -> String {
    const BITS: &[(u32, &str)] = &[
        (libc::IN_ACCESS, "IN_ACCESS"),
        (libc::IN_MODIFY, "IN_MODIFY"),
        (libc::IN_ATTRIB, "IN_ATTRIB"),
        (libc::IN_CLOSE_WRITE, "IN_CLOSE_WRITE"),
        (libc::IN_CLOSE_NOWRITE, "IN_CLOSE_NOWRITE"),
        (libc::IN_OPEN, "IN_OPEN"),
        (libc::IN_MOVED_FROM, "IN_MOVED_FROM"),
        (libc::IN_MOVED_TO, "IN_MOVED_TO"),
        (libc::IN_CREATE, "IN_CREATE"),
        (libc::IN_DELETE, "IN_DELETE"),
        (libc::IN_DELETE_SELF, "IN_DELETE_SELF"),
        (libc::IN_MOVE_SELF, "IN_MOVE_SELF"),
        (libc::IN_UNMOUNT, "IN_UNMOUNT"),
        (libc::IN_Q_OVERFLOW, "IN_Q_OVERFLOW"),
        (libc::IN_IGNORED, "IN_IGNORED"),
        (libc::IN_ISDIR, "IN_ISDIR"),
    ];
    let mut parts: Vec<&str> = Vec::new();
    for (bit, name) in BITS {
        if mask & bit != 0 {
            parts.push(name);
        }
    }
    if parts.is_empty() {
        format!("(未识别位 0x{mask:08x})")
    } else {
        parts.join("|")
    }
}

/// 把 `read(2)` 读到的字节流解析成事件列表（read_unaligned，不假设对齐）
fn parse_events(buf: &[u8]) -> Vec<RawEvent> {
    let hdr = std::mem::size_of::<libc::inotify_event>();
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + hdr <= buf.len() {
        let ev: libc::inotify_event =
            unsafe { std::ptr::read_unaligned(buf[off..].as_ptr() as *const libc::inotify_event) };
        let start = off + hdr;
        let end = start + ev.len as usize;
        if end > buf.len() {
            break;
        }
        let bytes = buf[start..end].split(|b| *b == 0).next().unwrap_or(&[]);
        out.push(RawEvent {
            wd: ev.wd,
            mask: ev.mask,
            cookie: ev.cookie,
            name: String::from_utf8_lossy(bytes).into_owned(),
        });
        off = end;
    }
    out
}

/// 后台持续 read 事件的收集器；Drop 时停线程并 close(fd)
struct InotifyCollector {
    events: Arc<Mutex<Vec<RawEvent>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl InotifyCollector {
    fn start(dir: &Path) -> io::Result<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let cpath = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "路径含 NUL"))?;
        let wd = unsafe { libc::inotify_add_watch(fd, cpath.as_ptr(), watch_mask()) };
        if wd < 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(e);
        }

        let events: Arc<Mutex<Vec<RawEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let events = Arc::clone(&events);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 16384];
                while !stop.load(Ordering::Relaxed) {
                    let n = unsafe {
                        libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
                    };
                    if n > 0 {
                        let parsed = parse_events(&buf[..n as usize]);
                        match events.lock() {
                            Ok(mut v) => v.extend(parsed),
                            Err(p) => p.into_inner().extend(parsed),
                        }
                        continue; // 立刻再读，直到 EAGAIN
                    }
                    if n == 0 {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    match io::Error::last_os_error().raw_os_error() {
                        Some(libc::EAGAIN) => std::thread::sleep(Duration::from_millis(1)),
                        Some(libc::EINTR) => {}
                        _ => break,
                    }
                }
                unsafe { libc::close(fd) };
            })
        };
        Ok(Self { events, stop, handle: Some(handle) })
    }

    fn snapshot(&self) -> Vec<RawEvent> {
        match self.events.lock() {
            Ok(v) => v.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    /// 轮询收尾：安静 `quiet` 无新事件即返回；最坏 `cap` 兜底（不用长 sleep）
    fn settle(&self, quiet: Duration, cap: Duration) -> Vec<RawEvent> {
        let t0 = Instant::now();
        let mut last = self.snapshot().len();
        let mut last_change = Instant::now();
        while last_change.elapsed() < quiet && t0.elapsed() < cap {
            std::thread::sleep(Duration::from_millis(2));
            let n = self.snapshot().len();
            if n != last {
                last = n;
                last_change = Instant::now();
            }
        }
        self.snapshot()
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for InotifyCollector {
    fn drop(&mut self) {
        self.stop();
    }
}

// ───────────────────────── 打印 ─────────────────────────

fn print_sequence(title: &str, dir: &Path, target: &Path, evs: &[RawEvent]) {
    print_sequence_capped(title, dir, target, evs, 24)
}

fn print_sequence_capped(title: &str, dir: &Path, target: &Path, evs: &[RawEvent], cap: usize) {
    let tname = target.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    println!("\n===== 事件序列：{title} =====");
    println!("WATCH_DIR : {}", dir.display());
    println!("TARGET    : {}", target.display());
    println!("事件总数  : {}（下面最多列 {cap} 条）", evs.len());
    for (i, e) in evs.iter().take(cap).enumerate() {
        let arrow = if e.name == tname { "  <== 原文件路径" } else { "" };
        println!(
            "EVENT[{i:02}] mask=0x{:08x} [{:26}] cookie={:<6} wd={} name={:?}{arrow}",
            e.mask,
            mask_names(e.mask),
            e.cookie,
            e.wd,
            e.name
        );
    }
    if evs.len() > cap {
        println!("... 省略 {} 条（判定汇总里有全量统计）", evs.len() - cap);
    }
    // 目标路径上的掩码集合 —— 本次 PoC 的核心观测
    let mut on_target: Vec<String> = Vec::new();
    for e in evs.iter().filter(|e| e.name == tname) {
        let n = mask_names(e.mask);
        if !on_target.contains(&n) {
            on_target.push(n);
        }
    }
    println!("TARGET_MASKS : {:?}", on_target);
    let created = evs.iter().any(|e| {
        e.name == tname && (e.mask & (libc::IN_CREATE | libc::IN_MOVED_TO)) != 0
    });
    let deleted = evs.iter().any(|e| {
        e.name == tname && (e.mask & (libc::IN_DELETE | libc::IN_DELETE_SELF | libc::IN_MOVED_FROM)) != 0
    });
    println!("TARGET_HAS_CREATE_SEMANTICS : {created}");
    println!("TARGET_HAS_DELETE_SEMANTICS : {deleted}");
    println!("===== 事件序列结束：{title} =====\n");
}

fn print_decisions(title: &str, rows: &[(RawEvent, WatchDecision)]) {
    println!("\n===== 判定汇总：{title} =====");
    let mut new_file = 0usize;
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (e, d) in rows {
        let key = format!("{} -> {d:?}", mask_names(e.mask));
        match counts.iter().position(|(k, _)| *k == key) {
            Some(i) => counts[i].1 += 1,
            None => counts.push((key, 1)),
        }
        if *d == WatchDecision::NewFile {
            new_file += 1;
        }
    }
    println!("喂给 classify 的事件数 : {}", rows.len());
    for (k, c) in &counts {
        println!("DECISION {c:>4} × {k}");
    }
    println!("NewFile 判定总数 : {new_file}");
    println!("===== 判定汇总结束：{title} =====\n");
}

// ───────────────────────── 测试 ─────────────────────────

const AUDIO_BYTES: usize = 1024;
const BATCH: usize = 50;

fn fake_audio(seed: usize) -> Vec<u8> {
    let mut v = vec![0u8; AUDIO_BYTES];
    v[0] = 0x49;
    v[1] = 0x44;
    v[2] = 0x33; // "ID3"
    v[3] = (seed & 0xFF) as u8;
    v[AUDIO_BYTES - 1] = (seed >> 8) as u8;
    v
}

/// 【第 1 部分】rename 覆盖在原文件路径上到底报什么？
#[test]
fn poc_rename_over_reports_create_on_original_path() {
    let dir = TempDir::new("poc-rename");
    let file = dir.path().join("song.mp3");
    std::fs::write(&file, fake_audio(0)).expect("准备假音频");

    let mut w = InotifyCollector::start(dir.path()).expect("inotify_init1/add_watch 失败");
    std::thread::sleep(Duration::from_millis(20)); // 等 watch 就位（add_watch 返回即已生效，此处仅稳妥）

    crate::tag::write::atomic::atomic_replace(&file, fake_audio(1), None).expect("原子写回");
    let evs = w.settle(Duration::from_millis(60), Duration::from_millis(1000));
    w.stop();

    print_sequence("原子写回 atomic_replace（rename 覆盖）", dir.path(), &file, &evs);

    assert!(!evs.is_empty(), "一个事件都没收到 —— 测试本身无效（watch 没生效？）");

    // 规格要求的断言：目标路径上出现「创建语义」事件
    let hit = evs
        .iter()
        .find(|e| e.name == "song.mp3" && (e.mask & (libc::IN_CREATE | libc::IN_MOVED_TO)) != 0);
    assert!(
        hit.is_some(),
        "原文件路径上没有 IN_CREATE/IN_MOVED_TO —— 假设不成立，见上方事件序列"
    );

    // 端到端：把这个事件喂给 classify（无抑制）——必须判成 NewFile，风险才算成立
    let reg = SelfWriteRegistry::new(DEFAULT_TTL);
    let decisions: Vec<WatchDecision> = evs
        .iter()
        .filter_map(|e| {
            watch_kind_from_mask(e.mask).map(|k| classify(&dir.path().join(&e.name), k, &reg))
        })
        .collect();
    println!("POC 无抑制时的 classify 结果 : {decisions:?}");
    assert!(
        decisions.contains(&WatchDecision::NewFile),
        "观测到了 Create 语义事件，但 classify 没判成 NewFile —— 见上方打印"
    );
}

/// 【第 4 部分】跑一批原子写回，返回 (事件, classify 结果)
fn run_batch(suppressed: bool) -> (TempDir, PathBuf, Vec<(RawEvent, WatchDecision)>) {
    let dir = TempDir::new(if suppressed { "batch-on" } else { "batch-off" });
    let file = dir.path().join("song.mp3");
    std::fs::write(&file, fake_audio(0)).expect("准备假音频");
    let reg = SelfWriteRegistry::new(DEFAULT_TTL);

    let mut w = InotifyCollector::start(dir.path()).expect("inotify 初始化失败");
    std::thread::sleep(Duration::from_millis(20));

    for i in 0..BATCH {
        if suppressed {
            reg.note_write(&file); // 写回**之前**登记
        }
        crate::tag::write::atomic::atomic_replace(&file, fake_audio(i + 1), None).expect("原子写回");
    }

    let evs = w.settle(Duration::from_millis(80), Duration::from_millis(3000));
    w.stop();

    let rows = evs
        .iter()
        .filter_map(|e| {
            watch_kind_from_mask(e.mask).map(|k| {
                let d = classify(&dir.path().join(&e.name), k, &reg);
                (e.clone(), d)
            })
        })
        .collect();
    (dir, file, rows)
}

/// 带抑制：50 次写回 → 不允许出现任何 NewFile
#[test]
fn suppressed_batch_never_yields_new_file() {
    let (dir, file, rows) = run_batch(true);
    print_sequence("带抑制：50 次 atomic_replace（仅列目标路径事件）", dir.path(), &file,
        &rows.iter().map(|(e, _)| e.clone()).collect::<Vec<_>>());
    print_decisions("带抑制：50 次原子写回", &rows);

    assert!(!rows.is_empty(), "没收到任何事件 —— 测试无效");
    let new_files: Vec<&(RawEvent, WatchDecision)> =
        rows.iter().filter(|(_, d)| *d == WatchDecision::NewFile).collect();
    assert!(
        new_files.is_empty(),
        "带抑制仍被判成 NewFile {} 次：{:?}",
        new_files.len(),
        new_files.iter().map(|(e, _)| (e.mask, e.name.clone())).collect::<Vec<_>>()
    );
    // 抑制必须真的命中过目标路径（否则「没判成 NewFile」可能只是因为没收到事件）
    assert!(
        rows.iter().any(|(e, d)| e.name == "song.mp3" && *d == WatchDecision::Ignore(IgnoreReason::SelfWrite)),
        "目标路径上没有任何 SelfWrite 判定 —— 抑制没生效，见上方打印"
    );
    // 中间 tmp 文件必须全部被 OurTmpFile 吃掉
    assert!(
        rows.iter().any(|(_, d)| *d == WatchDecision::Ignore(IgnoreReason::OurTmpFile)),
        "没有观察到 tmp 文件事件 —— 见上方打印"
    );
}

/// 对照组：关掉抑制（不调 note_write）→ 必须出现 NewFile
#[test]
fn unsuppressed_batch_does_yield_new_file() {
    let (dir, file, rows) = run_batch(false);
    print_sequence("对照组（无抑制）：50 次 atomic_replace", dir.path(), &file,
        &rows.iter().map(|(e, _)| e.clone()).collect::<Vec<_>>());
    print_decisions("对照组（无抑制）：50 次原子写回", &rows);

    let n_new = rows.iter().filter(|(_, d)| *d == WatchDecision::NewFile).count();
    assert!(
        n_new > 0,
        "对照组没有出现 NewFile —— 要么内核行为不同，要么抑制是多余的；见上方打印"
    );
}
