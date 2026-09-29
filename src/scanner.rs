//! src/scanner.rs — 递归枚举 + 只读体检分级（移植自 src/scanner.ts）
//!
//! 语义规格：ARCHITECTURE §11（ok/warn/rejected/broken 四分级；每文件独立 try/catch，
//! 单文件不拖垮整目录；目录不可达 / 零命中由 CLI 层报 exit 1）
//!
//! 安全契约（本模块最重要的不变量）：
//!   `AudioWalker::new` / `scan_dir_iter` 在**根目录不可读**时返回 `Err`，
//!   **绝不**退化成空迭代器 —— 空结果会被上层当成「这些文件都消失了」，
//!   而 NAS 上曲库目录临时挂载闪断是常见故障，据此标记删除会**清空整个曲库**。
//!   任何「按磁盘消失标记删除」的决策，都必须先看 `WalkStats::may_prune()`。
use std::cell::Cell;
use std::fmt;
use std::fs::ReadDir;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::tag::read::{read_tags, warnings_for, probe_format, Format, Probe, AudioMetadata, Warning};

/// 分级结果（与 TS ScanEntry.level 一致）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level { Ok, Warn, Rejected, Broken }
impl Level { pub fn as_str(&self) -> &'static str {
    match self { Level::Ok => "ok", Level::Warn => "warn", Level::Rejected => "rejected", Level::Broken => "broken" } } }

#[derive(Debug, Clone)]
pub struct ScanEntry {
    pub path: String,
    pub level: Level,
    pub format: String,
    pub title: Option<String>,
    pub artists_summary: Option<String>,
    pub warnings: Vec<Warning>,
    pub error: Option<String>,
    pub frame_count: Option<usize>,
    /// read_json 视图（sanitize 过、含 mbid）；rejected/broken 无
    pub view: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct ScanReport {
    pub dir: String,
    pub entries: Vec<ScanEntry>,
    pub counts: Counts,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct Counts { pub ok: usize, pub warn: usize, pub rejected: usize, pub broken: usize, pub total: usize }

const AUDIO_EXT: &[&str] = &["mp3", "flac", "wav"];

/// 递归枚举目录下的 .mp3/.flac/.wav（大小写不敏感；路径排序稳定）
pub fn scan_files(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    if !dir.is_dir() {
        return Err(std::io::Error::other(format!("不是目录: {}", dir.display())));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    walk(dir, &mut out);
    out.sort();
    Ok(out)
}
fn walk(d: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, out); continue }
        let Some(ext) = p.extension().and_then(|s| s.to_str()).map(|s| s.to_lowercase()) else { continue };
        if AUDIO_EXT.contains(&ext.as_str()) { out.push(p) }
    }
}

