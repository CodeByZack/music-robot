//! src/watcher/watch.rs — 曲库文件监听主循环（裸 libc inotify，无任何第三方封装）
//!
//! ## 定位
//!
//! `inotify_poc` 只回答了「内核到底报什么」；本模块把它做成能长期运行的 `poll` 循环：
//! 多根 / 递归 / 防抖 / 抑制 / 确定性 cookie 配对 / 队列溢出上报，一个都不少。
//!
//! ## 事件流水线（顺序即语义）
//!
//! ```text
//! read(2) → parse_events → ingest(每条原始事件)
//!                              ├─ IN_Q_OVERFLOW      → 清空状态 → Overflow（调用方必须全量重扫）
//!                              ├─ IN_IGNORED         → 摘掉 wd 映射
//!                              ├─ IN_ISDIR + CREATE  → 动态补加 watch（不产生 WatchEvent）
//!                              ├─ cookie 配对        → tmp 的 MOVED_FROM 记 cookie，同 cookie 的 MOVED_TO 吃掉
//!                              ├─ watch_kind_from_mask → classify()（tmp 名 / 自写注册表 / 音频扩展名）
//!                              └─ 存活事件进防抖表（同路径合并，Created > Removed > Modified）
//! poll() ← flush 已到期的合并结果
//! ```
//!
//! ## 几个刻意选择（偏离「唯一显然写法」的地方，都有理由）
//!
//! · **防抖窗口从「该路径本批第一条事件」起算**，不是「最后一条之后静默 N 毫秒」。
//!   持续写入不会把事件无限期推迟 —— 监听器必须给出**有界延迟**（见 `Pending`）。
//! · **cookie 表有界**（`COOKIE_CAP` / `COOKIE_TTL`）：rename 的 `MOVED_FROM` / `MOVED_TO`
//!   在同一批里成对出现，配对窗口只需毫秒级。队列溢出会拆散配对，所以它**不是唯一防线** ——
//!   自写注册表仍在 `classify` 里兜底。
//! · **目录永不进 `WatchEvent`**：目录只用来补 watch，避免「专辑.mp3 是个目录」被当成音频。
//! · 生产路径零 `unwrap` / `expect` / `panic`：所有失败都变成 `WatchError` 往外传。

use super::classify::{classify, watch_kind_from_mask, WatchDecision};
use super::suppress::{is_our_tmp_file, SelfWriteRegistry};

use std::collections::{HashMap, VecDeque};
use std::ffi::{CString, OsString};
use std::fmt;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// inotify 读缓冲大小。内核按 `read(2)` 交付整批事件；16 KiB 是常见上限，
/// 单次 read 拿不完就靠非阻塞 fd 循环再读。
const READ_BUF: usize = 16 * 1024;

/// cookie 配对表容量上限：超出就丢最旧的一条（表必须有界，不能随 rename 次数增长）。
const COOKIE_CAP: usize = 64;

/// cookie 条目寿命。`MOVED_FROM` / `MOVED_TO` 是同一次 rename 的两个事件，
/// 正常情况下同批到达；给 5s 是为了容忍「两次 read 之间被调度走」的极端情况。
const COOKIE_TTL: Duration = Duration::from_secs(5);

// ───────────────────────── 配置 ─────────────────────────

/// 监听配置。
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// 同路径事件合并窗口，默认 500ms。
    pub debounce: Duration,
    /// 每个根递归监听（根下每个子目录都加 watch，新建目录动态补加）。
    pub recursive: bool,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self { debounce: Duration::from_millis(500), recursive: true }
    }
}

// ───────────────────────── 对外类型 ─────────────────────────

/// 一次 `poll` 的结果。**三种情况语义完全不同，调用方必须分开处理。**
#[derive(Debug, PartialEq, Eq)]
pub enum WatchPoll {
    /// 已分类、已去重、已过滤的事件
    Events(Vec<WatchEvent>),
    /// timeout 内没有「已就绪」的事件。
    ///
    /// 注意：可能仍有事件正在防抖窗口里排队，窗口到期后下一次 `poll` 会交付它们
    /// （`poll` 内部把等待上限压到「最近一个窗口到期时刻」，所以不会饿死）。
    Timeout,
    /// **inotify 队列溢出** —— 内核明确告诉我们「有事件丢了」。
    /// 调用方必须触发一次全量重扫，**绝不能当作「没有变化」**。
    Overflow,
}

/// 一条已经过三道闸门的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    NewFile(PathBuf),
    Removed(PathBuf),
    MetadataChanged(PathBuf),
}

impl WatchEvent {
    /// 受影响文件的路径。
    pub fn path(&self) -> &Path {
        match self {
            WatchEvent::NewFile(p) | WatchEvent::Removed(p) | WatchEvent::MetadataChanged(p) => p,
        }
    }
}

/// 监听器错误。
#[derive(Debug)]
pub enum WatchError {
    Init(std::io::Error),
    Watch { path: PathBuf, source: std::io::Error },
    Read(std::io::Error),
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WatchError::Init(e) => write!(f, "inotify 初始化失败：{e}"),
            WatchError::Watch { path, source } => {
                write!(f, "添加监听失败（{}）：{source}", path.display())
            }
            WatchError::Read(e) => write!(f, "读取 inotify 事件失败：{e}"),
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WatchError::Init(e) | WatchError::Read(e) => Some(e),
            WatchError::Watch { source, .. } => Some(source),
        }
    }
}

