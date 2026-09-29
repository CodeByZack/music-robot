//! S17 · 后台长任务注册表（单例锁 + 状态快照）。
//!
//! 扫描 / 刮削这类任务一轮要几秒到几分钟，接口只负责「触发 + 轮询」：
//! handler 立刻返回 202 + batch_id，真正的活在本模块登记的独立线程里跑。
//!
//! ## 为什么不用 spawn_blocking
//!
//! tokio 的 blocking 线程池是给「短」阻塞用的。长任务会把它的线程占满，进而饿死
//! 所有依赖 spawn_blocking 的请求 —— 本项目的鉴权查库、/healthz 探活都走它。
//! 所以长任务一律 std::thread::spawn 到独立 OS 线程（见 routes::jobs）。
//!
//! ## 单例锁为什么是原子的
//!
//! 每类任务（scan / scrape 各一把）在注册表里只有一条「当前运行中」的登记。
//! JobRegistry::try_start 在**同一个临界区**里完成「查是否在跑 → 登记 → 发
//! batch_id」，不存在 check-then-act 窗口：并发 N 个触发请求只可能有一个拿到
//! JobGuard，其余拿到 AlreadyRunning，路由层转 409。
//!
//! ## 锁一定会释放
//!
//! 释放动作挂在 JobGuard 的 Drop 上，而不是 JoinHandle：线程正常结束、
//! 提前 return、乃至 panic 展开，Drop 都会跑，登记被摘除。
//! JobGuard::run_catching_panic 还会把 panic 收敛成 failed 状态，
//! 因此一次失败不会把这类任务永久锁死（否则只能重启服务）。
//!
//! ## 注册表的锁只用来读写状态
//!
//! 所有操作都是「加锁 → 克隆出需要的数据 → 立刻放锁」。绝不在持锁期间调用
//! 数据库、插件池、扫描器这类会长期阻塞的东西（那是本机踩过的死锁形态）。
//! 运行中任务的实时进度由 ProgressSource 提供（内部是原子量），放锁之后才去读。

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};

use crate::db::now_unix_ms;

/// 注册表里最多保留多少条任务记录。超出后从最老的**已结束**任务开始丢，
/// 运行中的任务永不丢弃（同类最多两条）。避免长跑服务里记录无限膨胀。
const MAX_JOBS: usize = 256;

/// 任务种类。scan / scrape 各自一把单例锁，互不影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// 曲库扫描
    Scan,
    /// 批量刮削
    Scrape,
}

impl JobKind {
    /// 机器可读的稳定串（进响应体，前端按它分支）。
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Scan => "scan",
            JobKind::Scrape => "scrape",
        }
    }
}

/// 任务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// 正在跑
    Running,
    /// 正常跑完（含「跑了但没活可干」）
    Done,
    /// 整轮失败（配置 / 依赖出错，或线程 panic）
    Failed,
}

impl JobStatus {
    /// 机器可读的稳定串（进响应体）。
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
        }
    }
}

/// 任务进度计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// 本批总数（扫描 = 遍历到的文件数；刮削 = 本批取的曲目数）
    pub total: usize,
    /// 已成功处理
    pub done: usize,
    /// 已失败
    pub failed: usize,
    /// 已跳过（扫描含未变跳过与去重跳过；刮削含 processing 跳过）
    pub skipped: usize,
}

/// 运行中任务的**实时**进度源。
///
/// 轮询接口会在请求线程上读它，所以实现内部必须是无锁或短锁（本仓库用原子量）。
pub trait ProgressSource: Send + Sync {
    /// 当前进度快照。
    fn counters(&self) -> Counters;
}

/// 一个任务对外可见的完整状态。
///
/// 字段就是画布 UT 点名的那些，JobState::to_json 逐字段输出，
/// 客户端能从 JSON 里完整读出来。
#[derive(Clone)]
pub struct JobState {
    /// 任务唯一标识
    pub batch_id: String,
    /// 任务种类
    pub kind: JobKind,
    /// 当前状态
    pub status: JobStatus,
    /// 开始时刻（Unix 毫秒）
    pub started_at: i64,
    /// 结束时刻（Unix 毫秒）；运行中为 None → JSON null
    pub finished_at: Option<i64>,
    /// 进度计数
    pub counters: Counters,
    /// 中文摘要（成功时是统计回报，失败时是错误摘要）；无内容为 None → JSON null
    pub message: Option<String>,
    /// 运行中才有效的实时进度源（内部原子量，读取不阻塞）
    live: Option<Arc<dyn ProgressSource>>,
}