/// 单文件只读体检（绝不抛：异常归入 rejected/broken）
pub fn inspect_file(path: &Path) -> ScanEntry {
    // 早先的写法是把 probe_format 的结果包成 Option，紧接着又立刻解开 ——
    // 恒为 Some 的死间接层，却凭空留了一个生产路径的解包点。
    // probe_format 恒返回 Probe，直接取引用即可。
    let format = format_name(&probe_format(path));
    // ⚠️ 分级按**错误变体**而不是错误文案：文案匹配脆弱（P2-4）。
    //   早期实现靠 `msg.contains("无法识别")` 判 rejected，但 `ReadError::Unrecognized`
    //   的文案里恰好有「无法识别」四个字，导致「FLAC 魔数成立但块序列损坏」被误判成
    //   rejected（TS 参照实现判 broken）。改为按变体映射：
    //     Id3PrefixedReal / Id3PrefixedUnknown → rejected（结构上明确拒绝的容器伪装）
    //     Unrecognized / Io / Escape           → broken（识别到格式但解析/IO 失败）
    use crate::tag::read::ReadError;
    enum Reader {
        Both(AudioMetadata, Vec<u8>),
        Rejected(String),
        Broken(String),
    }
    let reader: Reader = match read_tags(path) {
        Ok(meta) => match std::fs::read(path) {
            Ok(buf) => Reader::Both(meta, buf),
            Err(e) => Reader::Broken(format!("读取失败: {e}")),
        },
        Err(ReadError::Id3PrefixedReal(_) | ReadError::Id3PrefixedUnknown) => {
            Reader::Rejected("无法识别：ID3v2 前缀但容器不可解析（伪装文件或未知标签链）".into())
        }
        Err(e) => Reader::Broken(e.to_string()),
    };
    match reader {
        Reader::Both(meta, buf) => {
            let warnings = warnings_for(&meta, &buf);
            let view = Some(read_json_value_from_meta(&meta, path.display().to_string(), &warnings));
            ScanEntry {
                path: path.display().to_string(),
                level: if warnings.is_empty() { Level::Ok } else { Level::Warn },
                format,
                title: meta.title.clone(),
                artists_summary: (!meta.artists.is_empty()).then(|| meta.artists.join(" / ")),
                warnings,
                error: None,
                frame_count: Some(meta.raw_frames.len()),
                view,
            }
        }
        Reader::Rejected(msg) => ScanEntry {
            path: path.display().to_string(), level: Level::Rejected, format,
            title: None, artists_summary: None, warnings: vec![],
            error: Some(msg), frame_count: None, view: None,
        },
        Reader::Broken(msg) => ScanEntry {
            path: path.display().to_string(), level: Level::Broken, format,
            title: None, artists_summary: None, warnings: vec![],
            error: Some(msg), frame_count: None, view: None,
        },
    }
}

fn read_json_value_from_meta(meta: &AudioMetadata, file: String, warnings: &[Warning]) -> serde_json::Value {
    crate::tag::read::read_json_value(meta, &file, warnings)
}

fn format_name(p: &Probe) -> String {
    match p {
        Probe::Plain(Format::Mp3) => "mp3".into(),
        Probe::Plain(Format::Flac) => "flac".into(),
        Probe::Plain(Format::Wav) => "wav".into(),
        _ => "unknown".into(),
    }
}

/// 目录体检（纯函数）：逐文件 try/catch（单文件不拖垮整目录）
pub fn scan_dir(dir: &Path) -> Result<ScanReport, std::io::Error> {
    let files = scan_files(dir)?;
    let entries: Vec<ScanEntry> = files.iter().map(|p| inspect_file(p)).collect();
    let mut counts = Counts { total: entries.len(), ..Default::default() };
    for e in &entries {
        match e.level {
            Level::Ok => counts.ok += 1,
            Level::Warn => counts.warn += 1,
            Level::Rejected => counts.rejected += 1,
            Level::Broken => counts.broken += 1,
        }
    }
    Ok(ScanReport { dir: dir.display().to_string(), entries, counts })
}

// ─────────────────────────────────────────────────────────────────────────────
// 流式扫描：惰性 walker + 可信度信号
//
//   · 显式目录栈，不递归 —— 曲库目录可以很深，递归会耗调用栈；栈帧放堆上。
//   · 不预收集、不排序 —— 内存 O(目录深度)，与文件总数无关（10 万首也不多占内存）。
//   · 根目录打不开一律 Err，绝不静默返回空迭代器（见文件头「安全契约」）。
//   · 单线程迭代用 Rc<Cell<_>> 共享计数，不需要 Arc<Mutex>。
//     代价：AudioWalker / ScanIter 不是 Send；要跨线程就在目标线程里构造。
// ─────────────────────────────────────────────────────────────────────────────