// ───────────────────────── 内部状态 ─────────────────────────

/// 一条原始 inotify 事件（`libc::inotify_event` 的四个字段 + 名字，用 `OsString` 保真）。
#[derive(Debug, Clone)]
struct RawEvent {
    wd: i32,
    mask: u32,
    cookie: u32,
    name: OsString,
}

/// 防抖合并时的语义强弱：Created > Removed > Modified（与 `watch_kind_from_mask` 同序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    Modified = 0,
    Removed = 1,
    NewFile = 2,
}

struct PendingEntry {
    rank: Rank,
    /// 本批第一条事件到达的时刻（窗口起点）
    since: Instant,
}

/// 同路径事件合并表。
#[derive(Default)]
struct Pending {
    map: HashMap<PathBuf, PendingEntry>,
}

impl Pending {
    /// 记一条事件；同路径只保留最强 rank，窗口起点不变（有界延迟）。
    fn merge(&mut self, path: PathBuf, rank: Rank, now: Instant) {
        match self.map.get_mut(&path) {
            Some(e) => {
                if rank > e.rank {
                    e.rank = rank;
                }
            }
            None => {
                self.map.insert(path, PendingEntry { rank, since: now });
            }
        }
    }

    /// 取出所有窗口已到期的事件（并移除）。按路径排序，输出确定。
    fn flush_due(&mut self, window: Duration, now: Instant) -> Vec<WatchEvent> {
        let mut ready: Vec<(PathBuf, Rank)> = Vec::new();
        self.map.retain(|path, e| {
            if now.saturating_duration_since(e.since) >= window {
                ready.push((path.clone(), e.rank));
                false
            } else {
                true
            }
        });
        ready.sort_by(|a, b| a.0.cmp(&b.0));
        ready
            .into_iter()
            .map(|(p, r)| match r {
                Rank::NewFile => WatchEvent::NewFile(p),
                Rank::Removed => WatchEvent::Removed(p),
                Rank::Modified => WatchEvent::MetadataChanged(p),
            })
            .collect()
    }

    /// 最近一个窗口到期时刻；表空返回 `None`。`poll` 用它把等待切片，避免拖到 timeout。
    fn next_due(&self, window: Duration) -> Option<Instant> {
        self.map.values().map(|e| e.since + window).min()
    }