impl JobState {
    /// 按画布字段逐项手写响应体（项目规范：不引 serde derive）。
    pub fn to_json(&self) -> Value {
        json!({
            "batch_id": self.batch_id,
            "kind": self.kind.as_str(),
            "status": self.status.as_str(),
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "total": self.counters.total,
            "done": self.counters.done,
            "failed": self.counters.failed,
            "skipped": self.counters.skipped,
            "message": self.message,
        })
    }
}

/// 任务收尾结果。
pub struct JobOutcome {
    /// 最终状态
    pub status: JobStatus,
    /// 最终计数
    pub counters: Counters,
    /// 中文摘要（可空）
    pub message: Option<String>,
}

impl JobOutcome {
    /// 正常结束。
    pub fn done(counters: Counters, message: Option<String>) -> JobOutcome {
        JobOutcome {
            status: JobStatus::Done,
            counters,
            message,
        }
    }

    /// 整轮失败。文件 / 曲目级失败数在 counters 里另行体现，这里不重复计。
    pub fn failed(message: impl Into<String>) -> JobOutcome {
        JobOutcome {
            status: JobStatus::Failed,
            counters: Counters::default(),
            message: Some(message.into()),
        }
    }
}

/// 同类任务已经有在跑的了（路由层据此转 409）。
#[derive(Debug, Clone)]
pub struct AlreadyRunning {
    /// 正在跑的 batch_id
    pub batch_id: String,
    /// 它的开始时刻（Unix 毫秒）
    pub started_at: i64,
}

/// 注册表内部状态：任务记录 + 每类任务的「运行中」登记。
#[derive(Default)]
struct RegistryInner {
    /// 任务记录，按创建顺序排列（GET /api/jobs 按此顺序展示）
    jobs: Vec<JobState>,
    /// kind → 正在跑的 batch_id。这就是单例锁的载体。
    running: HashMap<JobKind, String>,
}

/// 后台任务注册表。持有它就能查询 / 触发。
pub struct JobRegistry {
    inner: Mutex<RegistryInner>,
    /// batch_id 里的进程内单调序号
    seq: AtomicU64,
}

impl JobRegistry {
    /// 建一个空注册表。
    pub fn new() -> JobRegistry {
        JobRegistry {
            inner: Mutex::new(RegistryInner::default()),
            seq: AtomicU64::new(0),
        }
    }

    /// 尝试开始一类任务。
    ///
    /// **原子**：查「是否在跑」与登记新任务在同一个临界区里完成，不存在
    /// check-then-act 竞态。成功返回的 guard 独占该类任务，直到 guard 被消费
    /// 或析构（Drop 兜底，panic 也会释放）。
    pub fn try_start(
        self: &Arc<Self>,
        kind: JobKind,
        live: Option<Arc<dyn ProgressSource>>,
    ) -> Result<JobGuard, AlreadyRunning> {
        let mut inner = lock(&self.inner);

        if let Some(active) = inner.running.get(&kind) {
            let started_at = inner
                .jobs
                .iter()
                .find(|job| &job.batch_id == active)
                .map(|job| job.started_at)
                .unwrap_or(0);
            return Err(AlreadyRunning {
                batch_id: active.clone(),
                started_at,
            });
        }

        let batch_id = self.next_batch_id(&inner, kind);
        inner.jobs.push(JobState {
            batch_id: batch_id.clone(),
            kind,
            status: JobStatus::Running,
            started_at: now_unix_ms(),
            finished_at: None,
            counters: Counters::default(),
            message: None,
            live,
        });
        // 登记运行中 = 上锁。之后同类触发一律 AlreadyRunning。
        inner.running.insert(kind, batch_id.clone());
        prune(&mut inner);

        Ok(JobGuard {
            registry: Arc::clone(self),
            kind,
            batch_id,
            finished: false,
        })
    }

    /// 按 batch_id 查一条状态（不存在 → None）。放锁后才读实时进度。
    pub fn get(&self, batch_id: &str) -> Option<JobState> {
        let state = {
            let inner = lock(&self.inner);
            inner
                .jobs
                .iter()
                .find(|job| job.batch_id == batch_id)
                .cloned()
        };
        state.map(|mut job| {
            refresh_live(&mut job);
            job
        })
    }