/// 目录级扫描失败。与单文件的 `Level` 无关：
/// `Level` 描述「某个文件体检出来什么」，`ScanError` 描述「这次遍历根本没能可信地开始」。
#[derive(Debug)]
pub enum ScanError {
    /// 根路径不是目录（不存在 / 是普通文件 / 悬空符号链接）
    RootNotDir { path: PathBuf },
    /// 根目录存在但打不开（权限不足 / 挂载闪断 / IO 错误）
    RootUnreadable { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::RootNotDir { path } => write!(f, "扫描失败：不是目录 —— {}", path.display()),
            ScanError::RootUnreadable { path, source } => {
                write!(f, "扫描失败：目录不可读 —— {}（{source}）", path.display())
            }
        }
    }
}

impl std::error::Error for ScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScanError::RootNotDir { .. } => None,
            ScanError::RootUnreadable { source, .. } => Some(source),
        }
    }
}

/// 一次遍历的可信度信号（遍历过程中 / 结束后都能取快照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WalkStats {
    /// 根目录是否成功打开
    pub root_ok: bool,
    /// 已产出的音频文件数。
    /// ⚠️ 惰性语义：迭代未跑完时这是**部分值** —— 判断 `may_prune()` 前必须先把迭代器跑到底，
    /// 否则「刚开始迭代」会被误判成「零命中」（恰好又落进可疑里，不会误删，但语义要对）。
    pub files_seen: usize,
    /// 遍历中打不开的子目录数（看到的树因此是不完整的）
    pub dirs_read_failed: usize,
}

impl WalkStats {
    /// 这次扫描是否「可疑」——三种情况任一成立即可疑：
    ///   · 根目录没读成功
    ///   · 有子目录读失败（树是不完整的）
    ///   · `files_seen == 0`（可能是挂载闪断，也可能真空了，无法区分，一律当可疑）
    pub fn is_suspicious(&self) -> bool {
        !self.root_ok || self.dirs_read_failed > 0 || self.files_seen == 0
    }

    /// 只有**不可疑**的扫描才允许按「磁盘上消失了」标记删除。宁可漏删，不可误删。
    pub fn may_prune(&self) -> bool { !self.is_suspicious() }
}

/// 单线程共享计数器（`AudioWalker` 与 `ScanIter` 各持一份 `Rc`，指向同一组 Cell）。
#[derive(Debug, Default)]
struct WalkCounters {
    root_ok: Cell<bool>,
    files_seen: Cell<usize>,
    dirs_read_failed: Cell<usize>,
}

impl WalkCounters {
    fn bump(counter: &Cell<usize>) { counter.set(counter.get() + 1) }

    fn snapshot(&self) -> WalkStats {
        WalkStats {
            root_ok: self.root_ok.get(),
            files_seen: self.files_seen.get(),
            dirs_read_failed: self.dirs_read_failed.get(),
        }
    }
}

/// 惰性遍历曲库目录下的音频文件。不预收集、不排序，内存 O(目录深度)。
///
/// 契约：根目录不存在 / 不是目录 / 不可读 → `new` 返回 `Err`，
/// **绝不静默返回空迭代器** —— 这是本模块最重要的契约。
#[derive(Debug)]
pub struct AudioWalker {
    /// 显式目录栈（先序 DFS）：栈顶是当前正在读的那个目录句柄。不递归。
    stack: Vec<ReadDir>,
    counters: Rc<WalkCounters>,
}

impl AudioWalker {
    pub fn new(root: &Path) -> Result<Self, ScanError> {
        let counters = Rc::new(WalkCounters::default());
        // 1) 根必须是目录：不存在 / 普通文件 / 悬空链接都在此被拒
        //    （与 scan_files 的前置判断用同一个 is_dir()，语义保持一致）
        if !root.is_dir() {
            return Err(ScanError::RootNotDir { path: root.to_path_buf() });
        }
        // 2) 根必须真的能打开：失败即 Err。
        //    「无权限 / 挂载闪断 / IO 错误」绝不退化成空迭代器 —— 那正是清空曲库的事故路径。
        let rd = std::fs::read_dir(root)
            .map_err(|source| ScanError::RootUnreadable { path: root.to_path_buf(), source })?;
        counters.root_ok.set(true);
        Ok(Self { stack: vec![rd], counters })
    }