    fn clear(&mut self) {
        self.map.clear();
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// cookie 配对表（有界 FIFO）。
///
/// `atomic_replace` 的 rename 会在同一批里给出 `IN_MOVED_FROM(tmp)` 与
/// `IN_MOVED_TO(原文名)`，两者 cookie 相同且非 0。记下前者、吃掉后者，
/// 就能**不依赖 TTL** 地确定性忽略一次自写 —— 即使注册表没登记（见测试第 7 条）。
///
/// 用定长 FIFO 环而不是 `HashMap` + 按时间淘汰：同一纳秒内的多条 cookie 时间戳相同，
/// 「最旧」在哈希序里是不确定的。FIFO 的淘汰是确定的，且容量只有 64，线性查找足够便宜。
#[derive(Default)]
struct CookieTracker {
    /// 按登记顺序排列（时间单调不减），front 恒为最旧
    ring: VecDeque<(u32, Instant)>,
}

impl CookieTracker {
    fn record(&mut self, cookie: u32, now: Instant) {
        self.prune(now);
        while self.ring.len() >= COOKIE_CAP {
            self.ring.pop_front();
        }
        self.ring.push_back((cookie, now));
    }

    /// 命中（说明这是一次「tmp → 原路径」的 rename）则吃掉该 cookie 并返回 true。
    fn consume(&mut self, cookie: u32, now: Instant) -> bool {
        self.prune(now);
        match self.ring.iter().position(|(c, _)| *c == cookie) {
            Some(i) => {
                self.ring.remove(i);
                true
            }
            None => false,
        }
    }

    fn prune(&mut self, now: Instant) {
        while let Some((_, t)) = self.ring.front() {
            if now.saturating_duration_since(*t) >= COOKIE_TTL {
                self.ring.pop_front();
            } else {
                break;
            }
        }
    }

    fn clear(&mut self) {
        self.ring.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.ring.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

/// 单条原始事件的摄取结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ingest {
    /// 已消化（进防抖表 / 被忽略 / 只用于补 watch）
    Consumed,
    /// 队列溢出：内部状态已清空，**调用方必须全量重扫**
    Overflow,
}

/// 监听掩码：覆盖规格要求的全部七类（与 `inotify_poc::watch_mask` 一致）。
fn watch_mask() -> u32 {
    libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_CLOSE_WRITE
        | libc::IN_MODIFY
        | libc::IN_ATTRIB
}

/// 把 `read(2)` 的字节流解析成事件（`read_unaligned`，不假设对齐；名字用 `OsString` 保真）。
fn parse_events(buf: &[u8], out: &mut Vec<RawEvent>) {
    let hdr = std::mem::size_of::<libc::inotify_event>();
    let mut off = 0usize;
    while off + hdr <= buf.len() {
        // SAFETY: off + hdr <= buf.len()，read_unaligned 允许任意对齐
        let ev: libc::inotify_event =
            unsafe { std::ptr::read_unaligned(buf[off..].as_ptr() as *const libc::inotify_event) };
        let start = off + hdr;
        let end = start + ev.len as usize;
        if end > buf.len() {
            break; // 截断的尾巴：丢弃，不猜
        }
        let name_bytes = buf[start..end].split(|b| *b == 0).next().unwrap_or(&[]);
        out.push(RawEvent {
            wd: ev.wd,
            mask: ev.mask,
            cookie: ev.cookie,
            name: OsString::from_vec(name_bytes.to_vec()),
        });
        off = end;
    }
}

// ───────────────────────── 监听器 ─────────────────────────

/// 曲库目录监听器：多根、可递归、带防抖与自写抑制。
///
/// `new` 之后由调用方在循环里 `poll`；`stop` 之后 `poll` 一律返回 `Timeout`（不 panic）。
pub struct LibraryWatcher {
    fd: i32,
    cfg: WatchConfig,
    registry: Arc<SelfWriteRegistry>,
    /// wd → 被监听的目录。IN_IGNORED 时摘除。
    wd_to_path: HashMap<i32, PathBuf>,
    pending: Pending,
    cookies: CookieTracker,
    /// 被 cookie 规则确定性吃掉的事件数（诊断用，见模块文档）。
    cookie_ignored: u64,
    read_buf: Vec<u8>,
    stopped: AtomicBool,
}

impl LibraryWatcher {
    /// 建立监听。`registry` 由调用方创建并共享 —— 写回方在 `atomic_replace` **之前**调 `note_write`。
    ///
    /// 任一根不存在 / 不可监听都返回 `Err`，**绝不静默成功**（静默成功意味着漏事件）。
    pub fn new(
        roots: &[PathBuf],
        cfg: WatchConfig,
        registry: Arc<SelfWriteRegistry>,
    ) -> Result<Self, WatchError> {
        if roots.is_empty() {
            return Err(WatchError::Init(io::Error::new(
                io::ErrorKind::InvalidInput,
                "至少需要一个监听根目录",
            )));
        }
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return Err(WatchError::Init(io::Error::last_os_error()));
        }
        let mut w = Self {
            fd,
            cfg,
            registry,
            wd_to_path: HashMap::new(),
            pending: Pending::default(),
            cookies: CookieTracker::default(),
            cookie_ignored: 0,
            read_buf: vec![0u8; READ_BUF],
            stopped: AtomicBool::new(false),
        };
        // 失败时 w 被 drop → Drop 里 close(fd)，不会泄漏 fd
        for root in roots {
            if w.cfg.recursive {
                w.add_tree_watch(root, false)?;
            } else {
                w.add_dir_watch(root, false)?;
            }
        }
        Ok(w)
    }

    /// 非阻塞轮询：收集并分类当前可用的事件，应用防抖与抑制。
    ///
    /// · 有到期事件 → `Events`
    /// · `timeout` 内既没有 fd 可读、也没有窗口到期 → `Timeout`
    /// · 内核报告队列溢出 → `Overflow`（状态已清空）
    pub fn poll(&mut self, timeout: Duration) -> Result<WatchPoll, WatchError> {
        if self.stopped.load(Ordering::SeqCst) {
            return Ok(WatchPoll::Timeout);
        }
        let deadline = Instant::now() + timeout;
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Ok(WatchPoll::Timeout);
            }
            // 1) 先把到期的防抖结果交出去
            let now = Instant::now();
            let ready = self.pending.flush_due(self.cfg.debounce, now);
            if !ready.is_empty() {
                return Ok(WatchPoll::Events(ready));
            }
            // 2) 决定这次等多久：不超过 deadline，也不超过最近一个窗口到期时刻
            let now = Instant::now();
            if now >= deadline {
                return Ok(WatchPoll::Timeout);
            }
            let next_due = self.pending.next_due(self.cfg.debounce);
            let wait_until = match next_due {
                Some(d) if d < deadline => d,
                _ => deadline,
            };
            let wait = wait_until.saturating_duration_since(now);
            // 3) 等 fd 可读
            if self.wait_readable(wait)? {
                let now = Instant::now();
                if self.drain(now)? == Ingest::Overflow {
                    // 安全关键：溢出必须上报，绝不当成「没变化」
                    return Ok(WatchPoll::Overflow);
                }
                // 回到顶部：本批里可能有窗口已到期的事件
            } else if next_due.is_none() {
                // 没有待交付的防抖条目，fd 也没动静 → 真的空闲
                return Ok(WatchPoll::Timeout);
            }
        }
    }

    /// 停止监听。之后 `poll` 一律返回 `Timeout`（fd 到 `Drop` 才关，保证不会用已关闭的 fd）。
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// 被 cookie 配对确定性忽略的事件数（诊断）。
    pub fn cookie_ignored_events(&self) -> u64 {
        self.cookie_ignored
    }

    /// 往 inotify fd 上等可读；超时返回 false。EINTR 不当作错误（交给外层重算剩余时间）。
    fn wait_readable(&self, timeout: Duration) -> Result<bool, WatchError> {
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let mut pfd = libc::pollfd { fd: self.fd, events: libc::POLLIN, revents: 0 };
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r > 0 {
            return Ok(true);
        }
        if r == 0 {
            return Ok(false);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            return Ok(false);
        }
        Err(WatchError::Read(err))
    }