    /// 列出全部保留的任务（按创建顺序，老的在前）。
    pub fn list(&self) -> Vec<JobState> {
        let mut jobs = {
            let inner = lock(&self.inner);
            inner.jobs.clone()
        };
        for job in &mut jobs {
            refresh_live(job);
        }
        jobs
    }

    /// 生成 batch_id。
    ///
    /// 形如 scan-1727512345678-0：kind + Unix 毫秒 + 进程内单调序号。
    ///   * 序号来自 AtomicU64 自增，同进程内绝不重复；
    ///   * 跨进程重启时毫秒时间戳会变；万一同一毫秒重启，序号也从头开始，
    ///     所以插入前还会查一次重名，撞了就继续推进序号（见 next_batch_id）。
    /// 两层保险叠加，不需要引 uuid 依赖。
    fn next_batch_id(&self, inner: &RegistryInner, kind: JobKind) -> String {
        loop {
            let seq = self.seq.fetch_add(1, Ordering::SeqCst);
            let id = format!("{}-{}-{}", kind.as_str(), now_unix_ms(), seq);
            if !inner.jobs.iter().any(|job| job.batch_id == id) {
                return id;
            }
        }
    }

    /// 收尾：落状态 + 释放单例锁。两件事在同一个临界区里做，所以一旦查询接口
    /// 看到终态，锁必然已经释放。
    fn mark_finished(&self, batch_id: &str, kind: JobKind, outcome: JobOutcome) {
        let mut inner = lock(&self.inner);
        if let Some(job) = inner.jobs.iter_mut().find(|job| job.batch_id == batch_id) {
            job.status = outcome.status;
            job.finished_at = Some(now_unix_ms());
            job.counters = outcome.counters;
            job.message = outcome.message;
            job.live = None;
        }
        if inner.running.get(&kind).map(String::as_str) == Some(batch_id) {
            inner.running.remove(&kind);
        }
    }
}

/// 一次任务运行的凭证，同时承担「释放单例锁」的职责。
///
/// 把它 move 进工作线程即可；无论工作怎么结束，Drop 都会摘掉运行中登记。
pub struct JobGuard {
    registry: Arc<JobRegistry>,
    kind: JobKind,
    batch_id: String,
    finished: bool,
}

impl JobGuard {
    /// 本次运行的 batch_id。
    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }

    /// 跑一段工作并把 panic 收敛成 failed 状态（解锁由 Drop 保证）。
    ///
    /// worker 是同步阻塞的长任务，调用方必须已经把它放进独立 OS 线程。
    pub fn run_catching_panic<F>(self, work: F)
    where
        F: FnOnce() -> JobOutcome,
    {
        match catch_unwind(AssertUnwindSafe(work)) {
            Ok(outcome) => self.finish(outcome),
            Err(_) => self.finish(JobOutcome::failed(
                "任务异常退出（内部 panic），独占锁已释放",
            )),
        }
    }

    /// 显式收尾，然后消费掉 guard（Drop 看到 finished 就不再重复处理）。
    fn finish(mut self, outcome: JobOutcome) {
        self.registry.mark_finished(&self.batch_id, self.kind, outcome);
        self.finished = true;
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // 没走 finish（提前 return / panic 展开）也要释放锁，否则这类任务永久锁死。
        self.registry.mark_finished(
            &self.batch_id,
            self.kind,
            JobOutcome::failed("任务异常退出，独占锁已释放"),
        );
    }
}

/// 运行中的任务：读一次实时进度覆盖计数（放锁之后调用）。
fn refresh_live(job: &mut JobState) {
    if job.status != JobStatus::Running {
        return;
    }
    if let Some(live) = job.live.as_ref() {
        job.counters = live.counters();
    }
}

/// 超出上限时丢弃最老的已结束任务；全在运行就保持原样。
fn prune(inner: &mut RegistryInner) {
    while inner.jobs.len() > MAX_JOBS {
        let victim = inner
            .jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| job.status != JobStatus::Running)
            .min_by_key(|(_, job)| job.started_at)
            .map(|(index, _)| index);
        match victim {
            Some(index) => {
                inner.jobs.remove(index);
            }
            None => break,
        }
    }
}

