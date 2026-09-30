//! S11 · 插件 worker 池 —— 同步实现的「借-用-还」进程池。
//!
//! 插件是 daemon 模式的长驻进程：一行请求进（stdin），一行响应出（stdout）。
//! 池子负责生命周期：懒加载、复用、超时打死、崩溃重建、背压、空闲回收。
//!
//! ## 给 async 调用方的说明
//!
//! 本模块**只有同步 API**（`std::process` + `std::sync` + 后台线程），不依赖 tokio。
//! 在 async 服务里不要直接 `await` 它——[`PoolGuard::call`] 会**阻塞当前线程**。
//! 正确姿势是把整段「借 → 调 → 还」丢进阻塞线程池：
//!
//! ~~~ignore
//! let out = tokio::task::spawn_blocking(move || {
//!     let mut guard = pool.acquire(Duration::from_secs(5))?;
//!     guard.call(&req)
//! }).await??;
//! ~~~
//!
//! 注意 `WorkerPool` 是 `Send + Sync`，但 [`PoolGuard`] 借了 `&WorkerPool`，
//! 不能跨 `await` 点存活——所以 `spawn_blocking` 的闭包必须把整段包进去，而不是只包 `call`。
//!
//! ## 几个关键行为
//!
//! * **懒加载**：[`WorkerPool::new`] 一个进程都不起；
//! * **复用**：空闲 worker 直接复用，不每次重启（daemon 协议要求）；
//! * **超时**：超过 `task_timeout` → [`PoolError::Timeout`]，并且 `killpg + wait`
//!   整组回收，不留僵尸、不留孤儿；
//! * **崩溃**：非正常退出 → [`PoolError::Crashed`]，该 worker 报废，下次 `acquire` 重建；
//! * **背压**：`live = idle + busy <= max`，满了就等，等不到（超过 `wait`）报
//!   [`PoolError::Busy`]，绝不会无限起进程；
//! * **空闲回收**：后台线程按 `idle_timeout / 4`（夹在 20ms..30s）轮询，超时的 idle worker
//!   会被 killpg 收掉。
//!
//! ## 已知取舍：复用 worker 的 cwd 是「首次 spawn 时的目录」
//!
//! 每次 `acquire` 都新建一个独立临时目录，`PoolGuard::drop` 时删掉。但复用的进程
//! 无法在外部改 cwd，所以**插件必须按请求里的 `work_dir` 落盘，不能依赖进程 cwd**——
//! 协议本来就把 `work_dir` 放在请求里（`ScrapeRequest.work_dir`），这正是原因之一。
//! 下载类请求的 `target_dir` 是产物目标目录，不属于池的管辖范围，池不代填。

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use super::manifest::PluginMeta;
use super::protocol::{decode_response, encode_request, PluginRequest, PluginResponse};
use super::sandbox::{self, SandboxConfig};

/// 空闲回收线程的轮询间隔上限
const REAPER_MAX_INTERVAL: Duration = Duration::from_secs(30);
/// 空闲回收线程的轮询间隔下限（防止 idle_timeout 很小时忙转）
const REAPER_MIN_INTERVAL: Duration = Duration::from_millis(20);
/// `PoolConfig::default()` 的 idle_timeout
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// 池的配置。
///
/// 三个字段都可以写 `0` 表示「跟着插件清单走」：`max` → `meta.max_concurrency`，
/// `task_timeout` → `meta.timeout_ms`，`idle_timeout` → [`DEFAULT_IDLE_TIMEOUT`]。
/// 直接用 [`PoolConfig::for_meta`] 更直白。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolConfig {
    /// 该插件的最大并发（同时存活的 worker 进程数）
    pub max: usize,
    /// 空闲多久后被后台线程回收（默认 5 分钟）
    pub idle_timeout: Duration,
    /// 单次 `call` 的墙钟超时（默认取 `meta.timeout_ms`）
    pub task_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        PoolConfig {
            max: 1,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            task_timeout: Duration::from_millis(super::manifest::DEFAULT_TIMEOUT_MS),
        }
    }
}

impl PoolConfig {
    /// 从插件清单取默认值：`max` = `meta.max_concurrency`，`task_timeout` = `meta.timeout_ms`
    pub fn for_meta(meta: &PluginMeta) -> Self {
        PoolConfig {
            max: meta.max_concurrency as usize,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            task_timeout: Duration::from_millis(meta.timeout_ms),
        }
    }
}

/// 池相关的错误。
#[derive(Debug)]
pub enum PoolError {
    /// 起不了进程（命令不存在 / 没权限 / pre_exec 里的沙箱调用失败）
    Spawn(std::io::Error),
    /// 超过 `task_timeout` 没等到一行响应；worker 已被 killpg 回收
    Timeout(Duration),
    /// 插件非正常退出（非零退出码或被信号杀死）
    Crashed { code: Option<i32>, signal: Option<i32> },
    /// 插件说了胡话：非 JSON、空行、提前 EOF、读不出 UTF-8
    Protocol(String),
    /// 池已满且等待超时
    Busy,
    /// 本地 IO 问题（建临时目录、读线程出错等）
    Io(std::io::Error),
    /// 池已经 shutdown，不再接受新的 acquire
    Shutdown,
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolError::Spawn(e) => write!(f, "启动插件进程失败：{e}"),
            PoolError::Timeout(d) => write!(f, "插件调用超时（{} ms）", d.as_millis()),
            PoolError::Crashed { code, signal } => {
                write!(f, "插件进程异常退出（exit={code:?} signal={signal:?}）")
            }
            PoolError::Protocol(m) => write!(f, "插件协议错误：{m}"),
            PoolError::Busy => write!(f, "插件池已满，等待超时"),
            PoolError::Io(e) => write!(f, "插件池 IO 错误：{e}"),
            PoolError::Shutdown => write!(f, "插件池已关闭"),
        }
    }
}

impl std::error::Error for PoolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PoolError::Spawn(e) | PoolError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for PoolError {
    fn from(e: std::io::Error) -> Self {
        PoolError::Io(e)
    }
}