    /// 把 fd 上当前可读的事件全部读空并摄取。返回 `Overflow` 表示内核报过队列溢出。
    fn drain(&mut self, now: Instant) -> Result<Ingest, WatchError> {
        let mut raw: Vec<RawEvent> = Vec::new();
        loop {
            // SAFETY: fd 有效（Drop 前不关），buf 独占且长度正确
            let n = unsafe {
                libc::read(
                    self.fd,
                    self.read_buf.as_mut_ptr() as *mut libc::c_void,
                    self.read_buf.len(),
                )
            };
            if n > 0 {
                raw.clear();
                parse_events(&self.read_buf[..n as usize], &mut raw);
                for ev in &raw {
                    if self.ingest(ev, now)? == Ingest::Overflow {
                        return Ok(Ingest::Overflow);
                    }
                }
                continue;
            }
            if n == 0 {
                // 非阻塞 fd 上不该出现；当作读空处理，不 panic
                return Ok(Ingest::Consumed);
            }
            let err = io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EAGAIN) => return Ok(Ingest::Consumed),
                Some(libc::EINTR) => continue,
                _ => return Err(WatchError::Read(err)),
            }
        }
    }

    /// 摄取**一条**原始事件。溢出分支在这里、且只在这里处理（可单测）。
    fn ingest(&mut self, ev: &RawEvent, now: Instant) -> Result<Ingest, WatchError> {
        // 0) 队列溢出：状态不可信了，清空后交给调用方全量重扫
        if ev.mask & libc::IN_Q_OVERFLOW != 0 {
            self.pending.clear();
            self.cookies.clear();
            return Ok(Ingest::Overflow);
        }
        // 1) watch 被移除（目录被删 / 被移走）：摘掉映射，不产生事件
        if ev.mask & libc::IN_IGNORED != 0 {
            self.wd_to_path.remove(&ev.wd);
            return Ok(Ingest::Consumed);
        }
        // 2) 自身事件（DELETE_SELF / MOVE_SELF）名字为空，无路径可言
        if ev.name.is_empty() {
            return Ok(Ingest::Consumed);
        }
        let dir = match self.wd_to_path.get(&ev.wd) {
            Some(d) => d.clone(),
            None => return Ok(Ingest::Consumed), // 已被摘除的 wd：丢弃
        };
        let path = dir.join(&ev.name);

        // 3) 目录：只用于动态补 watch，绝不进 WatchEvent
        if ev.mask & libc::IN_ISDIR != 0 {
            if self.cfg.recursive && ev.mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                // 宽容 ENOENT：目录可能在事件与 add_watch 之间就被删了，这是正常竞态
                self.add_tree_watch(&path, true)?;
            }
            return Ok(Ingest::Consumed);
        }

        // 4) cookie 配对：tmp 名的 MOVED_FROM 记 cookie；同 cookie 的 MOVED_TO 确定性忽略
        if ev.cookie != 0 {
            if ev.mask & libc::IN_MOVED_FROM != 0 && is_our_tmp_file(&path) {
                self.cookies.record(ev.cookie, now);
            } else if ev.mask & libc::IN_MOVED_TO != 0 && self.cookies.consume(ev.cookie, now) {
                self.cookie_ignored = self.cookie_ignored.saturating_add(1);
                return Ok(Ingest::Consumed);
            }
        }

        // 5) 位掩码 → 语义 kind
        let kind = match watch_kind_from_mask(ev.mask) {
            Some(k) => k,
            None => return Ok(Ingest::Consumed), // IN_OPEN 之类我们不关心的位
        };
        // 6) 三道闸门：tmp 名 → 自写注册表 → 音频扩展名
        let rank = match classify(&path, kind, &self.registry) {
            WatchDecision::Ignore(_) => return Ok(Ingest::Consumed),
            WatchDecision::NewFile => Rank::NewFile,
            WatchDecision::Removed => Rank::Removed,
            WatchDecision::MetadataChanged => Rank::Modified,
        };
        // 7) 进防抖表（同路径合并）
        self.pending.merge(path, rank, now);
        Ok(Ingest::Consumed)
    }

    /// 给一个目录挂 watch。`tolerate_not_found` 用于**动态补加**：目录可能已消失。
    fn add_dir_watch(&mut self, path: &Path, tolerate_not_found: bool) -> Result<(), WatchError> {
        let c = match CString::new(path.as_os_str().as_bytes()) {
            Ok(c) => c,
            Err(_) => {
                return Err(WatchError::Watch {
                    path: path.to_path_buf(),
                    source: io::Error::new(io::ErrorKind::InvalidInput, "路径包含 NUL 字节"),
                })
            }
        };
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), watch_mask()) };
        if wd < 0 {
            let source = io::Error::last_os_error();
            if tolerate_not_found && source.kind() == io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(WatchError::Watch { path: path.to_path_buf(), source });
        }
        self.wd_to_path.insert(wd, path.to_path_buf());
        Ok(())
    }

    /// 递归地给 `root` 及其下**每一个子目录**挂 watch（不跟随符号链接，防成环）。
    fn add_tree_watch(&mut self, root: &Path, tolerate_not_found: bool) -> Result<(), WatchError> {
        self.add_dir_watch(root, tolerate_not_found)?;
        let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if tolerate_not_found && e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(WatchError::Watch { path: dir, source: e }),
            };
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue, // 单个条目读失败不该拖垮整棵树
                };
                // file_type() 不跟随符号链接：符号链接目录不会被递归进去
                let ft = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if ft.is_dir() {
                    let child = entry.path();
                    self.add_dir_watch(&child, tolerate_not_found)?;
                    stack.push(child);
                }
            }
        }
        Ok(())
    }
}