/// 取锁；中毒（持锁线程 panic）时取回内部值继续用 —— 注册表里只有普通数据，
/// 没有需要靠 poisoning 保护的半成品不变式。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    /// 验证：16 个线程同时 try_start 同一类任务，恰好一个拿到 guard，其余全是
    /// AlreadyRunning —— 证明单例锁是原子的，不是 check-then-act。
    ///
    /// 用两道 barrier 保证「所有人试完之前，赢家一直持有 guard」：
    /// 第一道让 16 个线程同时发起，第二道让赢家持锁等到所有人都试过一次，
    /// 否则赢家提前析构（等于提前放锁）会让后面的人误判成「没人占着」。
    #[test]
    fn try_start_is_atomic_across_threads() {
        const N: usize = 16;
        let registry = Arc::new(JobRegistry::new());
        let start = Arc::new(Barrier::new(N));
        let attempted = Arc::new(Barrier::new(N));

        let mut handles = Vec::with_capacity(N);
        for _ in 0..N {
            let registry = Arc::clone(&registry);
            let start = Arc::clone(&start);
            let attempted = Arc::clone(&attempted);
            handles.push(std::thread::spawn(move || {
                start.wait();
                let guard = registry.try_start(JobKind::Scan, None).ok();
                // 赢家在这里持着锁，直到 16 个线程都试过一轮。
                attempted.wait();
                guard.is_some()
            }));
        }

        let winners = handles
            .into_iter()
            .map(|handle| handle.join().expect("并发线程不应 panic"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1, "并发触发必须恰好一个成功");
    }

    /// 验证：guard 没走 finish 就析构（等价 panic 展开）时，锁照样释放，
    /// 状态被兜底成 failed，并且这类任务仍能再次触发。
    #[test]
    fn dropping_guard_releases_lock_and_marks_failed() {
        let registry = Arc::new(JobRegistry::new());
        let batch_id = {
            let guard = registry.try_start(JobKind::Scrape, None).expect("首次触发");
            let id = guard.batch_id().to_string();
            assert!(
                registry.try_start(JobKind::Scrape, None).is_err(),
                "运行中必须拒绝第二次触发"
            );
            id
        };

        let state = registry.get(&batch_id).expect("状态还在");
        assert_eq!(state.status, JobStatus::Failed);
        assert!(state.finished_at.is_some(), "兜底收尾也要写结束时刻");
        assert!(state.message.is_some(), "兜底收尾要有中文原因");

        assert!(
            registry.try_start(JobKind::Scrape, None).is_ok(),
            "上一次退出后必须能再次触发"
        );
    }

    /// 验证：worker 里 panic 也会释放锁并落 failed，而不是把任务永久锁死。
    #[test]
    fn panic_inside_worker_still_releases_lock() {
        let registry = Arc::new(JobRegistry::new());
        let guard = registry.try_start(JobKind::Scan, None).expect("触发");
        let batch_id = guard.batch_id().to_string();
        guard.run_catching_panic(|| -> JobOutcome { panic!("模拟 worker 崩溃") });

        let state = registry.get(&batch_id).expect("状态还在");
        assert_eq!(state.status, JobStatus::Failed);
        assert!(state.message.is_some());
        assert!(
            registry.try_start(JobKind::Scan, None).is_ok(),
            "panic 之后仍要能再次触发"
        );
    }

    /// 验证：scan 与 scrape 各有一把锁，互不阻塞。
    #[test]
    fn kinds_have_independent_locks() {
        let registry = Arc::new(JobRegistry::new());
        let scan = registry.try_start(JobKind::Scan, None).expect("触发扫描");
        let scrape = registry.try_start(JobKind::Scrape, None).expect("触发刮削");
        assert_ne!(scan.batch_id(), scrape.batch_id());
        assert!(registry.try_start(JobKind::Scan, None).is_err());
        assert!(registry.try_start(JobKind::Scrape, None).is_err());
    }

    /// 验证：batch_id 在进程内绝不重复（递增序号 + 时间戳）。
    #[test]
    fn batch_ids_are_unique() {
        let registry = Arc::new(JobRegistry::new());
        let mut seen = std::collections::HashSet::new();
        for _ in 0..300 {
            let guard = registry.try_start(JobKind::Scan, None).expect("触发");
            assert!(
                seen.insert(guard.batch_id().to_string()),
                "batch_id 出现重复"
            );
            guard.finish(JobOutcome::done(Counters::default(), None));
        }
    }

    /// 验证：未知 batch_id 查不到（路由层据此转 404）。
    #[test]
    fn unknown_batch_id_is_none() {
        let registry = JobRegistry::new();
        assert!(registry.get("no-such-id").is_none());
        assert!(registry.list().is_empty());
    }
}