    /// 遍历过程中或结束后都可调用。
    pub fn stats(&self) -> WalkStats { self.counters.snapshot() }
}

impl Iterator for AudioWalker {
    type Item = PathBuf;

    fn next(&mut self) -> Option<PathBuf> {
        loop {
            // 先把栈顶目录的下一个条目取出所有权，避免与后面的 push/pop 借冲突。
            let entry = match self.stack.last_mut() {
                None => return None, // 栈空 = 整棵树走完
                Some(rd) => rd.next(),
            };
            match entry {
                None => { self.stack.pop(); } // 该目录读完，弹栈
                // 单个条目读失败：跳过（与旧 walk 的 rd.flatten() 语义一致，不计入统计）
                Some(Err(_)) => {}
                Some(Ok(e)) => {
                    let p = e.path();
                    if p.is_dir() {
                        // 子目录立刻打开 → 先序 DFS，与旧 walk 的递归顺序一致
                        match std::fs::read_dir(&p) {
                            Ok(sub) => self.stack.push(sub),
                            Err(_) => WalkCounters::bump(&self.counters.dirs_read_failed),
                        }
                        continue;
                    }
                    if is_audio_path(&p) {
                        WalkCounters::bump(&self.counters.files_seen);
                        return Some(p);
                    }
                }
            }
        }
    }
}

/// 扩展名规则：mp3 / flac / wav，大小写不敏感。
/// 与 `walk` 内联的判断逐字等价：`.mp3` 这种「名字本身就是扩展名」的隐藏文件
/// `Path::extension()` 返回 `None`，两边都跳过；`.hidden.mp3` 两边都算。
fn is_audio_path(p: &Path) -> bool {
    match p.extension().and_then(|s| s.to_str()) {
        Some(ext) => AUDIO_EXT.contains(&ext.to_lowercase().as_str()),
        None => false,
    }
}

/// 惰性扫描迭代器：包一个 `AudioWalker` + 共享统计句柄。
#[derive(Debug)]
pub struct ScanIter {
    walker: AudioWalker,
    counters: Rc<WalkCounters>,
}

impl ScanIter {
    /// 与 `AudioWalker::stats` 同源：迭代中 / 迭代后都能拿到可信度信号。
    /// 判 `may_prune()` 之前必须先把迭代器跑完（否则 `files_seen` 只是部分值）。
    pub fn stats(&self) -> WalkStats { self.counters.snapshot() }
}

impl Iterator for ScanIter {
    type Item = ScanEntry;

    fn next(&mut self) -> Option<ScanEntry> {
        let path = self.walker.next()?;
        Some(inspect_file(&path))
    }
}

/// 惰性扫描。根目录不可读 → `Err`（不是空迭代器）。
///
/// 服务端可用 spawn_blocking 包裹 + channel 转发（`ScanIter` 非 Send，需在目标线程内构造）。
/// 调用方在按「磁盘消失」标记删除前，必须先确认 `stats().may_prune()`。
pub fn scan_dir_iter(dir: &Path) -> Result<ScanIter, ScanError> {
    let walker = AudioWalker::new(dir)?;
    let counters = Rc::clone(&walker.counters);
    Ok(ScanIter { walker, counters })
}