impl Drop for LibraryWatcher {
    fn drop(&mut self) {
        if self.fd >= 0 {
            // SAFETY: fd 由 inotify_init1 返回，且只在这里关闭一次
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
    }
}

// ───────────────────────── 测试 ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::classify::WatchKind;
    use crate::watcher::suppress::DEFAULT_TTL;
    use crate::watcher::test_support::TempDir;

    fn cfg(debounce_ms: u64, recursive: bool) -> WatchConfig {
        WatchConfig { debounce: Duration::from_millis(debounce_ms), recursive }
    }

    fn registry() -> Arc<SelfWriteRegistry> {
        Arc::new(SelfWriteRegistry::new(DEFAULT_TTL))
    }

    fn watcher(roots: &[&Path], debounce_ms: u64, recursive: bool) -> LibraryWatcher {
        let paths: Vec<PathBuf> = roots.iter().map(|p| p.to_path_buf()).collect();
        LibraryWatcher::new(&paths, cfg(debounce_ms, recursive), registry()).expect("建立监听器失败")
    }

    /// 轮询到 `hit` 满足或超时；返回累积事件。绝不用长 sleep。
    fn poll_until(
        w: &mut LibraryWatcher,
        cap: Duration,
        mut hit: impl FnMut(&[WatchEvent]) -> bool,
    ) -> Vec<WatchEvent> {
        let t0 = Instant::now();
        let mut acc: Vec<WatchEvent> = Vec::new();
        while t0.elapsed() < cap {
            match w.poll(Duration::from_millis(50)) {
                Ok(WatchPoll::Events(mut e)) => {
                    acc.append(&mut e);
                    if hit(&acc) {
                        return acc;
                    }
                }
                Ok(WatchPoll::Timeout) => {}
                Ok(WatchPoll::Overflow) => panic!("测试里不该出现 inotify 队列溢出"),
                Err(e) => panic!("poll 出错：{e}"),
            }
        }
        acc
    }

    /// 静默观察一段时间（用于断言「没有更多事件」）。
    fn settle(w: &mut LibraryWatcher, dur: Duration) -> Vec<WatchEvent> {
        poll_until(w, dur, |_| false)
    }

    fn has_new(evs: &[WatchEvent], p: &Path) -> bool {
        evs.iter().any(|e| matches!(e, WatchEvent::NewFile(x) if x == p))
    }