/// 池的即时统计
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    /// 空闲待复用的 worker 数
    pub idle: usize,
    /// 已借出（含正在 spawn 的坑位）的 worker 数
    pub busy: usize,
    /// 该池累计 spawn 过的进程数
    pub spawned_total: u64,
}

/// 插件进程池。`Send + Sync`，可以放进 `Arc` 给多线程共享。
pub struct WorkerPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    meta: PluginMeta,
    sandbox: SandboxConfig,
    work_root: PathBuf,
    cfg: PoolConfig,
    state: Mutex<State>,
    cv: Condvar,
    spawned_total: AtomicU64,
    seq: AtomicU64,
    shutdown: AtomicBool,
}

#[derive(Default)]
struct State {
    idle: Vec<IdleWorker>,
    busy: usize,
}

/// 一个已经起好的插件进程 + 它 stdout 的读线程出口
struct IdleWorker {
    child: Child,
    stdin: ChildStdin,
    events: Receiver<ReadEvent>,
    last_used: Instant,
}

/// 读线程看到的东西
enum ReadEvent {
    /// 完整的一行（已去掉行尾换行）
    Line(String),
    /// stdout 关闭
    Eof,
    /// 读取失败（含非 UTF-8）
    Failed(String),
}

// ─────────────────────────── 对外 API ───────────────────────────

impl WorkerPool {
    /// 建池。**不 spawn 任何进程**（懒加载），只起一个空闲回收线程。
    ///
    /// `cfg` 里为 0 / ZERO 的字段会回退到 `meta` 的对应值，见 [`PoolConfig`]。
    pub fn new(
        meta: PluginMeta,
        sandbox: SandboxConfig,
        work_root: PathBuf,
        mut cfg: PoolConfig,
    ) -> Self {
        if cfg.max == 0 {
            cfg.max = (meta.max_concurrency as usize).max(1);
        }
        if cfg.task_timeout.is_zero() {
            cfg.task_timeout = Duration::from_millis(meta.timeout_ms.max(1));
        }
        if cfg.idle_timeout.is_zero() {
            cfg.idle_timeout = DEFAULT_IDLE_TIMEOUT;
        }
        let inner = Arc::new(PoolInner {
            meta,
            sandbox,
            work_root,
            cfg,
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            spawned_total: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
        });
        start_reaper(Arc::downgrade(&inner));
        WorkerPool { inner }
    }

    /// 实际生效的配置（含从 meta 回退出来的值）
    pub fn config(&self) -> &PoolConfig {
        &self.inner.cfg
    }

    /// 借一个 worker：有空闲就复用；没有且未达 `max` 就新起；否则最多等 `wait`。
    ///
    /// 超过 `wait` 还没借到 → [`PoolError::Busy`]（这就是背压）。
    /// 每次成功都会新建一个独立临时目录，`PoolGuard` 析构时删掉。
    pub fn acquire(&self, wait: Duration) -> Result<PoolGuard<'_>, PoolError> {
        let inner = self.inner.as_ref();
        let start = Instant::now();

        loop {
            if inner.shutdown.load(Ordering::SeqCst) {
                return Err(PoolError::Shutdown);
            }

            // 1) 优先复用空闲 worker
            if let Some(worker) = inner.take_idle() {
                return self.attach(worker);
            }

            // 2) 未达上限 → 新起一个（先占坑，防止并发线程一起超发）
            if inner.reserve_slot() {
                return self.spawn_and_attach();
            }

            // 3) 满了，等归还
            let elapsed = start.elapsed();
            if elapsed >= wait {
                return Err(PoolError::Busy);
            }
            let timeout = wait - elapsed;
            let guard = inner.lock_state();
            let (guard, _) = inner
                .cv
                .wait_timeout(guard, timeout)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(guard);
        }
    }

    /// 当前统计（idle / busy / 累计 spawn 次数）
    pub fn stats(&self) -> PoolStats {
        let st = self.inner.lock_state();
        PoolStats {
            idle: st.idle.len(),
            busy: st.busy,
            spawned_total: self.inner.spawned_total.load(Ordering::Relaxed),
        }
    }

    /// 关闭池：杀掉所有空闲 worker，并让后续 `acquire` 直接返回 [`PoolError::Shutdown`]。
    /// 已经借出去的 worker 会在 `PoolGuard::drop` 时被回收。幂等。
    pub fn shutdown(&self) {
        let inner = self.inner.as_ref();
        inner.shutdown.store(true, Ordering::SeqCst);
        let workers = {
            let mut st = inner.lock_state();
            std::mem::take(&mut st.idle)
        };
        inner.cv.notify_all();
        for mut w in workers {
            inner.retire(&mut w);
        }
    }

    /// 借到 worker 后：建一个本次调用专属的临时目录
    fn attach<'a>(&'a self, worker: IdleWorker) -> Result<PoolGuard<'a>, PoolError> {
        match self.inner.make_work_dir() {
            Ok(dir) => Ok(PoolGuard { pool: self, worker: Some(worker), work_dir: dir }),
            Err(e) => {
                // 目录都建不出来，就别占着 worker 了
                let mut worker = worker;
                self.inner.retire(&mut worker);
                self.inner.release_slot();
                Err(PoolError::Io(e))
            }
        }
    }

    /// 已经占好坑位：建目录 → spawn → 返回 guard
    fn spawn_and_attach(&self) -> Result<PoolGuard<'_>, PoolError> {
        let dir = match self.inner.make_work_dir() {
            Ok(d) => d,
            Err(e) => {
                self.inner.release_slot();
                return Err(PoolError::Io(e));
            }
        };
        match self.inner.spawn_worker(&dir) {
            Ok(worker) => {
                self.inner.spawned_total.fetch_add(1, Ordering::Relaxed);
                Ok(PoolGuard { pool: self, worker: Some(worker), work_dir: dir })
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                self.inner.release_slot();
                Err(e)
            }
        }
    }
}

impl Drop for PoolInner {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let workers = match self.state.get_mut() {
            Ok(st) => std::mem::take(&mut st.idle),
            Err(poisoned) => std::mem::take(&mut poisoned.into_inner().idle),
        };
        for mut w in workers {
            self.retire(&mut w);
        }
        self.cv.notify_all();
    }
}