/// 展示用相对路径
pub fn rel_path(p: &Path, dir: &Path) -> String {
    std::path::Path::strip_prefix(p, dir).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// 测试
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::test_support::TempDir;
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// 当前是否 root —— root 无视 DAC 位，chmod 000 拦不住它。
    /// 权限类断言必须据此分支：宁如实断言「能读」，也不伪造通过。
    fn is_root() -> bool {
        // SAFETY: geteuid 无参数、无副作用，永远成功。
        let uid = unsafe { libc::geteuid() };
        uid == 0
    }

    /// `chmod 000` 守卫：离开作用域（含断言 panic）一定把权限改回去，
    /// 否则 TempDir::drop 删不掉，/tmp 里会留下一堆删不掉的垃圾目录。
    struct DenyMode { path: PathBuf, original: u32 }

    impl DenyMode {
        fn new(path: &Path) -> Self {
            let original = fs::metadata(path)
                .map(|m| m.permissions().mode() & 0o7777)
                .unwrap_or(0o755);
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o000));
            Self { path: path.to_path_buf(), original }
        }
    }

    impl Drop for DenyMode {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(self.original));
        }
    }

    /// 建文件（自动建父目录）。内容为空：扫描层只认**路径**，不解析音频。
    fn touch(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        if let Some(parent) = p.parent() { fs::create_dir_all(parent).expect("建父目录"); }
        fs::write(&p, b"").expect("写测试文件");
        p
    }

    /// 相对路径集合：比对时忽略顺序。
    fn rel_set(root: &Path, paths: impl IntoIterator<Item = PathBuf>) -> BTreeSet<String> {
        paths.into_iter().map(|p| rel_path(&p, root)).collect()
    }

    // ── 1. 根不可读 → Err（核心安全契约）────────────────────────────────────

    #[test]
    fn root_missing_is_err_not_empty_iter() {
        let td = TempDir::new("scanner-root-missing");
        let missing = td.path().join("does-not-exist");
        match AudioWalker::new(&missing) {
            Err(ScanError::RootNotDir { path }) => assert_eq!(path, missing),
            other => panic!("路径不存在必须是 RootNotDir，实得 {other:?}"),
        }
        assert!(scan_dir_iter(&missing).is_err(), "根不存在必须 Err，绝不能是空迭代器");
    }

    #[test]
    fn root_is_file_is_err_not_empty_iter() {
        let td = TempDir::new("scanner-root-file");
        let f = touch(td.path(), "song.mp3");
        match AudioWalker::new(&f) {
            Err(ScanError::RootNotDir { path }) => assert_eq!(path, f),
            other => panic!("普通文件必须是 RootNotDir，实得 {other:?}"),
        }
        assert!(scan_dir_iter(&f).is_err(), "根是文件必须 Err，绝不能是空迭代器");
    }

    #[test]
    fn root_mode_000_is_err_not_empty_iter() {
        let td = TempDir::new("scanner-root-000");
        let _guard = DenyMode::new(td.path());
        if is_root() {
            eprintln!("[scanner::tests] 以 root 运行：chmod 000 仍可读，如实断言「可读」而非伪造 Err");
            let w = AudioWalker::new(td.path()).expect("root 无视 DAC，000 目录仍能打开");
            assert!(w.stats().root_ok);
            assert!(scan_dir_iter(td.path()).is_ok());
            return;
        }
        // 前置条件自检：确认 000 真的拦住了当前用户，否则这条测试没有意义
        assert!(fs::read_dir(td.path()).is_err(), "前置条件不成立：000 目录竟然可读");
        match AudioWalker::new(td.path()) {
            Err(ScanError::RootUnreadable { path, source }) => {
                assert_eq!(path, td.path());
                assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
            }
            other => panic!("000 目录必须是 RootUnreadable，实得 {other:?}"),
        }
        assert!(scan_dir_iter(td.path()).is_err(), "根不可读必须 Err —— 退化成空迭代器就是清空曲库的事故");
    }

    // ── 2. 与 scan_files 对拍 ───────────────────────────────────────────────

    #[test]
    fn walker_matches_scan_files_exactly() {
        let td = TempDir::new("scanner-parity");
        let root = td.path();
        touch(root, "a.mp3");
        touch(root, "b.FLAC");
        touch(root, "c.WaV");
        touch(root, "d.txt");
        touch(root, "noext");
        touch(root, ".mp3");        // 名字本身就是扩展名 → 无扩展名 → 两边都跳过
        touch(root, ".hidden.mp3"); // 隐藏但带扩展名 → 两边都算
        touch(root, "sub/e.MP3");
        touch(root, "sub/deep/f.flac");
        fs::create_dir_all(root.join("empty_dir")).expect("建空目录");

        let expected = scan_files(root).expect("scan_files");
        let got: Vec<PathBuf> = AudioWalker::new(root).expect("walker").collect();
        assert_eq!(rel_set(root, got), rel_set(root, expected), "AudioWalker 与 scan_files 的文件集合必须完全一致");

        assert_eq!(
            rel_set(root, scan_files(root).expect("scan_files")),
            BTreeSet::from([
                "a.mp3".to_string(), "b.FLAC".to_string(), "c.WaV".to_string(),
                ".hidden.mp3".to_string(), "sub/e.MP3".to_string(), "sub/deep/f.flac".to_string(),
            ]),
        );
    }

    // ── 3. 惰性证明 ─────────────────────────────────────────────────────────

    #[test]
    fn take_one_is_lazy_and_counts_seen() {
        let td = TempDir::new("scanner-lazy");
        for i in 0..200 { touch(td.path(), &format!("t{i:03}.mp3")); }

        let mut it = scan_dir_iter(td.path()).expect("根可读");
        assert!(it.next().is_some());
        assert_eq!(it.stats().files_seen, 1, "只取一个就不该遍历完 200 个 —— 惰性证明");
        assert!(it.stats().files_seen >= 1);
        drop(it); // 中途丢弃迭代器不得 panic
    }

    // ── 4. 扩展名过滤（大小写不敏感 / 非音频 / 隐藏文件）───────────────────

    #[test]
    fn extension_filter_is_case_insensitive_and_matches_scan_files() {
        let td = TempDir::new("scanner-ext");
        let root = td.path();
        touch(root, "a.mp3");
        touch(root, "b.MP3");
        touch(root, "c.flac");
        touch(root, "d.FlAc");
        touch(root, "e.wav");
        touch(root, "f.WAV");
        touch(root, "g.txt");
        touch(root, "h.mp4");
        touch(root, "noext");
        touch(root, "i.mp3.bak");
        touch(root, ".mp3");

        let expected = rel_set(root, scan_files(root).expect("scan_files"));
        let got = rel_set(root, AudioWalker::new(root).expect("walker").collect::<Vec<PathBuf>>());
        assert_eq!(got, expected, "扩展名规则必须与 scan_files 逐字一致");
        assert_eq!(
            got,
            BTreeSet::from([
                "a.mp3".to_string(), "b.MP3".to_string(), "c.flac".to_string(),
                "d.FlAc".to_string(), "e.wav".to_string(), "f.WAV".to_string(),
            ]),
            "只认 mp3/flac/wav（忽略大小写）；txt/无扩展名/.mp3 一律跳过",
        );
    }

    // ── 5. 子目录读失败 → 可疑 ─────────────────────────────────────────────

    #[test]
    fn unreadable_subdir_is_suspicious_and_not_prunable() {
        let td = TempDir::new("scanner-subdir-000");
        let root = td.path();
        touch(root, "ok.mp3");
        touch(root, "locked/inner.mp3");
        let _guard = DenyMode::new(&root.join("locked"));

        let mut it = scan_dir_iter(root).expect("根目录本身可读");
        let entries: Vec<ScanEntry> = it.by_ref().collect();
        let s = it.stats();

        assert!(s.root_ok);
        if is_root() {
            eprintln!("[scanner::tests] 以 root 运行：000 子目录仍可读，如实断言 dirs_read_failed == 0");
            assert_eq!(s.dirs_read_failed, 0);
            assert_eq!(entries.len(), 2);
            assert!(!s.is_suspicious());
            return;
        }
        assert!(s.dirs_read_failed >= 1, "locked/ 打不开应计入 dirs_read_failed，实得 {s:?}");
        assert_eq!(entries.len(), 1, "打不开的子目录里的文件不应被产出");
        assert_eq!(s.files_seen, 1);
        assert!(s.is_suspicious(), "树不完整 → 可疑");
        assert!(!s.may_prune(), "树不完整时绝不允许按「磁盘消失」标记删除");
    }

    // ── 6. 空目录：可读但零命中 → 仍然可疑 ────────────────────────────────

    #[test]
    fn empty_dir_is_suspicious_never_prunable() {
        let td = TempDir::new("scanner-empty");
        touch(td.path(), "readme.txt");

        let mut it = scan_dir_iter(td.path()).expect("空目录本身是可读的");
        assert!(it.next().is_none(), "空目录产出空迭代器是合法的");
        let s = it.stats();
        assert!(s.root_ok);
        assert_eq!(s.files_seen, 0);
        assert_eq!(s.dirs_read_failed, 0);
        assert!(s.is_suspicious(), "零命中一律可疑：挂载闪断与真空目录无法区分");
        assert!(!s.may_prune());
    }

    // ── 7. 干净扫描 → 允许 prune ───────────────────────────────────────────

    #[test]
    fn clean_scan_is_not_suspicious_and_may_prune() {
        let td = TempDir::new("scanner-clean");
        touch(td.path(), "a.mp3");
        touch(td.path(), "sub/b.flac");

        let mut it = scan_dir_iter(td.path()).expect("根可读");
        let n = it.by_ref().count();
        let s = it.stats();
        assert_eq!(n, 2);
        assert!(s.root_ok);
        assert_eq!(s.files_seen, 2);
        assert_eq!(s.dirs_read_failed, 0);
        assert!(!s.is_suspicious());
        assert!(s.may_prune(), "全部可读且有命中 → 允许按磁盘消失标记删除");
    }

    // ── 8. 深目录不爆栈 ───────────────────────────────────────────────────

    #[test]
    fn deep_tree_does_not_blow_the_stack() {
        let td = TempDir::new("scanner-deep");
        let mut dir = td.path().to_path_buf();
        for i in 0..50 { dir = dir.join(format!("d{i}")); }
        fs::create_dir_all(&dir).expect("建 50 层嵌套目录");
        let deepest = dir.join("song.mp3");
        fs::write(&deepest, b"").expect("写最深处文件");

        let files: Vec<PathBuf> = AudioWalker::new(td.path()).expect("walker").collect();
        assert_eq!(files, vec![deepest], "显式栈必须能走到 50 层深处");
        let s = AudioWalker::new(td.path()).expect("walker").stats();
        assert_eq!(s.files_seen, 0, "stats() 在未迭代时是初始快照");
        assert!(s.root_ok);
    }

    // ── 9. ScanError 的 Display / Error ────────────────────────────────────

    #[test]
    fn scan_error_display_is_chinese_and_contains_path() {
        let path = PathBuf::from("/vol1/curated/music");

        let not_dir = ScanError::RootNotDir { path: path.clone() };
        let s = not_dir.to_string();
        assert!(s.contains("/vol1/curated/music"), "Display 必须含路径：{s}");
        assert!(s.contains("目录"), "Display 必须是中文：{s}");
        assert!(s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)), "必须含汉字：{s}");
        assert!(std::error::Error::source(&not_dir).is_none());

        let unreadable = ScanError::RootUnreadable {
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "权限不足"),
        };
        let s2 = unreadable.to_string();
        assert!(s2.contains("/vol1/curated/music"), "Display 必须含路径：{s2}");
        assert!(s2.contains("权限不足"), "应当带上底层原因：{s2}");
        assert!(std::error::Error::source(&unreadable).is_some(), "RootUnreadable 必须暴露 source()");
    }

    #[test]
    fn default_stats_are_suspicious() {
        let s = WalkStats::default();
        assert!(!s.root_ok);
        assert_eq!((s.files_seen, s.dirs_read_failed), (0, 0));
        assert!(s.is_suspicious(), "没读过任何东西的默认值必须是可疑的");
        assert!(!s.may_prune());
    }
}