    fn events_for<'a>(evs: &'a [WatchEvent], p: &Path) -> Vec<&'a WatchEvent> {
        evs.iter().filter(|e| e.path() == p).collect()
    }

    // ── 1. 多根 ────────────────────────────────────────────────

    #[test]
    fn two_roots_each_deliver_new_file() {
        let a = TempDir::new("watch-root-a");
        let b = TempDir::new("watch-root-b");
        let mut w = watcher(&[a.path(), b.path()], 30, true);

        let fa = a.path().join("a.mp3");
        let fb = b.path().join("b.flac");
        std::fs::write(&fa, b"x").expect("写 a");
        std::fs::write(&fb, b"x").expect("写 b");

        let evs = poll_until(&mut w, Duration::from_secs(3), |e| has_new(e, &fa) && has_new(e, &fb));
        assert!(has_new(&evs, &fa), "根 A 没收到 NewFile：{evs:?}");
        assert!(has_new(&evs, &fb), "根 B 没收到 NewFile：{evs:?}");
    }

    // ── 2. 递归（启动时已存在的子目录） ──────────────────────────

    #[test]
    fn recursive_subdir_delivers_new_file() {
        let root = TempDir::new("watch-recursive");
        let sub = root.path().join("album");
        std::fs::create_dir(&sub).expect("建子目录");
        let mut w = watcher(&[root.path()], 30, true);

        let f = sub.join("song.mp3");
        std::fs::write(&f, b"x").expect("写子目录文件");

        let evs = poll_until(&mut w, Duration::from_secs(3), |e| has_new(e, &f));
        assert!(has_new(&evs, &f), "子目录里的新文件没收到：{evs:?}");
    }

    // ── 3. 动态补加 watch（先建目录，再往里放文件） ──────────────

    #[test]
    fn dynamically_created_subdir_is_watched() {
        let root = TempDir::new("watch-dynamic");
        let mut w = watcher(&[root.path()], 30, true);

        let newdir = root.path().join("fresh-album");
        let sync = root.path().join("sync.mp3");
        std::fs::create_dir(&newdir).expect("建新目录");
        std::fs::write(&sync, b"x").expect("写同步文件");

        // 事件在队列里严格有序：看到 sync.mp3 的 NewFile，说明前面
        // 「新目录 CREATE」已被 ingest 处理完 —— 那一刻 watch 已经补上。
        let evs = poll_until(&mut w, Duration::from_secs(3), |e| has_new(e, &sync));
        assert!(has_new(&evs, &sync), "同步文件没收到，测试前提不成立：{evs:?}");

        let inner = newdir.join("inner.mp3");
        std::fs::write(&inner, b"x").expect("往新目录写文件");
        let evs2 = poll_until(&mut w, Duration::from_secs(3), |e| has_new(e, &inner));
        assert!(has_new(&evs2, &inner), "新建目录里的文件漏了（动态补 watch 失效）：{evs2:?}");
    }

    // ── 4. 非音频被忽略（以 sync 文件证明监听是活的） ────────────

    #[test]
    fn non_audio_file_is_ignored() {
        let root = TempDir::new("watch-nonaudio");
        let mut w = watcher(&[root.path()], 30, true);

        let txt = root.path().join("notes.txt");
        let sync = root.path().join("sync.mp3");
        std::fs::write(&txt, b"x").expect("写 txt");
        std::fs::write(&sync, b"x").expect("写同步文件");

        let evs = poll_until(&mut w, Duration::from_secs(3), |e| has_new(e, &sync));
        assert!(has_new(&evs, &sync), "监听没生效，结论无效：{evs:?}");
        assert!(events_for(&evs, &txt).is_empty(), ".txt 不该产生事件：{evs:?}");
    }

    // ── 5. tmp 文件被忽略 ──────────────────────────────────────

    #[test]
    fn tmp_file_is_ignored() {
        let root = TempDir::new("watch-tmp");
        let mut w = watcher(&[root.path()], 30, true);

        let tmp = root.path().join("x.mp3.music-robot-tmp-abc");
        let sync = root.path().join("sync.mp3");
        std::fs::write(&tmp, b"x").expect("写 tmp");
        std::fs::write(&sync, b"x").expect("写同步文件");

        let evs = poll_until(&mut w, Duration::from_secs(3), |e| has_new(e, &sync));
        assert!(has_new(&evs, &sync), "监听没生效，结论无效：{evs:?}");
        assert!(events_for(&evs, &tmp).is_empty(), "tmp 文件不该产生事件：{evs:?}");
    }

    // ── 6. 自写抑制：note_write + atomic_replace → 静默 ─────────

    #[test]
    fn self_write_via_atomic_replace_is_silent() {
        let root = TempDir::new("watch-selfwrite");
        let f = root.path().join("song.mp3");
        std::fs::write(&f, b"orig").expect("准备原文件");

        let reg = registry();
        let mut w =
            LibraryWatcher::new(&[root.path().to_path_buf()], cfg(30, true), Arc::clone(&reg))
                .expect("建立监听器");

        reg.note_write(&f); // 写回**之前**登记
        crate::tag::write::atomic::atomic_replace(&f, b"new bytes".to_vec(), None)
            .expect("原子写回");

        let sync = root.path().join("sync.mp3");
        std::fs::write(&sync, b"x").expect("写同步文件");
        let evs = poll_until(&mut w, Duration::from_secs(3), |e| has_new(e, &sync));
        assert!(has_new(&evs, &sync), "监听没生效，结论无效：{evs:?}");
        assert!(
            events_for(&evs, &f).is_empty(),
            "自写路径产生了事件（抑制失效）：{:?}",
            events_for(&evs, &f)
        );
    }

    // ── 7. cookie 配对（关掉注册表抑制） ────────────────────────

    #[test]
    fn atomic_replace_without_registry_is_eaten_by_cookie_pairing() {
        let root = TempDir::new("watch-cookie");
        let f = root.path().join("song.mp3");
        std::fs::write(&f, b"orig").expect("准备原文件");

        let reg = registry(); // 故意**不**调 note_write
        let mut w =
            LibraryWatcher::new(&[root.path().to_path_buf()], cfg(30, true), Arc::clone(&reg))
                .expect("建立监听器");

        crate::tag::write::atomic::atomic_replace(&f, b"cookie bytes".to_vec(), None)
            .expect("原子写回");

        // 证据 A：此刻 classify 对「原路径 + Created」的判定就是 NewFile ——
        // 注册表没登记、扩展名是音频，唯一能吃掉它的只有 cookie 规则。
        assert_eq!(
            classify(&f, WatchKind::Created, &reg),
            WatchDecision::NewFile,
            "前提不成立：classify 本该判成 NewFile"
        );

        let sync = root.path().join("sync.mp3");
        std::fs::write(&sync, b"x").expect("写同步文件");
        let evs = poll_until(&mut w, Duration::from_secs(3), |e| has_new(e, &sync));
        assert!(has_new(&evs, &sync), "监听没生效，结论无效：{evs:?}");

        // 证据 B：cookie 规则确实命中过（1 次 rename = 1 条 MOVED_TO 被吃）
        let hits = w.cookie_ignored_events();
        assert!(
            hits >= 1,
            "cookie 配对一次都没命中 —— 那「没有 NewFile」就另有原因，不能归功于 cookie"
        );
        // 证据 C：端到端结果 —— 原路径没有产生任何事件
        assert!(
            events_for(&evs, &f).is_empty(),
            "原子写回在原路径留下了事件：{:?}",
            events_for(&evs, &f)
        );
        println!("[cookie] 被 cookie 配对忽略的事件数 = {hits}");
    }

    // ── 8. 对照组：普通新建音频文件必须产生 NewFile ──────────────

    #[test]
    fn control_plain_new_audio_file_yields_new_file() {
        let root = TempDir::new("watch-control");
        let mut w = watcher(&[root.path()], 30, true);

        let f = root.path().join("brand-new.mp3");
        std::fs::write(&f, b"x").expect("写新文件");

        let evs = poll_until(&mut w, Duration::from_secs(3), |e| has_new(e, &f));
        assert!(
            has_new(&evs, &f),
            "对照组失败：没有抑制的普通新文件也没产生 NewFile —— 监听失效：{evs:?}"
        );
    }

    // ── 9. 删除 ────────────────────────────────────────────────

    #[test]
    fn deleted_audio_file_yields_removed() {
        let root = TempDir::new("watch-removed");
        let f = root.path().join("gone.flac");
        std::fs::write(&f, b"x").expect("准备文件");
        let mut w = watcher(&[root.path()], 30, true);

        std::fs::remove_file(&f).expect("删文件");
        let evs = poll_until(&mut w, Duration::from_secs(3), |e| {
            e.iter().any(|x| matches!(x, WatchEvent::Removed(p) if p == &f))
        });
        assert!(evs.contains(&WatchEvent::Removed(f.clone())), "删除没产生 Removed：{evs:?}");
    }

    // ── 10. 防抖：窗口内多次写 → 一个事件 ────────────────────────

    #[test]
    fn burst_writes_debounce_to_single_event() {
        let root = TempDir::new("watch-debounce");
        let mut w = watcher(&[root.path()], 80, true);

        let f = root.path().join("burst.mp3");
        for _ in 0..5 {
            std::fs::write(&f, b"x").expect("写文件");
        }
        let evs = poll_until(&mut w, Duration::from_secs(3), |e| !events_for(e, &f).is_empty());
        let got = events_for(&evs, &f);
        assert_eq!(got.len(), 1, "同一路径在窗口内的事件没有被合并：{evs:?}");

        // 窗口过去后再看一会儿：不该冒出第二个
        let more = settle(&mut w, Duration::from_millis(200));
        assert!(events_for(&more, &f).is_empty(), "防抖窗口后又冒出事件：{more:?}");
    }

    // ── 11. 空闲 → Timeout ─────────────────────────────────────

    #[test]
    fn idle_poll_returns_timeout() {
        let root = TempDir::new("watch-timeout");
        let mut w = watcher(&[root.path()], 30, true);

        let t0 = Instant::now();
        let r = w.poll(Duration::from_millis(100)).expect("poll");
        assert_eq!(r, WatchPoll::Timeout);
        assert!(t0.elapsed() >= Duration::from_millis(90), "poll 提前返回了：{:?}", t0.elapsed());
    }

    // ── 12. 不存在的根 → Err ───────────────────────────────────

    #[test]
    fn nonexistent_root_is_an_error() {
        let root = TempDir::new("watch-missing");
        let missing = root.path().join("no-such-dir");
        let r = LibraryWatcher::new(&[missing.clone()], cfg(30, true), registry());
        match r {
            Err(WatchError::Watch { path, .. }) => assert_eq!(path, missing),
            Err(other) => panic!("错误类型不对：{other:?}"),
            Ok(_) => panic!("不存在的根居然建立成功了（静默漏事件）"),
        }
        // 空 roots 也必须是错误，不能静默成功
        assert!(LibraryWatcher::new(&[], cfg(30, true), registry()).is_err());
    }

    // ── 13. stop() 之后 ────────────────────────────────────────

    #[test]
    fn after_stop_poll_returns_timeout_without_panic() {
        let root = TempDir::new("watch-stop");
        let mut w = watcher(&[root.path()], 30, true);
        w.stop();
        assert_eq!(w.poll(Duration::from_millis(100)).expect("stop 后 poll"), WatchPoll::Timeout);

        // 就算真的有文件变化，stop 之后也不该再交付
        std::fs::write(root.path().join("after-stop.mp3"), b"x").expect("写文件");
        assert_eq!(w.poll(Duration::from_millis(100)).expect("stop 后 poll"), WatchPoll::Timeout);
        w.stop(); // 幂等
    }

    // ── 14. 溢出路径（单测：IN_Q_OVERFLOW 在测试里造不出来） ──────

    #[test]
    fn overflow_event_clears_state_and_is_reported() {
        let root = TempDir::new("watch-overflow");
        let mut w = watcher(&[root.path()], 30, true);

        // 先塞点状态，证明溢出会把它们清干净
        w.pending.merge(root.path().join("seed.mp3"), Rank::NewFile, Instant::now());
        w.cookies.record(1234, Instant::now());
        assert!(!w.pending.is_empty());
        assert_eq!(w.cookies.len(), 1);

        let ev = RawEvent { wd: -1, mask: libc::IN_Q_OVERFLOW, cookie: 0, name: OsString::new() };
        assert_eq!(w.ingest(&ev, Instant::now()).expect("ingest"), Ingest::Overflow);
        assert!(w.pending.is_empty(), "溢出后防抖表必须清空");
        assert!(w.cookies.is_empty(), "溢出后 cookie 表必须清空（配对已被拆散）");

        // 溢出后仍能继续 ingest 正常事件（状态机没坏）
        let dir = root.path().to_path_buf();
        w.wd_to_path.insert(7, dir.clone());
        let f = dir.join("after-overflow.mp3");
        std::fs::write(&f, b"x").expect("写文件");
        let ok = RawEvent {
            wd: 7,
            mask: libc::IN_CREATE,
            cookie: 0,
            name: OsString::from("after-overflow.mp3"),
        };
        assert_eq!(w.ingest(&ok, Instant::now()).expect("ingest"), Ingest::Consumed);
        let flushed = w.pending.flush_due(Duration::ZERO, Instant::now());
        assert_eq!(flushed, vec![WatchEvent::NewFile(f)]);
    }

    // ── 15. 防抖合并保留最强语义（纯单测） ───────────────────────

    #[test]
    fn debounce_merge_keeps_strongest_rank() {
        let mut p = Pending::default();
        let t0 = Instant::now();
        let path = PathBuf::from("/m/song.mp3");

        p.merge(path.clone(), Rank::Modified, t0);
        p.merge(path.clone(), Rank::Removed, t0);
        p.merge(path.clone(), Rank::NewFile, t0);
        // 窗口内再来一条弱的，不能把强的顶下去
        p.merge(path.clone(), Rank::Modified, t0);

        assert!(p.flush_due(Duration::from_millis(50), t0).is_empty(), "窗口未到不该交付");
        let out = p.flush_due(Duration::from_millis(50), t0 + Duration::from_millis(60));
        assert_eq!(out, vec![WatchEvent::NewFile(path)], "Created > Removed > Modified 未生效");
        assert!(p.is_empty(), "交付后必须移除，否则同路径永远只有一个事件");
    }

    // ── 16. cookie 表有界 + 过期清理 ─────────────────────────────

    #[test]
    fn cookie_table_is_bounded_and_expires() {
        let mut c = CookieTracker::default();
        let t0 = Instant::now();
        for i in 1..=(COOKIE_CAP as u32 * 4) {
            c.record(i, t0);
        }
        assert!(c.len() <= COOKIE_CAP, "cookie 表必须是有界的：{}", c.len());
        // 最新的还在，最旧的已被挤掉
        assert!(c.consume(COOKIE_CAP as u32 * 4, t0), "最新 cookie 必须还留着");
        assert!(!c.consume(1, t0), "最旧 cookie 应已被挤掉");

        // 过期清理
        let mut c2 = CookieTracker::default();
        c2.record(42, t0);
        assert!(!c2.consume(42, t0 + COOKIE_TTL + Duration::from_millis(1)), "过期 cookie 不该再命中");
        assert!(c2.is_empty());
    }

    #[test]
    fn cookie_only_arms_for_our_tmp_names() {
        let root = TempDir::new("watch-cookie-name");
        let mut w = watcher(&[root.path()], 30, true);
        let dir = root.path().to_path_buf();
        w.wd_to_path.insert(3, dir.clone());

        // 外部把 song.mp3 改名成 other.mp3：MOVED_FROM 的名字不是 tmp，绝不记 cookie
        let ev =
            RawEvent { wd: 3, mask: libc::IN_MOVED_FROM, cookie: 999, name: OsString::from("song.mp3") };
        assert_eq!(w.ingest(&ev, Instant::now()).expect("ingest"), Ingest::Consumed);
        assert!(w.cookies.is_empty(), "非 tmp 名的 MOVED_FROM 不该武装 cookie");

        // tmp 名的 MOVED_FROM 才记
        let tmp_ev = RawEvent {
            wd: 3,
            mask: libc::IN_MOVED_FROM,
            cookie: 1000,
            name: OsString::from("song.mp3.music-robot-tmp-1-abc-000001"),
        };
        assert_eq!(w.ingest(&tmp_ev, Instant::now()).expect("ingest"), Ingest::Consumed);
        assert_eq!(w.cookies.len(), 1);

        // 中间清一次防抖表：上面那条 MOVED_FROM(song.mp3) 名字不是 tmp，本来就会
        // 按 Removed 正常进表（文件被移出曲库，这是对的行为），与本次断言无关。
        w.pending.clear();

        // 同 cookie 的 MOVED_TO 落到原路径：被吃掉，且不产生 NewFile
        let to_ev =
            RawEvent { wd: 3, mask: libc::IN_MOVED_TO, cookie: 1000, name: OsString::from("song.mp3") };
        assert_eq!(w.ingest(&to_ev, Instant::now()).expect("ingest"), Ingest::Consumed);
        assert_eq!(w.cookie_ignored_events(), 1, "只有 tmp 配对的 MOVED_TO 才该被吃");
        assert!(w.pending.is_empty(), "被 cookie 吃掉的 MOVED_TO 不该进防抖表（否则会入库）");
    }

    // ── 17. 配置默认值 ─────────────────────────────────────────

    #[test]
    fn config_defaults() {
        let d = WatchConfig::default();
        assert_eq!(d.debounce, Duration::from_millis(500));
        assert!(d.recursive);
    }
}