// ─────────────────────────── 借出的 guard ───────────────────────────

/// 借出中的 worker。`Drop` 时把进程还回池子（或回收），并删掉本次的临时目录。
pub struct PoolGuard<'a> {
    pool: &'a WorkerPool,
    worker: Option<IdleWorker>,
    work_dir: PathBuf,
}

impl PoolGuard<'_> {
    /// 本次调用专属的可写目录（插件应当把中间产物写在这里）
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// 发一条请求、读一条响应（同步阻塞，最长 `task_timeout`）。
    ///
    /// 任何失败都会把这个 worker 判死刑：`killpg` 整组收掉再 `wait` 回收，不留僵尸。
    /// 失败后同一个 guard 再 `call` 会返回 [`PoolError::Crashed`]（进程已经没了），
    /// 想要继续用请重新 `acquire`，池会重建进程。
    pub fn call(&mut self, req: &PluginRequest) -> Result<PluginResponse, PoolError> {
        let mut worker = match self.worker.take() {
            Some(w) => w,
            None => return Err(PoolError::Crashed { code: None, signal: None }),
        };
        let timeout = self.pool.inner.cfg.task_timeout;
        match exchange(&mut worker, req, timeout) {
            Ok(resp) => {
                self.worker = Some(worker);
                Ok(resp)
            }
            Err(e) => {
                self.pool.inner.retire(&mut worker);
                self.pool.inner.release_slot();
                Err(e)
            }
        }
    }
}

impl Drop for PoolGuard<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.work_dir);
        let mut worker = match self.worker.take() {
            Some(w) => w,
            None => return,
        };
        let inner = self.pool.inner.as_ref();
        let mut st = inner.lock_state();
        st.busy = st.busy.saturating_sub(1);
        let reusable = !inner.shutdown.load(Ordering::SeqCst)
            && !worker.has_pending_event()
            && worker.is_healthy();
        if reusable {
            worker.last_used = Instant::now();
            st.idle.push(worker);
            drop(st);
            inner.cv.notify_all();
        } else {
            drop(st);
            inner.retire(&mut worker);
            inner.cv.notify_all();
        }
    }
}

// ─────────────────────────── 池内部实现 ───────────────────────────

impl PoolInner {
    fn lock_state(&self) -> MutexGuard<'_, State> {
        // 生产路径不允许 panic：锁被毒化只能说明别的线程 panic 过，
        // 池的数据结构本身没有「半更新」不变量，取回内部值继续用是安全的。
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 从空闲队列里取一个健康 worker（busy += 1）；顺手清掉已经死掉的。
    fn take_idle(&self) -> Option<IdleWorker> {
        let mut dead: Vec<IdleWorker> = Vec::new();
        let chosen = {
            let mut st = self.lock_state();
            let mut chosen = None;
            while let Some(mut worker) = st.idle.pop() {
                if worker.is_healthy() && !worker.has_pending_event() {
                    chosen = Some(worker);
                    break;
                }
                dead.push(worker);
            }
            if chosen.is_some() {
                st.busy += 1;
            }
            chosen
        };
        if !dead.is_empty() {
            // 腾出了坑位，唤醒等待者
            self.cv.notify_all();
            for mut w in dead {
                self.retire(&mut w);
            }
        }
        chosen
    }

    /// 锁内检查容量并占位（busy += 1）。返回 false = 满了
    fn reserve_slot(&self) -> bool {
        let mut st = self.lock_state();
        if st.idle.len() + st.busy < self.cfg.max {
            st.busy += 1;
            true
        } else {
            false
        }
    }

    /// 归还一个坑位（spawn 失败 / 建目录失败时用）
    fn release_slot(&self) {
        let mut st = self.lock_state();
        st.busy = st.busy.saturating_sub(1);
        drop(st);
        self.cv.notify_all();
    }

    /// 起一个插件进程，并给它挂一个 stdout 读线程
    fn spawn_worker(&self, work_dir: &Path) -> Result<IdleWorker, PoolError> {
        let program = self.meta.command.first().ok_or_else(|| {
            PoolError::Spawn(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "插件 command 为空",
            ))
        })?;
        let mut cmd = Command::new(program);
        cmd.args(self.meta.command.iter().skip(1));
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());
        cmd.current_dir(work_dir);
        sandbox::apply(&mut cmd, &self.sandbox)
            .map_err(|e| PoolError::Spawn(std::io::Error::other(e.to_string())))?;

        let mut child = cmd.spawn().map_err(PoolError::Spawn)?;
        let stdin = match child.stdin.take() {
            Some(s) => s,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PoolError::Spawn(std::io::Error::other("stdin 管道没建起来")));
            }
        };
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PoolError::Spawn(std::io::Error::other("stdout 管道没建起来")));
            }
        };
        let events = spawn_reader(stdout)?;
        Ok(IdleWorker { child, stdin, events, last_used: Instant::now() })
    }

    /// 本次 acquire 专属的临时目录（名字经白名单清洗，插件名里的 `../` 跑不出去）
    fn make_work_dir(&self) -> std::io::Result<PathBuf> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let name = sanitize_name(&self.meta.name);
        let dir = self.work_root.join(format!("{name}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// 把一个 worker 判死刑：killpg 整组 + 兜底 kill + wait 回收僵尸
    fn retire(&self, worker: &mut IdleWorker) {
        let _ = sandbox::kill_process_group(worker.child.id());
        let _ = worker.child.kill();
        let _ = worker.child.wait();
    }

    /// 后台回收：清掉空闲超时（或已死）的 worker。shutdown 时全清。
    fn reap_idle(&self) {
        let now = Instant::now();
        let timeout = self.cfg.idle_timeout;
        let shutting_down = self.shutdown.load(Ordering::SeqCst);
        let mut expired: Vec<IdleWorker> = Vec::new();
        {
            let mut st = self.lock_state();
            let idle = std::mem::take(&mut st.idle);
            let mut keep: Vec<IdleWorker> = Vec::with_capacity(idle.len());
            for mut worker in idle {
                let stale = shutting_down || now.saturating_duration_since(worker.last_used) >= timeout;
                if stale || !worker.is_healthy() || worker.has_pending_event() {
                    expired.push(worker);
                } else {
                    keep.push(worker);
                }
            }
            st.idle = keep;
        }
        if !expired.is_empty() {
            for mut w in expired {
                self.retire(&mut w);
            }
            self.cv.notify_all();
        }
    }
}

/// 起一个 detached 读线程，把 stdout 的一行行送进 channel。
/// 用 channel 而不是 `poll(2)`：既躲开 `BufReader` 缓冲与 `poll` 的竞争，
/// 又天然支持 `recv_timeout`，不需要给 fd 设非阻塞。
fn spawn_reader(stdout: ChildStdout) -> Result<Receiver<ReadEvent>, PoolError> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("plugin-pool-reader".to_string())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(0) => {
                        let _ = tx.send(ReadEvent::Eof);
                        return;
                    }
                    Ok(_) => {
                        let line = buf.trim_end_matches(&['\r', '\n'][..]).to_string();
                        if tx.send(ReadEvent::Line(line)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(ReadEvent::Failed(e.to_string()));
                        return;
                    }
                }
            }
        })
        .map_err(PoolError::Spawn)?;
    Ok(rx)
}

/// 后台空闲回收线程。只拿 `Weak`，池没了自己退出，不会把池吊住不释放。
fn start_reaper(weak: Weak<PoolInner>) {
    let spawned = std::thread::Builder::new()
        .name("plugin-pool-reaper".to_string())
        .spawn(move || loop {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let interval = reaper_interval(inner.cfg.idle_timeout);
            let guard = inner.lock_state();
            if inner.shutdown.load(Ordering::SeqCst) {
                return;
            }
            let (guard, _) = inner
                .cv
                .wait_timeout(guard, interval)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(guard);
            inner.reap_idle();
        });
    // 线程起不来也不致命：只是不会自动回收空闲 worker，池本身照常工作
    let _ = spawned;
}

fn reaper_interval(idle_timeout: Duration) -> Duration {
    let quarter = idle_timeout / 4;
    if quarter < REAPER_MIN_INTERVAL {
        REAPER_MIN_INTERVAL
    } else if quarter > REAPER_MAX_INTERVAL {
        REAPER_MAX_INTERVAL
    } else {
        quarter
    }
}

/// 插件名进文件名前的清洗：只留 [A-Za-z0-9_-]，其余变 `_`，最长 32 字符
fn sanitize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len().min(32));
    for ch in name.chars().take(32) {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("plugin");
    }
    out
}

// ─────────────────────────── 一次请求/响应往返 ───────────────────────────

/// 写一行 → 等一行。任何异常都会让调用方回收这个 worker。
fn exchange(
    worker: &mut IdleWorker,
    req: &PluginRequest,
    timeout: Duration,
) -> Result<PluginResponse, PoolError> {
    let line = encode_request(req);
    if let Err(e) = write_line(&mut worker.stdin, &line) {
        let (code, signal) = exit_status(worker);
        if e.kind() == std::io::ErrorKind::BrokenPipe || code.is_some() || signal.is_some() {
            // 管道断了 = 对面已经死了
            return Err(PoolError::Crashed { code, signal });
        }
        return Err(PoolError::Io(e));
    }

    match worker.events.recv_timeout(timeout) {
        Ok(ReadEvent::Line(text)) => {
            if text.trim().is_empty() {
                return Err(PoolError::Protocol("插件输出了空行".to_string()));
            }
            decode_response(&text).map_err(|e| PoolError::Protocol(e.to_string()))
        }
        Ok(ReadEvent::Eof) | Err(RecvTimeoutError::Disconnected) => classify_eof(worker),
        Ok(ReadEvent::Failed(m)) => Err(PoolError::Protocol(format!("读取插件输出失败：{m}"))),
        Err(RecvTimeoutError::Timeout) => Err(PoolError::Timeout(timeout)),
    }
}

/// EOF 之后给进程一点点时间落幕：管道关闭与进程变成僵尸之间有毫秒级的窗口，
/// 直接判定「进程还活着」会把 `exec 1>&-@@; exit@@ 这种正常收尾误判成协议错误。
/// 超过 grace 还活着才按协议错误处理——绝不傻等满 task_timeout。
const EOF_GRACE: Duration = Duration::from_millis(200);

fn classify_eof(worker: &mut IdleWorker) -> Result<PluginResponse, PoolError> {
    let deadline = Instant::now() + EOF_GRACE;
    loop {
        match worker.child.try_wait() {
            // 正常退出但一行都没给 = 协议错误（能跑完说明不是崩溃）
            Ok(Some(status)) if status.success() => {
                return Err(PoolError::Protocol("插件在给出响应前就 EOF 了（exit=0）".to_string()));
            }
            Ok(Some(status)) => {
                let (code, signal) = split_status(&status);
                return Err(PoolError::Crashed { code, signal });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    return Err(PoolError::Protocol(
                        "插件在给出响应前关闭了 stdout（进程仍在运行）".to_string(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => return Err(PoolError::Io(e)),
        }
    }
}

fn write_line(stdin: &mut ChildStdin, line: &str) -> std::io::Result<()> {
    stdin.write_all(line.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()
}

/// 不阻塞地问一下退出状态（已退出就顺手回收）
fn exit_status(worker: &mut IdleWorker) -> (Option<i32>, Option<i32>) {
    match worker.child.try_wait() {
        Ok(Some(status)) => split_status(&status),
        _ => (None, None),
    }
}

fn split_status(status: &std::process::ExitStatus) -> (Option<i32>, Option<i32>) {
    use std::os::unix::process::ExitStatusExt;
    (status.code(), status.signal())
}

impl IdleWorker {
    /// 进程还活着吗（顺带回收已经退出的）
    fn is_healthy(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 空闲的 worker 不该有任何输出堆积；有就说明它状态不对
    fn has_pending_event(&self) -> bool {
        !matches!(self.events.try_recv(), Err(TryRecvError::Empty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::manifest::PluginKind;
    use crate::plugin::protocol::{RequestOptions, ScrapeRequest, SongRef};

    // ─────────────────────────── 测试脚手架 ───────────────────────────

    /// 一个「读一行、回一行」的 daemon 插件：把请求里的 id 原样回填
    const ECHO_LOOP: &str = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  printf '{"id":"%s","protocol":1,"action":"scrape","ok":true}\n' "$id"
done
"#;

    /// 测试用的临时根目录：**Drop 时自动清理**。
    ///
    /// 早先直接返回 PathBuf 且从不收尾 —— 每跑一次 cargo test 就在 /tmp
    /// 留下 18 个 music-robot-pool-* 目录（实测累积到 181 个）。
    struct TempRoot(PathBuf);

    impl std::ops::Deref for TempRoot {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp_root(tag: &str) -> TempRoot {
        let dir = std::env::temp_dir().join(format!("music-robot-pool-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建测试临时目录失败");
        TempRoot(dir)
    }

    fn write_plugin(root: &Path, body: &str) -> PathBuf {
        let path = root.join("plugin.sh");
        std::fs::write(&path, body).expect("写假插件失败");
        path
    }

    fn meta_for(script: &Path, timeout_ms: u64, max: u32) -> PluginMeta {
        PluginMeta {
            name: "fake".to_string(),
            kind: PluginKind::Scraper,
            protocol: 1,
            capabilities: Vec::new(),
            command: vec!["/bin/sh".to_string(), script.to_string_lossy().into_owned()],
            timeout_ms,
            max_concurrency: max,
            path: script.to_path_buf(),
        }
    }

    /// 测试用沙箱：六条 rlimit 全部显式给出**宽松**值。
    /// 不设成 0（不设置）是为了让池的 spawn 路径也真的走一遍 setrlimit，
    /// 而 NPROC 给 4096 是因为 RLIMIT_NPROC 按 uid 全局计数，测试机上不可控。
    fn test_sandbox() -> SandboxConfig {
        SandboxConfig {
            memory_mb: 4096,
            cpu_sec: 60,
            procs: 4096,
            file_mb: 100,
            nofile: 256,
            plugin_uid: None,
            plugin_gid: None,
        }
    }

    fn cfg(max: usize, idle: Duration, task: Duration) -> PoolConfig {
        PoolConfig { max, idle_timeout: idle, task_timeout: task }
    }

    fn scrape_req(id: &str) -> PluginRequest {
        PluginRequest::Scrape(ScrapeRequest {
            id: id.to_string(),
            song: SongRef::default(),
            work_dir: "/tmp".to_string(),
            want: Vec::new(),
            options: RequestOptions::default(),
        })
    }

    fn resp_id(resp: &PluginResponse) -> String {
        match resp {
            PluginResponse::ScrapeOk(r) => r.id.clone(),
            PluginResponse::DownloadOk(r) => r.id.clone(),
            PluginResponse::Error(r) => r.id.clone(),
        }
    }

    fn resp_source(resp: &PluginResponse) -> Option<String> {
        match resp {
            PluginResponse::ScrapeOk(r) => r.source.clone(),
            _ => None,
        }
    }

    fn process_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// 等脚本把 pid 写进文件（最多 secs 秒），返回读到的 pid
    fn wait_for_pids(path: &Path, want: usize, secs: u64) -> Vec<u32> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                let pids: Vec<u32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
                if pids.len() >= want {
                    return pids;
                }
            }
            if Instant::now() >= deadline {
                return Vec::new();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_until(mut cond: impl FnMut() -> bool, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cond()
    }

    // ─────────────────────────── 纯逻辑 ───────────────────────────

    #[test]
    fn new_is_lazy_and_stats_start_at_zero() {
        let root = tmp_root("lazy");
        let script = write_plugin(&root, ECHO_LOOP);
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        );
        assert_eq!(
            pool.stats(),
            PoolStats { idle: 0, busy: 0, spawned_total: 0 },
            "new() 一个进程都不该起"
        );
        assert!(!root.join("work").exists(), "没 acquire 就不该建 work_root");
    }

    #[test]
    fn config_defaults_and_zero_sentinels_fall_back_to_meta() {
        let root = tmp_root("cfg");
        let script = write_plugin(&root, ECHO_LOOP);
        let meta = meta_for(&script, 1234, 3);

        let from_meta = PoolConfig::for_meta(&meta);
        assert_eq!(from_meta.max, 3);
        assert_eq!(from_meta.task_timeout, Duration::from_millis(1234));
        assert_eq!(from_meta.idle_timeout, DEFAULT_IDLE_TIMEOUT);

        let default = PoolConfig::default();
        assert_eq!(default.max, 1);
        assert_eq!(default.idle_timeout, Duration::from_secs(300));
        assert_eq!(default.task_timeout, Duration::from_millis(30_000));

        // 0 / ZERO 哨兵：跟 meta 走
        let pool = WorkerPool::new(
            meta.clone(),
            test_sandbox(),
            root.join("work"),
            PoolConfig { max: 0, idle_timeout: Duration::ZERO, task_timeout: Duration::ZERO },
        );
        assert_eq!(pool.config().max, 3);
        assert_eq!(pool.config().task_timeout, Duration::from_millis(1234));
        assert_eq!(pool.config().idle_timeout, DEFAULT_IDLE_TIMEOUT);
    }

    #[test]
    fn sanitize_name_strips_path_components() {
        assert_eq!(sanitize_name("netease"), "netease");
        assert_eq!(sanitize_name("a-b_c1"), "a-b_c1");
        assert_eq!(sanitize_name(""), "plugin");
        assert_eq!(sanitize_name("../../x"), "______x");
        assert_eq!(sanitize_name("a/b c"), "a_b_c");
        assert_eq!(sanitize_name(&"x".repeat(100)).len(), 32, "名字要截断，不能让目录名无限长");
    }

    #[test]
    fn pool_error_display_is_chinese() {
        let cases = [
            PoolError::Spawn(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file")),
            PoolError::Timeout(Duration::from_millis(300)),
            PoolError::Crashed { code: Some(1), signal: None },
            PoolError::Crashed { code: None, signal: Some(9) },
            PoolError::Protocol("bad".to_string()),
            PoolError::Busy,
            PoolError::Io(std::io::Error::other("disk")),
            PoolError::Shutdown,
        ];
        for e in &cases {
            let text = e.to_string();
            assert!(!text.is_empty());
            assert!(text.contains("插件"), "错误消息应为中文且提到插件：{text}");
        }
        assert!(std::error::Error::source(&cases[0]).is_some(), "Spawn 应暴露底层错误");
        assert!(std::error::Error::source(&cases[6]).is_some(), "Io 应暴露底层错误");
        assert!(std::error::Error::source(&cases[5]).is_none());
        let _boxed: Box<dyn std::error::Error> = Box::new(PoolError::Busy);
    }

    // ─────────────────────────── 懒加载 / 复用 ───────────────────────────

    #[test]
    fn two_calls_reuse_one_worker_process() {
        let root = tmp_root("reuse");
        let script = write_plugin(&root, ECHO_LOOP);
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        );

        let mut guard = pool.acquire(Duration::from_secs(2)).expect("应当借到 worker");
        assert_eq!(pool.stats().busy, 1);
        assert_eq!(pool.stats().idle, 0);

        let first = guard.call(&scrape_req("q1")).expect("第一次调用");
        let second = guard.call(&scrape_req("q2")).expect("第二次调用");
        assert_eq!(resp_id(&first), "q1");
        assert_eq!(resp_id(&second), "q2");
        assert_eq!(pool.stats().spawned_total, 1, "daemon 模式必须复用同一个进程");

        drop(guard);
        assert_eq!(
            pool.stats(),
            PoolStats { idle: 1, busy: 0, spawned_total: 1 },
            "归还后应当变成空闲"
        );
    }

    #[test]
    fn stats_track_idle_and_busy_across_acquires() {
        let root = tmp_root("stats");
        let script = write_plugin(&root, ECHO_LOOP);
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 2),
            test_sandbox(),
            root.join("work"),
            cfg(2, Duration::from_secs(60), Duration::from_secs(5)),
        );

        let mut a = pool.acquire(Duration::from_secs(2)).expect("第一个");
        let b = pool.acquire(Duration::from_secs(2)).expect("第二个");
        assert_eq!(pool.stats(), PoolStats { idle: 0, busy: 2, spawned_total: 2 });
        a.call(&scrape_req("q1")).expect("调用");
        drop(a);
        assert_eq!(pool.stats(), PoolStats { idle: 1, busy: 1, spawned_total: 2 });
        drop(b);
        assert_eq!(pool.stats(), PoolStats { idle: 2, busy: 0, spawned_total: 2 });

        // 再借一次应当复用，不再起新进程
        let mut c = pool.acquire(Duration::from_secs(2)).expect("第三个");
        assert_eq!(pool.stats().spawned_total, 2, "空闲 worker 必须被复用");
        c.call(&scrape_req("q3")).expect("调用");
    }

    // ─────────────────────────── 超时 ───────────────────────────

    #[test]
    fn timeout_is_reported_and_whole_process_group_is_reaped() {
        let root = tmp_root("timeout");
        let pids = root.join("pids.txt");
        let body = format!(
            r#"echo $$ > {p}
sleep 30 &
echo $! >> {p}
read -r line
sleep 30
echo '{{"id":"q","protocol":1,"action":"scrape","ok":true}}'
"#,
            p = pids.display()
        );
        let script = write_plugin(&root, &body);
        let task = Duration::from_millis(300);
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), task),
        );

        let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
        match guard.call(&scrape_req("q1")) {
            Err(PoolError::Timeout(d)) => assert_eq!(d, task),
            other => panic!("期望 Timeout，实际 {other:?}"),
        }
        assert_eq!(pool.stats().busy, 0, "超时后坑位必须立刻释放");

        // 插件在读到请求前就把 shell 和后台 sleep 的 pid 写好了
        let recorded = wait_for_pids(&pids, 2, 2);
        assert_eq!(recorded.len(), 2, "应当记录 shell 与后台 sleep 两个 pid：{recorded:?}");
        assert!(
            wait_until(|| recorded.iter().all(|p| !process_alive(*p)), 3),
            "killpg 之后 shell 与孙进程都该消失：{recorded:?}"
        );
        // 没有僵尸：直接子进程已被 wait 回收，kill(pid,0) 必定 ESRCH
        for p in &recorded {
            assert!(!process_alive(*p), "pid {p} 仍是活的（僵尸也算）");
        }
    }

    #[test]
    fn guard_is_dead_after_a_failed_call() {
        let root = tmp_root("deadguard");
        let script = write_plugin(&root, "read -r line\nsleep 30\n");
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_millis(200)),
        );
        let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
        match guard.call(&scrape_req("q1")) {
            Err(PoolError::Timeout(_)) => {}
            other => panic!("期望 Timeout，实际 {other:?}"),
        }
        match guard.call(&scrape_req("q2")) {
            Err(PoolError::Crashed { .. }) => {}
            other => panic!("同一个 guard 上的第二次调用应当报 Crashed，实际 {other:?}"),
        }
        assert_eq!(pool.stats().busy, 0);
        drop(guard);
        assert_eq!(pool.stats(), PoolStats { idle: 0, busy: 0, spawned_total: 1 });
    }

    // ─────────────────────────── 崩溃恢复 ───────────────────────────

    #[test]
    fn crash_is_reported_and_pool_rebuilds_worker() {
        let root = tmp_root("crash");
        let flag = root.join("crashed.once");
        let body = format!(
            r#"if [ ! -f {f} ]; then
  : > {f}
  exit 1
fi
while IFS= read -r line; do
  printf '{{"id":"ok","protocol":1,"action":"scrape","ok":true}}\n'
done
"#,
            f = flag.display()
        );
        let script = write_plugin(&root, &body);
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        );

        {
            let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
            match guard.call(&scrape_req("q1")) {
                Err(PoolError::Crashed { .. }) => {}
                other => panic!("期望 Crashed，实际 {other:?}"),
            }
        }
        assert_eq!(pool.stats(), PoolStats { idle: 0, busy: 0, spawned_total: 1 });

        // 池必须能重建 worker
        let mut guard = pool.acquire(Duration::from_secs(2)).expect("崩溃后应当能重建");
        let resp = guard.call(&scrape_req("q2")).expect("重建后应当成功");
        assert!(matches!(resp, PluginResponse::ScrapeOk(_)));
        assert_eq!(pool.stats().spawned_total, 2, "崩溃的 worker 不该被复用");
    }

    // ─────────────────────────── 协议健壮性 ───────────────────────────

    #[test]
    fn non_json_output_is_protocol_error() {
        let root = tmp_root("badjson");
        let script = write_plugin(&root, "read -r line\nprintf 'not json\\n'\n");
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(3)),
        );
        let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
        match guard.call(&scrape_req("q1")) {
            Err(PoolError::Protocol(_)) => {}
            other => panic!("期望 Protocol，实际 {other:?}"),
        }
        assert_eq!(pool.stats().busy, 0, "坏 worker 必须被回收");
    }

    #[test]
    fn empty_line_is_protocol_error() {
        let root = tmp_root("emptyline");
        let script = write_plugin(&root, "read -r line\nprintf '\\n'\n");
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(3)),
        );
        let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
        match guard.call(&scrape_req("q1")) {
            Err(PoolError::Protocol(m)) => assert!(m.contains("空行"), "消息应指出空行：{m}"),
            other => panic!("期望 Protocol，实际 {other:?}"),
        }
    }

    #[test]
    fn early_eof_is_protocol_error_but_a_real_crash_is_not() {
        let root = tmp_root("eof");

        // (a) 读完请求就正常退出，一行响应都没有 → Protocol
        let script = write_plugin(&root, "read -r line\nexit 0\n");
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(3)),
        );
        let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
        match guard.call(&scrape_req("q1")) {
            Err(PoolError::Protocol(_)) => {}
            other => panic!("exit=0 的提前 EOF 应当报 Protocol，实际 {other:?}"),
        }

        // (b) 提前关掉 stdout 但进程还活着 → Protocol（不能傻等满超时）
        let root2 = tmp_root("eof-alive");
        let script2 = write_plugin(&root2, "read -r line\nexec 1>&-\nsleep 30\n");
        let pool2 = WorkerPool::new(
            meta_for(&script2, 5000, 1),
            test_sandbox(),
            root2.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(30)),
        );
        let mut guard2 = pool2.acquire(Duration::from_secs(2)).expect("借到 worker");
        let started = Instant::now();
        match guard2.call(&scrape_req("q1")) {
            Err(PoolError::Protocol(_)) => {}
            other => panic!("stdout 关闭应当立刻报 Protocol，实际 {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "不该傻等满 task_timeout：{:?}",
            started.elapsed()
        );
        match guard2.call(&scrape_req("q2")) {
            Err(PoolError::Crashed { .. }) => {}
            other => panic!("失效的 guard 应当报 Crashed，实际 {other:?}"),
        }
    }

    // ─────────────────────────── 背压 ───────────────────────────

    #[test]
    fn max_one_blocks_the_second_acquire_until_busy() {
        let root = tmp_root("busy");
        let script = write_plugin(&root, ECHO_LOOP);
        let pool = Arc::new(WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        ));

        let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
        guard.call(&scrape_req("q1")).expect("第一次调用");

        let shared = Arc::clone(&pool);
        let waiter = std::thread::spawn(move || {
            let started = Instant::now();
            let outcome = shared.acquire(Duration::from_millis(200));
            let busy = matches!(outcome, Err(PoolError::Busy));
            (started.elapsed(), busy)
        });
        let (elapsed, busy) = waiter.join().expect("等待线程不该 panic");
        assert!(busy, "max=1 且被占用时第二次 acquire 应当返回 Busy");
        assert!(
            elapsed >= Duration::from_millis(150),
            "应当先阻塞等待再放弃，实际只等了 {elapsed:?}"
        );
        assert_eq!(pool.stats().spawned_total, 1, "背压期间绝不能偷偷多起进程");

        // 还回去之后，等待者（或下一次 acquire）立刻能拿到
        drop(guard);
        let mut again = pool.acquire(Duration::from_secs(2)).expect("归还后应当立刻借到");
        assert_eq!(pool.stats().spawned_total, 1);
        again.call(&scrape_req("q2")).expect("复用调用");
    }

    // ─────────────────────────── 空闲回收 ───────────────────────────

    #[test]
    fn idle_worker_is_reaped_after_idle_timeout() {
        let root = tmp_root("idle");
        let pids = root.join("pid.txt");
        let body = format!(
            r#"echo $$ > {p}
while IFS= read -r line; do
  printf '{{"id":"ok","protocol":1,"action":"scrape","ok":true}}\n'
done
"#,
            p = pids.display()
        );
        let script = write_plugin(&root, &body);
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_millis(200), Duration::from_secs(5)),
        );

        {
            let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
            guard.call(&scrape_req("q1")).expect("调用");
        }
        assert_eq!(pool.stats().idle, 1, "归还后应当是空闲的");
        let recorded = wait_for_pids(&pids, 1, 2);
        assert_eq!(recorded.len(), 1, "应当记录 shell 的 pid");
        let pid = recorded[0];
        assert!(process_alive(pid));

        assert!(
            wait_until(|| pool.stats().idle == 0, 3),
            "空闲超过 idle_timeout 的 worker 应当被后台线程回收"
        );
        assert!(
            wait_until(|| !process_alive(pid), 3),
            "被回收的 worker 进程也必须真的死掉"
        );
    }

    // ─────────────────────────── work_dir ───────────────────────────

    #[test]
    fn work_dir_is_unique_per_acquire_and_cleaned_on_drop() {
        let root = tmp_root("workdir");
        let work_root = root.join("work");
        let script = write_plugin(&root, ECHO_LOOP);
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            work_root.clone(),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        );

        let first = {
            let mut guard = pool.acquire(Duration::from_secs(2)).expect("第一次 acquire");
            let dir = guard.work_dir().to_path_buf();
            assert!(dir.starts_with(&work_root), "临时目录必须落在 work_root 下：{dir:?}");
            assert!(dir.is_dir(), "acquire 后目录就应当存在");
            guard.call(&scrape_req("q1")).expect("调用");
            dir
        };
        assert!(!first.exists(), "Drop 之后临时目录应当被删掉：{first:?}");

        let second = {
            let guard = pool.acquire(Duration::from_secs(2)).expect("第二次 acquire");
            let dir = guard.work_dir().to_path_buf();
            assert!(dir.is_dir());
            dir
        };
        assert_ne!(first, second, "每次 acquire 都应当是独立目录");
        assert!(!second.exists(), "Drop 之后同样要清理");
    }

    #[test]
    fn plugin_name_with_path_separators_cannot_escape_work_root() {
        let root = tmp_root("sanitize-dir");
        let work_root = root.join("work");
        let script = write_plugin(&root, ECHO_LOOP);
        let mut meta = meta_for(&script, 5000, 1);
        meta.name = "../../evil name".to_string();
        let pool = WorkerPool::new(
            meta,
            test_sandbox(),
            work_root.clone(),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        );

        let guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
        let dir = guard.work_dir();
        assert_eq!(dir.parent(), Some(work_root.as_path()), "必须直接落在 work_root 下");
        let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
        assert!(!name.contains("..") && !name.contains('/'), "目录名不该含路径成分：{name}");
    }

    // ─────────────────────────── 启动失败 / 关闭 ───────────────────────────

    #[test]
    fn spawn_failure_is_reported_and_slot_released() {
        let root = tmp_root("spawnfail");
        let script = write_plugin(&root, ECHO_LOOP);

        let mut meta = meta_for(&script, 5000, 1);
        meta.command = vec!["/definitely/not/a/real/plugin".to_string()];
        let pool = WorkerPool::new(
            meta,
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        );
        match pool.acquire(Duration::from_secs(2)) {
            Err(PoolError::Spawn(_)) => {}
            Err(other) => panic!("期望 Spawn，实际 {other:?}"),
            Ok(_) => panic!("期望 Spawn，实际借到了 worker"),
        }
        assert_eq!(pool.stats(), PoolStats { idle: 0, busy: 0, spawned_total: 0 });
        let leftovers = std::fs::read_dir(root.join("work")).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 0, "spawn 失败不该留下临时目录");

        // 空 command 也要被挡住，而不是 panic
        let mut meta2 = meta_for(&script, 5000, 1);
        meta2.command = Vec::new();
        let pool2 = WorkerPool::new(
            meta2,
            test_sandbox(),
            root.join("work2"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        );
        match pool2.acquire(Duration::from_secs(2)) {
            Err(PoolError::Spawn(_)) => {}
            Err(other) => panic!("空 command 期望 Spawn，实际 {other:?}"),
            Ok(_) => panic!("空 command 期望 Spawn，实际借到了 worker"),
        };
    }

    #[test]
    fn shutdown_kills_idle_workers_and_rejects_acquire() {
        let root = tmp_root("shutdown");
        let pids = root.join("pid.txt");
        let body = format!(
            r#"echo $$ > {p}
while IFS= read -r line; do
  printf '{{"id":"ok","protocol":1,"action":"scrape","ok":true}}\n'
done
"#,
            p = pids.display()
        );
        let script = write_plugin(&root, &body);
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        );

        {
            let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
            guard.call(&scrape_req("q1")).expect("调用");
        }
        assert_eq!(pool.stats().idle, 1);
        let recorded = wait_for_pids(&pids, 1, 2);
        assert_eq!(recorded.len(), 1);
        let pid = recorded[0];
        assert!(process_alive(pid));

        pool.shutdown();
        assert_eq!(pool.stats().idle, 0, "shutdown 必须清掉空闲 worker");
        assert!(wait_until(|| !process_alive(pid), 3), "shutdown 后进程应当死掉");

        match pool.acquire(Duration::from_millis(50)) {
            Err(PoolError::Shutdown) => {}
            Err(other) => panic!("shutdown 后期望 Shutdown，实际 {other:?}"),
            Ok(_) => panic!("shutdown 后期望 Shutdown，实际借到了 worker"),
        }
        pool.shutdown(); // 幂等
        pool.shutdown();
    }

    // ─────────────────────────── 与沙箱的集成 ───────────────────────────

    // 插件脚本体里读 /proc/self/status 的 NoNewPrivs —— prctl 与 /proc 都是 Linux 专有。
    #[cfg(target_os = "linux")]
    #[test]
    fn pool_spawns_plugins_under_the_sandbox() {
        let root = tmp_root("sandboxed");
        let script = write_plugin(
            &root,
            r#"while IFS= read -r line; do
  nnp=$(grep -m1 '^NoNewPrivs:' /proc/self/status | tr -cd '0-9')
  printf '{"id":"q","protocol":1,"action":"scrape","ok":true,"source":"nnp=%s"}\n' "$nnp"
done
"#,
        );
        let pool = WorkerPool::new(
            meta_for(&script, 5000, 1),
            test_sandbox(),
            root.join("work"),
            cfg(1, Duration::from_secs(60), Duration::from_secs(5)),
        );

        let mut guard = pool.acquire(Duration::from_secs(2)).expect("借到 worker");
        let first = guard.call(&scrape_req("q1")).expect("第一次调用");
        assert_eq!(
            resp_source(&first).as_deref(),
            Some("nnp=1"),
            "池 spawn 出来的插件必须带 PR_SET_NO_NEW_PRIVS（证明 apply 真的生效）"
        );
        let second = guard.call(&scrape_req("q2")).expect("第二次调用");
        assert_eq!(resp_source(&second).as_deref(), Some("nnp=1"));
    }
}
