//! S13 · 刮削编排 —— ScrapeService（单曲）+ BatchRunner（批量）。
//!
//! 队列就是 `songs` 表本身（`scrape_status = 'pending'`），**不另建任务表**；
//! 状态机 pending → processing → done / failed 全部落在 songs.scrape_status 上。
//!
//! ## 画布区块 ⑦ 的命中判定（逐条对应到实现）
//!
//! | 画布规则 | 实现位置 |
//! |---|---|
//! | confidence 0.00 ~ 1.00 | 协议层 decode_response 已保证是 f64；越界值不做裁剪，照实比较 |
//! | 插件未返回 confidence → 0.50 | 协议层 DEFAULT_CONFIDENCE（decode_response） |
//! | ≥ 0.80 视为命中 → 停止回退、进入落库 | [HIT_CONFIDENCE] + scrape_song 的 for 循环 break |
//! | < 0.80 当作未命中 → 试下一个插件 | try_plugin 的 AttemptOutcome::Miss |
//! | 命中但某字段为空 / 缺省 → 该字段不修改 | build_edit_meta / apply_tags_to_song 的 `_` 分支 |
//! | 全部插件试完仍无命中 → status = failed | scrape_song 的 None 分支 |
//!
//! **0.80 这个阈值的来源**：画布区块 ⑦ 写死「≥ 0.80 视为命中」。协议层的
//! `DEFAULT_CONFIDENCE = 0.50` 只是「插件没返回 confidence」的兜底，0.50 < 0.80，
//! 所以**插件不返回 confidence 就等于未命中**——这是画布规则的直接推论，不是 bug。
//!
//! ## 画布区块 ③ 的刮削规则（逐条对应到实现）
//!
//! | 画布规则 | 实现位置 |
//! |---|---|
//! | 触发 1：每首歌首次入库自动刮削 | 队列 = songs 表里 scrape_status = pending 的行（扫描入库即 pending） |
//! | 触发 2：手动重刮（单曲 / 批量失败项） | `POST /api/scrape` 的 body 选队列 → [BatchRunner::run_ids] / [BatchRunner::run_failed] |
//! | 失败不自动重试 | 队列只取 pending；failed 行要靠 `{"mode":"failed"}` 显式重新提交 |
//! | 调度：顺序回退（顺序尝试插件，命中即停） | [ScrapeService::scrape_song] 的 for 循环 |
//! | 命中 → 写回文件标签 + 更新 DB → done | commit_hit |
//! | 歌词只入 DB（songs.lyrics），不写文件 | commit_hit 里 Id3EditMeta::lyrics / lyrics_timed 恒为 None |
//!
//! ## 安全不变式
//!
//! * **processing 是真落库的中间态**：占位走一条 CAS UPDATE（`scrape_status <> 'processing'`），
//!   受影响行数为 0 就说明别的线程正在处理这首歌，直接返回 [ScrapeOutcome::SkippedBusy]。
//!   并发 / 重复触发因此不可能把同一首歌处理两遍。
//! * **写文件之前先 note_write**：写回走 atomic_replace（rename 覆盖），inotify 在原路径报
//!   IN_MOVED_TO，与「外部新建文件」在位掩码上不可区分。不登记自写抑制，
//!   监听器就会把自己写的文件当成新文件，触发无谓重扫（甚至刮削死循环）。
//! * **封面路径过两道闸**：先 [validate_cover_path]（协议层：必须是相对 work_dir 的安全
//!   相对路径），再 [PathSandbox]（canonicalize 后仍要落在 work_dir 内，挡 symlink 逃逸）。
//!   校验失败按错误处理 —— 绝不让越界路径进 pictures。
//! * **歌词绝不写文件**：插件返回的 lyrics 只进 songs.lyrics，Id3EditMeta 的 lyrics /
//!   lyrics_timed 一律保持 None（= 不修改），保留文件里已有的歌词帧。
//! * **写完文件刷新 file_size / file_mtime**：写标签会改 mtime，不刷新的话下一轮扫描会把
//!   这首歌判成「有变化」再重读一遍（无害但是无谓的抖动）。
//!
//! ## 偏离说明（与任务书的差异，都写在明处）
//!
//! 1. **服务层出现了两条手写 SQL**（队列查询 + processing 占位）。songs repo 没有
//!    「按 scrape_status 列行」的函数，而本步不允许改 src/db/**；如果不写 SQL 就只能
//!    用 `songs::list` 全表分页后在内存里过滤，那是 O(n²) 且语义更差。两条 SQL 都只碰
//!    songs 表、全部走参数绑定，且集中在 queue_ids / claim_song 两个函数里。
//! 2. **album 的 upsert 与 search_text 重建**：刮削返回的专辑名必须落进 songs.album_id，
//!    否则「更新 DB」只更新了一半；search_text 不同步重建则搜索会指向旧标题。
//!    两者的口径与 library.rs 里的私有实现一致（这里是复制，因为不许改 library.rs）。
//! 3. **request_delay_ms 做成真的节流**（同一插件相邻请求的最小间隔）。max_retry 故意不接线：
//!    画布区块 ③ 写死「失败不自动重试」，本步不引入重试。
//! 4. **封面大小上限 16 MiB**：插件是不受信任的子进程，不能让它把任意大的文件读进内存。
//!
//! ## 已知取舍
//!
//! * 进程在 processing 期间崩溃 / panic，这首歌会**停留在 processing**，需要人工或
//!   后续的「重置僵死 processing」动作才能再刮（本步不做自动回收，宁可卡住也不误删数据）。
//! * [BatchRunner] 用线程实现 concurrency；`WorkerPool` 是同步且带内部背压的，
//!   借不到 worker 时会按 task_timeout 等待（不是直接判未命中就回退到下一个插件）。

use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use crate::config::ScrapeConfig;
use crate::db::models::{Album, ScrapeStatus, Song};
use crate::db::pool::{DbPool, DbPoolError};
use crate::db::repos::{albums, songs, RepoError};
use crate::fs::PathSandbox;
use crate::plugin::protocol::{
    validate_cover_path, CoverRef, ErrorCode, FieldUpdate, PluginRequest, PluginResponse,
    RequestOptions, ScrapeRequest, SongRef, TagValue, Tags,
};
use crate::plugin::{PluginMeta, PoolConfig, SandboxConfig, WorkerPool};
use crate::tag::read::Picture;
use crate::tag::write::{sniff_image_mime, write_tags, Id3EditMeta};
use crate::watcher::suppress::SelfWriteRegistry;

// ─────────────────────────────────────────────────────────────────────────────
// 常量
// ─────────────────────────────────────────────────────────────────────────────

/// 命中阈值 —— 来源：画布区块 ⑦「≥ 0.80 视为命中 → 停止回退，进入落库」。
///
/// 注意协议层的 [crate::plugin::DEFAULT_CONFIDENCE] = 0.50 是「插件没返回 confidence」
/// 的兜底值，0.50 < 0.80，因此**插件不返回 confidence 就等于未命中**。
pub const HIT_CONFIDENCE: f64 = 0.80;

/// 封面文件大小上限（16 MiB）。插件是不可信子进程，不能让它把任意大的文件读进内存。
const MAX_COVER_BYTES: u64 = 16 * 1024 * 1024;

/// 刮削请求里点名要的字段（与 [Tags] 的六个字段同口径）。
const WANT_FIELDS: [&str; 6] = ["title", "artist", "album", "year", "genre", "track"];

/// 借不到 worker 时的兜底等待（配置里 task_timeout_sec 写 0 = 不设超时时用）。
const DEFAULT_ACQUIRE_WAIT: Duration = Duration::from_secs(30);

// ─────────────────────────────────────────────────────────────────────────────
// 时钟（冷却判定的可注入依赖）
// ─────────────────────────────────────────────────────────────────────────────

/// 单调时钟。生产用 [SystemClock]，测试注入一个可手动推进的假时钟，
/// 这样「冷却 60 秒后恢复调用」不需要真的 sleep 60 秒。
pub trait Clock: Send + Sync {
    /// 当前单调时刻
    fn now(&self) -> Instant;
}

/// 系统单调时钟（默认实现）。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 错误类型
// ─────────────────────────────────────────────────────────────────────────────

/// 刮削编排层的错误。**单曲刮不到不算这里的错误** —— 那是正常业务分支，
/// 结果记录在数据库的 scrape_status / scrape_error 与 [ScrapeOutcome] 里；
/// 这里只报让流程无法继续的故障（借不到连接、数据库读写失败、行不存在）。
#[derive(Debug)]
pub enum ScrapeError {
    /// 从连接池借连接失败
    Pool(DbPoolError),
    /// 数据库读写失败
    Repo(RepoError),
    /// 库里没有这条在库曲目（id 不存在或已被软删）
    SongNotFound {
        /// 曲目主键
        id: i64,
    },
}

impl fmt::Display for ScrapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScrapeError::Pool(e) => write!(f, "刮削时借数据库连接失败：{e}"),
            ScrapeError::Repo(e) => write!(f, "刮削时数据库读写失败：{e}"),
            ScrapeError::SongNotFound { id } => {
                write!(f, "刮削失败：曲库里没有 id={id} 的在库曲目")
            }
        }
    }
}

impl std::error::Error for ScrapeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScrapeError::Pool(e) => Some(e),
            ScrapeError::Repo(e) => Some(e),
            ScrapeError::SongNotFound { .. } => None,
        }
    }
}

impl From<DbPoolError> for ScrapeError {
    fn from(e: DbPoolError) -> Self {
        ScrapeError::Pool(e)
    }
}

impl From<RepoError> for ScrapeError {
    fn from(e: RepoError) -> Self {
        ScrapeError::Repo(e)
    }
}

/// 裸 rusqlite 错误（两条手写 SQL、事务 begin / commit）按同一口径归类。
impl From<rusqlite::Error> for ScrapeError {
    fn from(e: rusqlite::Error) -> Self {
        ScrapeError::Repo(e.into())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 结果结构
// ─────────────────────────────────────────────────────────────────────────────

/// 单曲刮削的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ScrapeOutcome {
    /// 命中并落库（文件标签写回成功、DB 更新、status = done）
    Done {
        /// 曲目主键
        id: i64,
        /// 命中的插件名
        plugin: String,
        /// 该插件给出的 confidence
        confidence: f64,
    },
    /// 全部插件未命中（或全部尝试都出错），status = failed
    Failed {
        /// 曲目主键
        id: i64,
        /// 中文原因，已写进 songs.scrape_error
        error: String,
    },
    /// 这首歌已经是 processing（并发 / 重复触发），本次什么都没做
    SkippedBusy {
        /// 曲目主键
        id: i64,
    },
}

impl ScrapeOutcome {
    /// 曲目主键
    pub fn song_id(&self) -> i64 {
        match self {
            ScrapeOutcome::Done { id, .. }
            | ScrapeOutcome::Failed { id, .. }
            | ScrapeOutcome::SkippedBusy { id } => *id,
        }
    }

    /// 是否命中落库
    pub fn is_done(&self) -> bool {
        matches!(self, ScrapeOutcome::Done { .. })
    }
}

impl fmt::Display for ScrapeOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScrapeOutcome::Done { id, plugin, confidence } => write!(
                f,
                "刮削命中：曲目 {id} 由插件 {plugin} 命中（confidence {confidence:.2}），已落库"
            ),
            ScrapeOutcome::Failed { id, error } => write!(f, "刮削失败：曲目 {id} 未命中，{error}"),
            ScrapeOutcome::SkippedBusy { id } => {
                write!(f, "刮削跳过：曲目 {id} 正在处理中（processing）")
            }
        }
    }
}

/// 单条异常记录（某首歌为什么失败）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapeIssue {
    /// 曲目主键；0 = 与具体曲目无关的批次级问题
    pub song_id: i64,
    /// 中文说明，可直接进日志
    pub message: String,
}

/// 一批刮削的统计回报。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchReport {
    /// 本批提交的曲目数
    pub total: usize,
    /// 命中并落库
    pub done: usize,
    /// 全部插件未命中 / 出错
    pub failed: usize,
    /// 已经处于 processing，本次跳过
    pub skipped: usize,
    /// 异常明细
    pub issues: Vec<ScrapeIssue>,
}

impl fmt::Display for BatchReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "刮削批次完成：共 {} 首，成功 {}，失败 {}，跳过 {}",
            self.total, self.done, self.failed, self.skipped
        )
    }
}

/// 进度的即时快照（总数 / 已完成 / 失败 / 跳过）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// 本批总数
    pub total: usize,
    /// 已完成
    pub done: usize,
    /// 已失败
    pub failed: usize,
    /// 已跳过
    pub skipped: usize,
}

/// 可跨线程读取的进度计数器。
#[derive(Debug, Default)]
pub struct BatchProgress {
    total: AtomicUsize,
    done: AtomicUsize,
    failed: AtomicUsize,
    skipped: AtomicUsize,
}

impl BatchProgress {
    /// 一批开始时重置计数并把总数写进去。
    fn reset(&self, total: usize) {
        self.total.store(total, Ordering::SeqCst);
        self.done.store(0, Ordering::SeqCst);
        self.failed.store(0, Ordering::SeqCst);
        self.skipped.store(0, Ordering::SeqCst);
    }

    /// 当前进度快照。
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            total: self.total.load(Ordering::SeqCst),
            done: self.done.load(Ordering::SeqCst),
            failed: self.failed.load(Ordering::SeqCst),
            skipped: self.skipped.load(Ordering::SeqCst),
        }
    }

    fn record_done(&self) {
        self.done.fetch_add(1, Ordering::SeqCst);
    }

    fn record_failed(&self) {
        self.failed.fetch_add(1, Ordering::SeqCst);
    }

    fn record_skipped(&self) {
        self.skipped.fetch_add(1, Ordering::SeqCst);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 插件注册信息
// ─────────────────────────────────────────────────────────────────────────────

/// 一个参与刮削的插件：清单元数据 + 沙箱配置 + 池的工作根目录。
///
/// 「按顺序排列的插件池」就是这个 Vec 的顺序 —— scrape_song 从头到尾依次尝试，
/// 命中即停。
#[derive(Debug, Clone)]
pub struct ScrapePlugin {
    /// 插件清单（name / command / timeout_ms / max_concurrency…）
    pub meta: PluginMeta,
    /// 子进程沙箱
    pub sandbox: SandboxConfig,
    /// 该插件 worker 的临时目录根（每次 acquire 在其下建独立子目录）
    pub work_root: PathBuf,
}

// ─────────────────────────────────────────────────────────────────────────────
// 服务
// ─────────────────────────────────────────────────────────────────────────────

/// 一个插件槽位：池 + 该插件的冷却 / 节流状态。
struct PluginSlot {
    name: String,
    pool: WorkerPool,
    /// 限流冷却截止时刻；None = 不在冷却
    cooldown_until: Mutex<Option<Instant>>,
    /// 上一次真正发起调用的时刻（request_delay_ms 节流用）
    last_call: Mutex<Option<Instant>>,
}

/// 一次插件尝试的结果（内部类型）。
enum AttemptOutcome {
    /// 命中：confidence 达标，附加数据（封面）也已备好
    Hit(Box<ScrapedHit>),
    /// 未命中 / 出错：带上中文原因，继续试下一个插件
    Miss(String),
}

/// 命中后要落库 / 落文件的全部素材。
struct ScrapedHit {
    plugin: String,
    confidence: f64,
    tags: Tags,
    lyrics: FieldUpdate<String>,
    /// None = 不修改封面；Some(空) = 清除封面；Some(图) = 替换封面
    cover_pictures: Option<Vec<Picture>>,
}

/// 刮削服务：持有连接池 + 按顺序排列的插件池 + 配置。
pub struct ScrapeService {
    db: Arc<DbPool>,
    plugins: Vec<PluginSlot>,
    cfg: ScrapeConfig,
    self_write: Arc<SelfWriteRegistry>,
    clock: Arc<dyn Clock>,
    seq: AtomicU64,
}

impl ScrapeService {
    /// 用系统时钟构造。
    pub fn new(
        db: Arc<DbPool>,
        plugins: Vec<ScrapePlugin>,
        cfg: ScrapeConfig,
        self_write: Arc<SelfWriteRegistry>,
    ) -> ScrapeService {
        ScrapeService::with_clock(db, plugins, cfg, self_write, Arc::new(SystemClock))
    }

    /// 用可注入的时钟构造（测试用假时钟推进冷却窗口）。
    pub fn with_clock(
        db: Arc<DbPool>,
        plugins: Vec<ScrapePlugin>,
        cfg: ScrapeConfig,
        self_write: Arc<SelfWriteRegistry>,
        clock: Arc<dyn Clock>,
    ) -> ScrapeService {
        let slots = plugins
            .into_iter()
            .map(|entry| {
                let mut pool_cfg = PoolConfig::for_meta(&entry.meta);
                if let Some(max) = cfg.per_plugin_max.get(&entry.meta.name) {
                    if *max > 0 {
                        pool_cfg.max = *max as usize;
                    }
                }
                if cfg.task_timeout_sec > 0 {
                    pool_cfg.task_timeout = Duration::from_secs(cfg.task_timeout_sec);
                }
                if cfg.pool_idle_timeout_sec > 0 {
                    pool_cfg.idle_timeout = Duration::from_secs(cfg.pool_idle_timeout_sec);
                }
                PluginSlot {
                    name: entry.meta.name.clone(),
                    pool: WorkerPool::new(entry.meta, entry.sandbox, entry.work_root, pool_cfg),
                    cooldown_until: Mutex::new(None),
                    last_call: Mutex::new(None),
                }
            })
            .collect();
        ScrapeService {
            db,
            plugins: slots,
            cfg,
            self_write,
            clock,
            seq: AtomicU64::new(0),
        }
    }

    /// 按顺序排列的插件名。
    pub fn plugin_names(&self) -> Vec<&str> {
        self.plugins.iter().map(|p| p.name.as_str()).collect()
    }

    /// 生效的配置。
    pub fn config(&self) -> &ScrapeConfig {
        &self.cfg
    }

    /// 单曲刮削：置 processing → 按顺序试插件（命中即停）→ 落库并置 done，
    /// 全部未命中则置 failed 并写 scrape_error。
    ///
    /// 已经是 processing 的行直接返回 [ScrapeOutcome::SkippedBusy]，不重复处理。
    /// 刮削一首，**写回文件**（与历史行为一致，也是绝大多数场景）。
    pub fn scrape_song(&self, song_id: i64) -> Result<ScrapeOutcome, ScrapeError> {
        self.scrape_song_with(song_id, true)
    }

    /// 刮削一首。`write_files = false` 时**只更新数据库、绝不碰原文件**。
    ///
    /// 为什么要有这个档位：刮削是**不可撤销**的（原文件被 rename 覆盖、DB 旧值被 UPDATE）。
    /// 用户明确要求界面上能选「只入库 / 也写文件」，所以在触发那一刻就要定下来，
    /// 而不是等服务端自己决定。
    pub fn scrape_song_with(
        &self,
        song_id: i64,
        write_files: bool,
    ) -> Result<ScrapeOutcome, ScrapeError> {
        // ① 读歌 + 原子占位。插件调用期间**不持有**库连接（插件调用可能几十秒）。
        let song = {
            let conn = self.db.acquire()?;
            let song = songs::get(&conn, song_id, false)?
                .ok_or(ScrapeError::SongNotFound { id: song_id })?;
            if !claim_song(&conn, song_id)? {
                return Ok(ScrapeOutcome::SkippedBusy { id: song_id });
            }
            song
        };
        let album_name = match song.album_id {
            Some(album_id) => {
                let conn = self.db.acquire()?;
                albums::get(&conn, album_id)?.map(|album| album.name)
            }
            None => None,
        };

        // ② 顺序回退：命中即停，后面的插件不再试。
        let mut attempts: Vec<String> = Vec::new();
        let mut hit: Option<ScrapedHit> = None;
        for slot in &self.plugins {
            let now = self.clock.now();
            // 先把值拷出来：if let 的临时量会活到整个 if let 语句结束，
            // 在 body 里再锁同一个 Mutex 会直接死锁（std 的 Mutex 不可重入）。
            let cooldown_until = *lock(&slot.cooldown_until);
            if let Some(until) = cooldown_until {
                if now < until {
                    attempts.push(format!("{}：限流冷却中，本次跳过", slot.name));
                    continue;
                }
                *lock(&slot.cooldown_until) = None;
            }
            match self.try_plugin(slot, &song, album_name.as_deref()) {
                AttemptOutcome::Hit(found) => {
                    hit = Some(*found);
                    break;
                }
                AttemptOutcome::Miss(reason) => attempts.push(format!("{}：{reason}", slot.name)),
            }
        }

        // ③ 落地。
        match hit {
            Some(found) => {
                let result = self.commit_hit(song, found, write_files);
                if let Err(e) = &result {
                    // 尽力把状态落成 failed，别让这首歌永远卡在 processing。
                    let _ = self.set_status(song_id, ScrapeStatus::Failed, Some(&e.to_string()));
                }
                result
            }
            None => {
                let error = if attempts.is_empty() {
                    "没有配置任何刮削插件".to_string()
                } else {
                    attempts.join("；")
                };
                self.set_status(song_id, ScrapeStatus::Failed, Some(&error))?;
                Ok(ScrapeOutcome::Failed { id: song_id, error })
            }
        }
    }

    /// 试一个插件。返回 Miss 表示「这个插件这次不算命中」，调用方继续回退。
    fn try_plugin(&self, slot: &PluginSlot, song: &Song, album_name: Option<&str>) -> AttemptOutcome {
        self.throttle(slot);
        let mut guard = match slot.pool.acquire(self.acquire_wait()) {
            Ok(guard) => guard,
            Err(e) => return AttemptOutcome::Miss(format!("借插件 worker 失败：{e}")),
        };
        let request_id = format!("{}-{}", song.id, self.seq.fetch_add(1, Ordering::Relaxed));
        let request = PluginRequest::Scrape(ScrapeRequest {
            id: request_id.clone(),
            song: song_ref(song, album_name),
            work_dir: guard.work_dir().to_string_lossy().into_owned(),
            want: WANT_FIELDS.iter().map(|name| (*name).to_string()).collect(),
            options: RequestOptions {
                timeout_ms: Some(self.cfg.task_timeout_sec.saturating_mul(1000)),
            },
        });

        let response = match guard.call(&request) {
            Ok(response) => response,
            Err(e) => return AttemptOutcome::Miss(format!("调用插件失败：{e}")),
        };
        if response.id() != request_id {
            return AttemptOutcome::Miss(format!(
                "响应 id 与请求不匹配（期望 {request_id}，收到 {}）",
                response.id()
            ));
        }

        match response {
            PluginResponse::ScrapeOk(ok) => {
                if ok.confidence < HIT_CONFIDENCE {
                    return AttemptOutcome::Miss(format!(
                        "confidence {:.2} 低于命中阈值 {HIT_CONFIDENCE:.2}",
                        ok.confidence
                    ));
                }
                // 封面必须在 guard 还活着的时候读：guard 析构会删掉 work_dir。
                let cover_pictures = match &ok.cover {
                    FieldUpdate::Absent => None,
                    FieldUpdate::Clear => Some(Vec::new()),
                    FieldUpdate::Set(cover) => match load_cover(guard.work_dir(), cover) {
                        Ok(picture) => Some(vec![picture]),
                        Err(message) => return AttemptOutcome::Miss(message),
                    },
                };
                AttemptOutcome::Hit(Box::new(ScrapedHit {
                    plugin: slot.name.clone(),
                    confidence: ok.confidence,
                    tags: ok.tags,
                    lyrics: ok.lyrics,
                    cover_pictures,
                }))
            }
            PluginResponse::Error(err) => {
                if err.error.code == ErrorCode::RateLimited {
                    self.set_cooldown(slot);
                    return AttemptOutcome::Miss(format!(
                        "插件限流（{}），冷却 {} 秒",
                        err.error.message, self.cfg.rate_limit_cooldown_sec
                    ));
                }
                AttemptOutcome::Miss(format!(
                    "插件报错（{}）：{}",
                    err.error.code.as_str(),
                    err.error.message
                ))
            }
            PluginResponse::DownloadOk(_) => {
                AttemptOutcome::Miss("插件返回了下载类响应，action 与 scrape 不匹配".to_string())
            }
        }
    }

    /// 借 worker 的等待上限：至少等到一次 task_timeout，别让池背压被误判成「未命中」。
    fn acquire_wait(&self) -> Duration {
        if self.cfg.task_timeout_sec == 0 {
            DEFAULT_ACQUIRE_WAIT
        } else {
            Duration::from_secs(self.cfg.task_timeout_sec)
        }
    }

    /// 同一插件的相邻请求间隔（request_delay_ms）。写 0 = 不节流。
    fn throttle(&self, slot: &PluginSlot) {
        let delay = Duration::from_millis(self.cfg.request_delay_ms);
        let now = self.clock.now();
        if !delay.is_zero() {
            let previous = *lock(&slot.last_call);
            if let Some(prev) = previous {
                let elapsed = now.saturating_duration_since(prev);
                if elapsed < delay {
                    std::thread::sleep(delay - elapsed);
                }
            }
        }
        *lock(&slot.last_call) = Some(self.clock.now());
    }

    /// 记下该插件的冷却截止时刻。时长溢出（配置给到 u64::MAX）时按不冷却处理，绝不 panic。
    fn set_cooldown(&self, slot: &PluginSlot) {
        let until = self
            .clock
            .now()
            .checked_add(Duration::from_secs(self.cfg.rate_limit_cooldown_sec));
        *lock(&slot.cooldown_until) = until;
    }

    /// 命中后的落地：写文件标签 → 更新 DB → 置状态。
    ///
    /// 文件写失败**不丢刮削成果**：DB 的标签 / 歌词照常写入，状态置 failed 并在
    /// scrape_error 里说明原因，等用户手动重刮。file_size / file_mtime 只在写成功后刷新。
    fn commit_hit(
        &self,
        mut song: Song,
        hit: ScrapedHit,
        write_files: bool,
    ) -> Result<ScrapeOutcome, ScrapeError> {
        let ScrapedHit { plugin, confidence, tags, lyrics, cover_pictures } = hit;
        let path = PathBuf::from(&song.file_path);
        // 只入库模式：**一个字节都不写**。连 note_write 都不登记 —— 没写就没有自写事件，
        // 登记了反而会让监听器误以为这个文件刚被我们改过。
        let write_result = if write_files {
            let meta = build_edit_meta(&tags, cover_pictures);
            // 自写抑制：必须**先登记再写**，否则监听器会把这次写回当成新文件。
            self.self_write.note_write(&path);
            write_tags(&path, &meta)
        } else {
            Ok(())
        };

        let mut conn = self.db.acquire()?;
        let tx = conn.transaction()?;
        // 改前快照：必须赶在 apply_tags_to_song 之前，它是「改了什么」的唯一来源。
        let before = TagSnapshot::of(&song);
        let album_before = album_name(&tx, before.album_id);
        apply_tags_to_song(&tx, &mut song, &tags)?;
        let album_after = album_name(&tx, song.album_id);
        // 歌词只入 DB：Absent = 保留原值，Clear = 清空，Set = 覆盖。
        let mut extra: Vec<String> = Vec::new();
        match &lyrics {
            FieldUpdate::Set(text) => {
                song.lyrics = Some(text.clone());
                extra.push(format!("歌词「已更新 {} 字」", text.chars().count()));
            }
            FieldUpdate::Clear => {
                song.lyrics = None;
                extra.push("歌词「已清空」".to_string());
            }
            FieldUpdate::Absent => {}
        }
        if write_result.is_ok() {
            refresh_file_stat(&mut song);
        }
        songs::update_tags(&tx, &song)?;
        let error_text = write_result
            .as_ref()
            .err()
            .map(|e| format!("写回文件标签失败：{e}"));
        let status = if write_result.is_ok() {
            ScrapeStatus::Done
        } else {
            ScrapeStatus::Failed
        };
        songs::update_scrape_status(&tx, song.id, status, error_text.as_deref())?;
        tx.commit()?;

        // 日志放在 commit 之后：这时说「已落库」才是真的。写回失败的算 error ——
        // 文件没改成功，用户的曲库和 DB 已经不一致了，这条得看得见。
        let changes = before.changes(&TagSnapshot::of(&song), album_before, album_after, extra);
        let detail = if changes.is_empty() {
            "无字段变化".to_string()
        } else {
            format!("改动 {} 处：{}", changes.len(), changes.join("；"))
        };
        match write_result {
            Ok(()) => {
                // ⚠️ 只入库模式必须写明白 —— 否则日志看着像写了文件，事后无从分辨
                let where_to = if write_files {
                    format!("；{}", path.display())
                } else {
                    "；**只入库，未写文件**".to_string()
                };
                crate::serverlog::info(
                    "scrape",
                    format!(
                        "曲目 {} 命中：插件 {plugin}（confidence {confidence:.2}）{detail}{where_to}",
                        song.id,
                    ),
                );
                Ok(ScrapeOutcome::Done { id: song.id, plugin, confidence })
            }
            Err(e) => {
                crate::serverlog::error(
                    "scrape",
                    format!(
                        "曲目 {} 命中但写回文件失败：插件 {plugin}（confidence {confidence:.2}）{detail}；\
                         {} —— {e}；DB 标签已更新，文件仍是旧值，需重刮",
                        song.id,
                        path.display()
                    ),
                );
                Ok(ScrapeOutcome::Failed {
                    id: song.id,
                    error: format!("写回文件标签失败：{e}"),
                })
            }
        }
    }

    /// 只改状态（单条 UPDATE 本身就是原子的）。
    fn set_status(
        &self,
        song_id: i64,
        status: ScrapeStatus,
        scrape_error: Option<&str>,
    ) -> Result<(), ScrapeError> {
        let conn = self.db.acquire()?;
        songs::update_scrape_status(&conn, song_id, status, scrape_error)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 批量执行器
// ─────────────────────────────────────────────────────────────────────────────

/// 批量刮削器：队列就是 songs 表，一个批次取 batch_size 行，用 concurrency 个线程消费。
pub struct BatchRunner {
    service: Arc<ScrapeService>,
    progress: Arc<BatchProgress>,
}

impl BatchRunner {
    /// 绑定一个服务。
    pub fn new(service: Arc<ScrapeService>) -> BatchRunner {
        BatchRunner {
            service,
            progress: Arc::new(BatchProgress::default()),
        }
    }

    /// 底层的刮削服务。
    pub fn service(&self) -> &ScrapeService {
        &self.service
    }

    /// 当前进度快照（运行中也可读）。
    pub fn progress(&self) -> ProgressSnapshot {
        self.progress.snapshot()
    }

    /// 跑一批：取 scrape_status = 'pending' 的曲目（最多 batch_size 首）。
    ///
    /// failed 的行**不会**被这里取到 —— 失败不自动重试。
    pub fn run(&self, write_files: bool) -> Result<BatchReport, ScrapeError> {
        let limit = self.service.cfg.batch_size.max(1) as usize;
        let ids = {
            let conn = self.service.db.acquire()?;
            queue_ids(&conn, ScrapeStatus::Pending, limit)?
        };
        self.process(ids, write_files)
    }

    /// 批量重刮失败项：取 scrape_status = 'failed' 的曲目（最多 batch_size 首）。
    pub fn run_failed(&self, write_files: bool) -> Result<BatchReport, ScrapeError> {
        let limit = self.service.cfg.batch_size.max(1) as usize;
        let ids = {
            let conn = self.service.db.acquire()?;
            queue_ids(&conn, ScrapeStatus::Failed, limit)?
        };
        self.process(ids, write_files)
    }

    /// 显式重刮指定曲目（单曲重刮 / 手工挑出来的集合）。
    pub fn run_ids(&self, ids: &[i64], write_files: bool) -> Result<BatchReport, ScrapeError> {
        self.process(ids.to_vec(), write_files)
    }

    /// 多线程消费一个 id 队列。WorkerPool 同步且带内部背压，这里是真并发
    /// （每个线程各借各的 worker），不是「假并发」。
    fn process(&self, ids: Vec<i64>, write_files: bool) -> Result<BatchReport, ScrapeError> {
        let total = ids.len();
        self.progress.reset(total);
        if total == 0 {
            return Ok(BatchReport::default());
        }

        let queue = Arc::new(Mutex::new(VecDeque::from(ids)));
        let issues: Arc<Mutex<Vec<ScrapeIssue>>> = Arc::new(Mutex::new(Vec::new()));
        let workers = (self.service.cfg.concurrency.max(1) as usize).min(total);

        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let service = Arc::clone(&self.service);
            let progress = Arc::clone(&self.progress);
            let issues = Arc::clone(&issues);
            handles.push(std::thread::spawn(move || loop {
                let next = match queue.lock() {
                    Ok(mut guard) => guard.pop_front(),
                    Err(poisoned) => poisoned.into_inner().pop_front(),
                };
                let Some(id) = next else { break };
                match service.scrape_song_with(id, write_files) {
                    Ok(ScrapeOutcome::Done { .. }) => progress.record_done(),
                    Ok(ScrapeOutcome::Failed { id, error }) => {
                        progress.record_failed();
                        // 逐曲一条：批次汇总只给「失败 N」，不说是哪几首、为什么。
                        crate::serverlog::warn(
                            "scrape",
                            format!("曲目 {id} 未完成刮削：{error}"),
                        );
                        push_issue(&issues, ScrapeIssue { song_id: id, message: error });
                    }
                    Ok(ScrapeOutcome::SkippedBusy { id }) => {
                        progress.record_skipped();
                        crate::serverlog::warn(
                            "scrape",
                            format!("曲目 {id} 正在处理中（processing），本次跳过"),
                        );
                        push_issue(
                            &issues,
                            ScrapeIssue {
                                song_id: id,
                                message: "该曲目正在处理中（processing），本次跳过".to_string(),
                            },
                        );
                    }
                    Err(e) => {
                        progress.record_failed();
                        crate::serverlog::warn("scrape", format!("曲目 {id} 刮削出错：{e}"));
                        push_issue(&issues, ScrapeIssue { song_id: id, message: e.to_string() });
                    }
                }
            }));
        }

        for handle in handles {
            if handle.join().is_err() {
                push_issue(
                    &issues,
                    ScrapeIssue {
                        song_id: 0,
                        message: "刮削工作线程异常退出（panic），本批结果可能不完整；\
                                  受影响曲目可能停留在 processing"
                            .to_string(),
                    },
                );
            }
        }

        let snapshot = self.progress.snapshot();
        let issues = match issues.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        Ok(BatchReport {
            total: snapshot.total,
            done: snapshot.done,
            failed: snapshot.failed,
            skipped: snapshot.skipped,
            issues,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 内部小工具
// ─────────────────────────────────────────────────────────────────────────────

/// 取互斥锁：锁中毒不 panic，取回内部值继续用（与 watcher::suppress 同口径）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 队列查询：按状态取 id（songs 表就是刮削队列，不另建任务表）。
///
/// 【偏离说明】songs repo 没有按 scrape_status 过滤的接口，而本步不允许改 src/db/**，
/// 所以这一条 SQL 写在服务层，全部走参数绑定。
fn queue_ids(
    conn: &Connection,
    status: ScrapeStatus,
    limit: usize,
) -> Result<Vec<i64>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id FROM songs
          WHERE deleted_at IS NULL AND scrape_status = ?1
          ORDER BY id LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        params![status.as_str(), i64::try_from(limit).unwrap_or(i64::MAX)],
        |row| row.get::<_, i64>(0),
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 原子占位：任何非 processing 的状态都可以被抢成 processing（CAS）。
///
/// 返回 true = 这次抢到了；false = 别的线程已经在处理这首（或者行不存在 / 已软删）。
/// 这一步是「并发 / 重复触发不重复处理同一首歌」的唯一依据 —— 状态**真的落了库**。
fn claim_song(conn: &Connection, song_id: i64) -> Result<bool, rusqlite::Error> {
    let now = crate::db::now_unix_ms();
    let changed = conn.execute(
        "UPDATE songs
            SET scrape_status = ?2, scrape_error = NULL, scrape_at = ?3, updated_at = ?3
          WHERE id = ?1 AND deleted_at IS NULL AND scrape_status <> ?2",
        params![song_id, ScrapeStatus::Processing.as_str(), now],
    )?;
    Ok(changed == 1)
}

/// 请求里的歌曲描述。专辑名从 albums 表带出来（songs 表只存 album_id）。
fn song_ref(song: &Song, album_name: Option<&str>) -> SongRef {
    SongRef {
        file_path: Some(song.file_path.clone()),
        title: song.title.clone(),
        artist: song.artists.clone(),
        album: album_name.map(str::to_string),
        duration_ms: song.duration_ms,
        audio_hash: song.audio_hash.clone(),
        fingerprint: None,
    }
}

/// 把插件的三态标签翻成 [Id3EditMeta]。
///
/// 关键语义（画布区块 ⑦）：
///   * `Text(非空)` → 写入；
///   * `Text(空 / 全空白)` → **不修改**（画布：「命中但某字段为空 / 缺省 → 该字段不修改」）；
///   * `Absent`（缺省）→ **不修改**，绝不能退化写成空串；
///   * `Clear`（显式 null）→ 清除，翻译成 unset_fields 里的字段名。
///
/// 歌词一律留 None（= 不修改）：画布区块 ③ 规定歌词只入 DB，不写文件。
fn build_edit_meta(tags: &Tags, pictures: Option<Vec<Picture>>) -> Id3EditMeta {
    let mut meta = Id3EditMeta::default();
    let mut unset: Vec<String> = Vec::new();

    match &tags.title {
        TagValue::Text(text) if !text.trim().is_empty() => meta.title = Some(text.clone()),
        TagValue::Clear => unset.push("title".to_string()),
        _ => {}
    }
    match &tags.artist {
        TagValue::Text(text) if !text.trim().is_empty() => meta.artists = Some(vec![text.clone()]),
        TagValue::Clear => unset.push("artists".to_string()),
        _ => {}
    }
    match &tags.album {
        TagValue::Text(text) if !text.trim().is_empty() => meta.albums = Some(vec![text.clone()]),
        TagValue::Clear => unset.push("albums".to_string()),
        _ => {}
    }
    match &tags.genre {
        TagValue::Text(text) if !text.trim().is_empty() => meta.genres = Some(vec![text.clone()]),
        TagValue::Clear => unset.push("genres".to_string()),
        _ => {}
    }
    match &tags.year {
        TagValue::Text(text) if !text.trim().is_empty() => meta.year = Some(text.clone()),
        TagValue::Number(year) => meta.year = Some(year.to_string()),
        TagValue::Clear => unset.push("year".to_string()),
        _ => {}
    }
    match &tags.track {
        TagValue::Number(track) => meta.track = Some(*track),
        TagValue::Text(text) => {
            if let Ok(track) = text.trim().parse::<i64>() {
                meta.track = Some(track);
            }
        }
        TagValue::Clear => unset.push("track".to_string()),
        _ => {}
    }

    meta.unset_fields = unset;
    meta.pictures = pictures;
    // 歌词只入 DB：lyrics / lyrics_timed 保持 None = 不修改文件里已有的歌词帧。
    meta
}

/// 读插件产出的封面，做成 ID3 的 [Picture]。
///
/// 两道校验缺一不可：
///   1. [validate_cover_path] —— 协议层：必须是相对 work_dir 的安全相对路径；
///   2. [PathSandbox] —— canonicalize 之后仍要落在 work_dir 内（挡 symlink 逃逸）。
/// 任何一步失败都返回中文错误，由调用方当成「这次尝试失败」处理，绝不硬塞越界数据。
fn load_cover(work_dir: &Path, cover: &CoverRef) -> Result<Picture, String> {
    validate_cover_path(&cover.path).map_err(|e| format!("封面路径非法：{e}"))?;
    let sandbox = PathSandbox::new(work_dir).map_err(|e| format!("封面目录不可用：{e}"))?;
    let relative = Path::new(&cover.path);
    let metadata = sandbox
        .metadata(relative)
        .map_err(|e| format!("读不到封面文件：{e}"))?;
    if metadata.len() > MAX_COVER_BYTES {
        return Err(format!(
            "封面文件过大（{} 字节，上限 {MAX_COVER_BYTES} 字节）",
            metadata.len()
        ));
    }
    let data = sandbox
        .read_file(relative)
        .map_err(|e| format!("读取封面失败：{e}"))?;
    if data.is_empty() {
        return Err("插件给出的封面文件是空的".to_string());
    }
    let mime = cover
        .mime
        .as_deref()
        .map(str::trim)
        .filter(|mime| !mime.is_empty())
        .map(str::to_string)
        .or_else(|| sniff_image_mime(&data))
        .unwrap_or_else(|| "image/jpeg".to_string());
    Ok(Picture {
        mime_type: mime,
        // 3 = front cover
        pic_type: 3,
        description: String::new(),
        data,
    })
}

/// 刮削**前后**的标签快照，只为日志里那句「到底改了什么」服务。
///
/// 为什么必须在这里记：命中后原文件被 `atomic_replace` 直接 rename 覆盖，DB 的旧值也
/// 被 UPDATE 掉，**事后没有任何地方能还原出改前的样子**。插件写坏了标签（比如把歌手
/// 全刷成广告词），唯一的线索就是这行日志。
///
/// 不含歌词：歌词走 `FieldUpdate` 三分支，在调用处顺手记更准。
/// 不含专辑名：库里存的是 `album_id`，取名字要多查一次库，放在 [Self::changes] 里按需解析。
#[derive(Clone, PartialEq, Eq)]
struct TagSnapshot {
    title: Option<String>,
    artists: Option<String>,
    genres: Option<String>,
    year: Option<i64>,
    track: Option<i64>,
    album_id: Option<i64>,
}

impl TagSnapshot {
    fn of(song: &Song) -> Self {
        TagSnapshot {
            title: song.title.clone(),
            artists: song.artists.clone(),
            genres: song.genres.clone(),
            year: song.year,
            track: song.track,
            album_id: song.album_id,
        }
    }

    /// 逐字段比对，**只列出真的变了的**。空快照（没改任何字段）返回空 Vec，
    /// 调用方据此说「无字段变化」，而不是打一行没有信息量的空改动。
    fn changes(
        &self,
        after: &Self,
        album_before: Option<String>,
        album_after: Option<String>,
        extra: Vec<String>,
    ) -> Vec<String> {
        let pairs = [
            ("标题", self.title.clone(), after.title.clone()),
            ("歌手", self.artists.clone(), after.artists.clone()),
            ("风格", self.genres.clone(), after.genres.clone()),
            ("年份", self.year.map(|y| y.to_string()), after.year.map(|y| y.to_string())),
            ("轨道", self.track.map(|t| t.to_string()), after.track.map(|t| t.to_string())),
        ];
        let mut out: Vec<String> = pairs
            .into_iter()
            .filter(|(_, old, new)| old != new)
            .map(|(name, old, new)| {
                format!("{name}「{}」→「{}」", show(old.as_deref()), show(new.as_deref()))
            })
            .collect();
        // 专辑比名字而不是比 id：id 变了但名字没变，对用户来说等于没改。
        if album_before != album_after {
            out.push(format!(
                "专辑「{}」→「{}」",
                show(album_before.as_deref()),
                show(album_after.as_deref())
            ));
        }
        out.extend(extra);
        out
    }
}

/// 日志里展示空值：`None` 与空串都视作「(空)」，免得打出「歌手「」→「示例歌手」」这种半截话。
fn show(value: Option<&str>) -> &str {
    match value {
        Some(text) if !text.trim().is_empty() => text,
        _ => "(空)",
    }
}

/// 专辑 id → 名字。查不到（或没有专辑）一律 `None`，**绝不因此让刮削失败** ——
/// 这只是日志的装饰。
fn album_name(conn: &Connection, album_id: Option<i64>) -> Option<String> {
    let id = album_id?;
    albums::get(conn, id).ok().flatten().map(|album| album.name)
}

/// 把插件的标签写进要落库的 Song。语义与 [build_edit_meta] 完全一致（Absent 不动、
/// 空串不动、null 清空），保证文件与 DB 不会各写各的。
fn apply_tags_to_song(conn: &Connection, song: &mut Song, tags: &Tags) -> Result<(), ScrapeError> {
    match &tags.title {
        TagValue::Text(text) if !text.trim().is_empty() => song.title = Some(text.clone()),
        TagValue::Clear => song.title = None,
        _ => {}
    }
    match &tags.artist {
        TagValue::Text(text) if !text.trim().is_empty() => song.artists = Some(text.clone()),
        TagValue::Clear => song.artists = None,
        _ => {}
    }
    match &tags.genre {
        TagValue::Text(text) if !text.trim().is_empty() => song.genres = Some(text.clone()),
        TagValue::Clear => song.genres = None,
        _ => {}
    }
    match &tags.year {
        // 年份解析不出来时保持原值，绝不把已有年份抹成 NULL。
        TagValue::Text(text) if !text.trim().is_empty() => {
            if let Some(year) = parse_year(text) {
                song.year = Some(year);
            }
        }
        TagValue::Number(year) => song.year = Some(*year),
        TagValue::Clear => song.year = None,
        _ => {}
    }
    match &tags.track {
        TagValue::Number(track) => song.track = Some(*track),
        TagValue::Text(text) => {
            if let Ok(track) = text.trim().parse::<i64>() {
                song.track = Some(track);
            }
        }
        TagValue::Clear => song.track = None,
        _ => {}
    }
    match &tags.album {
        TagValue::Text(name) if !name.trim().is_empty() => {
            let album_artist = album_artist_for(song, tags);
            let year = song.year;
            song.album_id = upsert_album(conn, name.trim(), &album_artist, year)?;
        }
        TagValue::Clear => song.album_id = None,
        _ => {}
    }
    song.search_text = rebuild_search_text(conn, song)?;
    Ok(())
}

/// 专辑艺术家：优先沿用这首歌已有的 album_artist，其次用刮削到的 artist，都没有就是空串
/// （repo 约定：album_artist 未知时用空串，SQLite 的 UNIQUE 不把多个 NULL 当冲突）。
fn album_artist_for(song: &Song, tags: &Tags) -> String {
    if let Some(existing) = song.album_artist.as_deref() {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let TagValue::Text(artist) = &tags.artist {
        let trimmed = artist.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    String::new()
}

/// 按 (name, album_artist) upsert 专辑，返回专辑 id。
///
/// 口径与 library.rs 的私有 upsert_album 一致：已有专辑只补缺失的年份，不覆盖封面 / 年份
/// 等已有成果；并发插入撞唯一约束时复用已有行（不让这次刮削白白失败）。
/// 【偏离说明】这里是复制而非复用 —— 那个函数是私有的，本步不允许改 library.rs。
fn upsert_album(
    conn: &Connection,
    name: &str,
    album_artist: &str,
    year: Option<i64>,
) -> Result<Option<i64>, RepoError> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if let Some(existing) = albums::find_by_name_artist(conn, name, album_artist)? {
        if existing.year.is_none() {
            if let Some(year) = year {
                let mut updated = existing.clone();
                updated.year = Some(year);
                albums::update(conn, &updated)?;
            }
        }
        return Ok(Some(existing.id));
    }
    let album = Album {
        id: 0,
        name: name.to_string(),
        album_artist: album_artist.to_string(),
        year,
        cover_data: None,
        cover_mime: None,
        updated_at: 0,
    };
    match albums::insert(conn, &album) {
        Ok(id) => Ok(Some(id)),
        Err(RepoError::Conflict { constraint }) => {
            match albums::find_by_name_artist(conn, &album.name, &album.album_artist)? {
                Some(found) => Ok(Some(found.id)),
                None => Err(RepoError::Conflict { constraint }),
            }
        }
        Err(e) => Err(e),
    }
}

/// 重建 search_text（标题 / 歌手 / 专辑艺术家 / 专辑 / 流派 + 文件名主干，去重后空格相连）。
///
/// 标签变了而 search_text 不同步，搜索结果就会指向旧标题 —— 所以刮削落库必须重算。
/// 【偏离说明】与 library.rs 的同名私有函数同口径，复制原因同上。
fn rebuild_search_text(conn: &Connection, song: &Song) -> Result<Option<String>, RepoError> {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return;
        }
        if !parts.iter().any(|part| part == trimmed) {
            parts.push(trimmed.to_string());
        }
    };

    if let Some(title) = song.title.as_deref() {
        push(title);
    }
    if let Some(artists) = song.artists.as_deref() {
        for artist in artists.split(" / ") {
            push(artist);
        }
    }
    if let Some(album_artist) = song.album_artist.as_deref() {
        push(album_artist);
    }
    if let Some(album_id) = song.album_id {
        if let Some(album) = albums::get(conn, album_id)? {
            push(&album.name);
        }
    }
    if let Some(genres) = song.genres.as_deref() {
        for genre in genres.split(" / ") {
            push(genre);
        }
    }
    if let Some(stem) = Path::new(&song.file_path).file_stem().and_then(|s| s.to_str()) {
        push(stem);
    }

    Ok(if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    })
}

/// 从任意年份文本里抠出一个四位年份（与 library.rs 的 parse_year 同口径）。
fn parse_year(raw: &str) -> Option<i64> {
    let mut run = String::new();
    for ch in raw.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            run.push(ch);
            continue;
        }
        if run.len() == 4 {
            if let Ok(year) = run.parse::<i64>() {
                if (1000..=2999).contains(&year) {
                    return Some(year);
                }
            }
        }
        run.clear();
    }
    None
}

/// 写完文件标签后刷新 file_size / file_mtime。
///
/// 不刷新的话下一轮扫描会看到 mtime 变了，把这首歌判成「有变化」再重读一遍 ——
/// 无害但是无谓的抖动。
fn refresh_file_stat(song: &mut Song) {
    let Ok(metadata) = std::fs::metadata(&song.file_path) else {
        return;
    };
    if let Ok(size) = i64::try_from(metadata.len()) {
        song.file_size = Some(size);
    }
    song.file_mtime = metadata.modified().ok().and_then(system_time_to_ms);
}

/// SystemTime → Unix 毫秒（早于 epoch 的时钟异常返回 None）。
fn system_time_to_ms(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
}

/// 往问题清单里追加一条（锁中毒也不 panic）。
fn push_issue(issues: &Mutex<Vec<ScrapeIssue>>, issue: ScrapeIssue) {
    match issues.lock() {
        Ok(mut guard) => guard.push(issue),
        Err(poisoned) => poisoned.into_inner().push(issue),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 测试
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::db::pool::{DbGuard, TempDb};
    use crate::plugin::manifest::PluginKind;
    use crate::service::library::LibraryService;
    use crate::tag::read::read_tags;
    use crate::watcher::test_support::TempDir;
    use std::collections::HashMap;

    /// 三首**内容互不相同**的真实样本。S6 去重按 audio_hash 判重，同一份音频复制两份
    /// 只会入一条库 —— 多曲目测试必须用不同样本。
    const FIXTURES: [&str; 3] = [
        "华夏传说 - 凤凰传奇.mp3",
        "盛夏-毛不易.mp3",
        "最美情侣-白小白.mp3",
    ];

    // ─────────────────────────── 测试脚手架 ───────────────────────────

    /// 可注入的假时钟：基准时刻 + 手动推进的毫秒偏移。
    ///
    /// 用它来测「冷却到期后恢复调用」，不需要真的 sleep 60 秒。
    struct TestClock {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl TestClock {
        fn new() -> TestClock {
            TestClock {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            }
        }

        fn advance(&self, delta: Duration) {
            self.offset_ms
                .fetch_add(delta.as_millis() as u64, Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
        }
    }

    struct Env {
        pool: Arc<DbPool>,
        /// 必须留在结构体里：TempDb 一析构就删掉库文件
        _temp: TempDb,
        /// 必须留在结构体里：TempDir 一析构就递归删库根与插件目录
        _dir: TempDir,
        root: PathBuf,
        /// 假插件脚本、调用日志与池工作根都放这里
        work: PathBuf,
        clock: Arc<TestClock>,
    }

    impl Env {
        fn new(tag: &str) -> Env {
            let (pool, temp) = DbPool::open_temp(tag).expect("建临时文件库池");
            {
                let mut guard = pool.acquire().expect("借连接");
                migrations::apply(&mut guard).expect("应用迁移");
            }
            let dir = TempDir::new(tag);
            let root = dir.path().join("lib");
            std::fs::create_dir_all(&root).expect("建库根");
            let work = dir.path().join("work");
            std::fs::create_dir_all(&work).expect("建插件目录");
            Env {
                pool: Arc::new(pool),
                _temp: temp,
                _dir: dir,
                root,
                work,
                clock: Arc::new(TestClock::new()),
            }
        }

        /// 拷 n 个真实样本进库根并扫一轮入库（新入库一律 pending = 刮削队列）。
        fn seed(&self, n: usize) -> Vec<Song> {
            assert!(n <= FIXTURES.len(), "样本数量不够");
            let paths: Vec<PathBuf> = (0..n)
                .map(|index| {
                    let dst = self.root.join(format!("song{index}.mp3"));
                    std::fs::copy(fixture(FIXTURES[index]), &dst).expect("拷贝样本");
                    dst
                })
                .collect();
            let service = LibraryService::new(
                Arc::clone(&self.pool),
                vec![self.root.to_string_lossy().into_owned()],
            );
            let report = service.scan().expect("扫描入库");
            assert_eq!(report.added, n, "样本必须全部入库（去重不该影响本测试）");
            let conn = self.conn();
            paths
                .iter()
                .map(|path| {
                    songs::find_by_file_path(&conn, &canonical(path))
                        .expect("按路径查")
                        .expect("样本必须入库")
                })
                .collect()
        }

        /// 单个样本。
        fn seed_one(&self) -> Song {
            self.seed(1).remove(0)
        }

        fn conn(&self) -> DbGuard<'_> {
            self.pool.acquire().expect("借连接")
        }

        fn config(&self) -> ScrapeConfig {
            ScrapeConfig {
                concurrency: 1,
                batch_size: 50,
                task_timeout_sec: 20,
                max_retry: 0,
                request_delay_ms: 0,
                rate_limit_cooldown_sec: 60,
                pool_idle_timeout_sec: 60,
                per_plugin_max: HashMap::new(),
            }
        }

        fn service(&self, plugins: Vec<ScrapePlugin>) -> Arc<ScrapeService> {
            let clock: Arc<dyn Clock> = self.clock.clone();
            Arc::new(ScrapeService::with_clock(
                Arc::clone(&self.pool),
                plugins,
                self.config(),
                Arc::new(SelfWriteRegistry::default()),
                clock,
            ))
        }
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    /// 落库时用的规范路径（与 library 的 canonical_path_string 同口径）
    fn canonical(path: &Path) -> String {
        std::fs::canonicalize(path)
            .expect("canonicalize")
            .to_string_lossy()
            .into_owned()
    }

    /// 测试用沙箱：六条 rlimit 全部显式给出**宽松**值（NPROC 按 uid 全局计数，不可控）。
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

    /// 生成「读一行、记一次调用、执行 body 回一行」的 daemon 插件脚本。
    ///
    /// 模板与 plugin/pool.rs 测试里的 ECHO_LOOP 同形：/bin/sh + read + printf。
    /// body 里可以直接用 `$id`（请求 id）与 `$wd`（本次调用的 work_dir，插件落封面用）。
    fn scrape_script(call_log: &Path, body: &str) -> String {
        let mut script = String::new();
        script.push_str("while IFS= read -r line; do\n");
        script.push_str(&format!(
            "  printf '%s\\n' \"$line\" >> '{}'\n",
            call_log.display()
        ));
        script.push_str(
            "  id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\"\\([^\"]*\\)\".*/\\1/p')\n",
        );
        script.push_str(
            "  wd=$(printf '%s' \"$line\" | sed -n 's/.*\"work_dir\":\"\\([^\"]*\\)\".*/\\1/p')\n",
        );
        script.push_str("  ");
        script.push_str(body);
        script.push_str("\ndone\n");
        script
    }

    /// 注册一个假插件（脚本落在 env.work 下，每个插件一份独立的调用日志）。
    fn plugin(env: &Env, name: &str, body: &str) -> ScrapePlugin {
        let call_log = env.work.join(format!("{name}.calls"));
        let path = env.work.join(format!("{name}.sh"));
        std::fs::write(&path, scrape_script(&call_log, body)).expect("写假插件失败");
        ScrapePlugin {
            meta: PluginMeta {
                name: name.to_string(),
                kind: PluginKind::Scraper,
                protocol: 1,
                capabilities: Vec::new(),
                command: vec!["/bin/sh".to_string(), path.to_string_lossy().into_owned()],
                timeout_ms: 20_000,
                max_concurrency: 1,
                path,
            },
            sandbox: test_sandbox(),
            work_root: env.work.join(format!("run-{name}")),
        }
    }

    /// 该插件被真正调用了多少次（脚本每处理一条请求就往日志追加一行）。
    fn calls(env: &Env, name: &str) -> usize {
        std::fs::read_to_string(env.work.join(format!("{name}.calls")))
            .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
            .unwrap_or(0)
    }

    // ── 假插件的应答体 ──────────────────────────────────────────────────────

    /// 只有 confidence、不带任何标签。
    fn body_confidence(confidence: &str) -> String {
        format!(
            "printf '{{\"id\":\"%s\",\"protocol\":1,\"action\":\"scrape\",\"ok\":true,\"confidence\":{confidence}}}\\n' \"$id\""
        )
    }

    /// 命中并带 tags。
    fn body_hit(confidence: &str, tags: &str) -> String {
        format!(
            "printf '{{\"id\":\"%s\",\"protocol\":1,\"action\":\"scrape\",\"ok\":true,\"confidence\":{confidence},\"tags\":{tags}}}\\n' \"$id\""
        )
    }

    /// 插件报错。
    fn body_error(code: &str) -> String {
        format!(
            "printf '{{\"id\":\"%s\",\"protocol\":1,\"action\":\"scrape\",\"ok\":false,\"error\":{{\"code\":\"{code}\",\"message\":\"fake error\"}}}}\\n' \"$id\""
        )
    }

    /// 带封面的命中。
    fn body_cover(path: &str, confidence: &str) -> String {
        format!(
            "printf '{{\"id\":\"%s\",\"protocol\":1,\"action\":\"scrape\",\"ok\":true,\"confidence\":{confidence},\"cover\":{{\"path\":\"{path}\",\"mime\":\"image/jpeg\"}}}}\\n' \"$id\""
        )
    }

    /// 不是 JSON 的胡话（协议错误）。
    fn body_garbage() -> String {
        "printf 'not json at all\\n'".to_string()
    }

    // ── 1. 顺序回退命中即停 ────────────────────────────────────────────────

    #[test]
    fn fallback_stops_at_the_first_hit() {
        let env = Env::new("scrape-fallback");
        let song = env.seed_one();
        let service = env.service(vec![
            plugin(&env, "low", &body_confidence("0.50")),
            plugin(&env, "high", &body_hit("0.90", "{\"title\":\"Hit Title\"}")),
            plugin(&env, "never", &body_hit("0.99", "{\"title\":\"Never\"}")),
        ]);

        let outcome = service.scrape_song(song.id).expect("刮削");
        match &outcome {
            ScrapeOutcome::Done { plugin, confidence, .. } => {
                assert_eq!(plugin, "high");
                assert!((*confidence - 0.90).abs() < 1e-9);
            }
            other => panic!("应当命中第二个插件，实际 {other:?}"),
        }
        assert_eq!(calls(&env, "low"), 1, "第一个插件必须被尝试");
        assert_eq!(calls(&env, "high"), 1, "第二个插件必须被尝试");
        assert_eq!(calls(&env, "never"), 0, "命中即停：第三个插件绝不能被调用");

        let conn = env.conn();
        let row = songs::get(&conn, song.id, false).expect("查").expect("行");
        assert_eq!(row.scrape_status, ScrapeStatus::Done);
        assert_eq!(row.title.as_deref(), Some("Hit Title"));
        assert_eq!(row.scrape_error, None);

        let tags = read_tags(Path::new(&song.file_path)).expect("读回文件标签");
        assert_eq!(tags.title.as_deref(), Some("Hit Title"), "命中的标签必须写回文件");
    }

    // ── 2. confidence ≥ 0.8 边界两侧 ───────────────────────────────────────

    #[test]
    fn confidence_below_threshold_keeps_falling_back() {
        let env = Env::new("scrape-conf-below");
        let song = env.seed_one();
        let service = env.service(vec![
            plugin(&env, "p79", &body_hit("0.79", "{\"title\":\"Below\"}")),
            plugin(&env, "p80", &body_hit("0.80", "{\"title\":\"At\"}")),
        ]);

        let outcome = service.scrape_song(song.id).expect("刮削");
        match &outcome {
            ScrapeOutcome::Done { plugin, .. } => assert_eq!(plugin, "p80", "0.80 必须算命中"),
            other => panic!("0.79 必须判未命中并继续回退，实际 {other:?}"),
        }
        assert_eq!(calls(&env, "p79"), 1);
        assert_eq!(calls(&env, "p80"), 1);

        let conn = env.conn();
        let row = songs::get(&conn, song.id, false).expect("查").expect("行");
        assert_eq!(row.title.as_deref(), Some("At"), "0.79 的标签绝不能落地");
    }

    #[test]
    fn confidence_below_threshold_alone_marks_failed() {
        let env = Env::new("scrape-conf-alone");
        let song = env.seed_one();
        let before = std::fs::read(&song.file_path).expect("读原文件");
        let service = env.service(vec![plugin(&env, "p79", &body_hit("0.79", "{\"title\":\"Below\"}"))]);

        let outcome = service.scrape_song(song.id).expect("刮削");
        assert!(matches!(outcome, ScrapeOutcome::Failed { .. }), "0.79 单独出现必须判失败");

        let conn = env.conn();
        let row = songs::get(&conn, song.id, false).expect("查").expect("行");
        assert_eq!(row.scrape_status, ScrapeStatus::Failed);
        assert!(row.scrape_error.as_deref().unwrap_or("").contains("0.79"));
        assert_eq!(row.title, song.title, "未命中不能改 DB 标签");

        let after = std::fs::read(&song.file_path).expect("读文件");
        assert_eq!(before, after, "未命中绝不能碰文件");
    }

    // ── 3. 未命中 → 下一个 ─────────────────────────────────────────────────

    #[test]
    fn plugin_error_falls_through_to_the_next_plugin() {
        let env = Env::new("scrape-error-fallback");
        let song = env.seed_one();
        let service = env.service(vec![
            plugin(&env, "garbage", &body_garbage()),
            plugin(&env, "notfound", &body_error("NOT_FOUND")),
            plugin(&env, "third", &body_hit("0.95", "{\"title\":\"Third\"}")),
        ]);

        let outcome = service.scrape_song(song.id).expect("刮削");
        match &outcome {
            ScrapeOutcome::Done { plugin, .. } => assert_eq!(plugin, "third"),
            other => panic!("协议胡话与插件报错都必须回退，实际 {other:?}"),
        }
        assert_eq!(calls(&env, "garbage"), 1);
        assert_eq!(calls(&env, "notfound"), 1);
        assert_eq!(calls(&env, "third"), 1);
    }

    // ── 4. 全失败 → failed，且不碰文件 ─────────────────────────────────────

    #[test]
    fn all_plugins_miss_marks_failed_without_touching_the_file() {
        let env = Env::new("scrape-all-miss");
        let song = env.seed_one();
        let before = std::fs::read(&song.file_path).expect("读原文件");
        let service = env.service(vec![
            plugin(&env, "notfound", &body_error("NOT_FOUND")),
            plugin(&env, "low", &body_confidence("0.30")),
        ]);

        let outcome = service.scrape_song(song.id).expect("刮削");
        let error = match outcome {
            ScrapeOutcome::Failed { error, .. } => error,
            other => panic!("全部插件未命中必须判失败，实际 {other:?}"),
        };
        assert!(!error.is_empty(), "scrape_error 必须非空");
        assert!(
            error.contains("notfound") && error.contains("low"),
            "错误里要点出每个插件的原因：{error}"
        );

        let conn = env.conn();
        let row = songs::get(&conn, song.id, false).expect("查").expect("行");
        assert_eq!(row.scrape_status, ScrapeStatus::Failed);
        assert_eq!(row.scrape_error.as_deref(), Some(error.as_str()));
        assert_eq!(row.title, song.title, "未命中不能改 DB 标签");

        let after = std::fs::read(&song.file_path).expect("读文件");
        assert_eq!(before, after, "全失败绝不能把文件的标签改坏");
    }

    // ── 5. RATE_LIMITED → 冷却 60 秒 ───────────────────────────────────────

    #[test]
    fn rate_limited_plugin_is_skipped_until_the_cooldown_expires() {
        let env = Env::new("scrape-cooldown");
        let songs = env.seed(3);
        let service = env.service(vec![
            plugin(&env, "limited", &body_error("RATE_LIMITED")),
            plugin(&env, "fallback", &body_hit("0.90", "{\"title\":\"Fallback\"}")),
        ]);

        // 第一次：限流插件被调用并记冷却，回退到 fallback 命中。
        assert!(service.scrape_song(songs[0].id).expect("第一次").is_done());
        assert_eq!(calls(&env, "limited"), 1);
        assert_eq!(calls(&env, "fallback"), 1);

        // 冷却期内：被限流的插件**不再被调用**（不傻等、也不重试），直接跳下一个。
        assert!(service.scrape_song(songs[1].id).expect("第二次").is_done());
        assert_eq!(calls(&env, "limited"), 1, "冷却期内不该再调用被限流的插件");
        assert_eq!(calls(&env, "fallback"), 2);

        // 冷却到期（假时钟推进 60 秒）→ 恢复调用。
        env.clock.advance(Duration::from_secs(60));
        assert!(service.scrape_song(songs[2].id).expect("第三次").is_done());
        assert_eq!(calls(&env, "limited"), 2, "冷却到期必须恢复调用");
        assert_eq!(calls(&env, "fallback"), 3);
    }

    // ── 6. 歌词只入 DB，不落文件 ───────────────────────────────────────────

    #[test]
    fn lyrics_go_to_the_database_only() {
        let env = Env::new("scrape-lyrics");
        let song = env.seed_one();
        let before = read_tags(Path::new(&song.file_path)).expect("读原标签");
        assert!(before.lyrics.is_none(), "样本本身不该带歌词，否则本测试没意义");

        let body = r#"lyrics='第一行\n第二行'
  printf '{"id":"%s","protocol":1,"action":"scrape","ok":true,"confidence":0.95,"tags":{"title":"Scraped Title"},"lyrics":"%s"}\n' "$id" "$lyrics""#;
        let service = env.service(vec![plugin(&env, "lyric", body)]);
        assert!(service.scrape_song(song.id).expect("刮削").is_done());

        let conn = env.conn();
        let row = songs::get(&conn, song.id, false).expect("查").expect("行");
        assert_eq!(
            row.lyrics.as_deref(),
            Some("第一行\n第二行"),
            "歌词必须进 songs.lyrics"
        );

        let after = read_tags(Path::new(&song.file_path)).expect("读回文件标签");
        assert_eq!(after.title.as_deref(), Some("Scraped Title"), "标签确实写回了文件");
        assert!(after.lyrics.is_none(), "歌词绝不能写进文件");
        assert!(after.lyrics_timed.is_none(), "带时间戳的歌词同样不能写进文件");
    }

    // ── 7. processing 真落库 + 重复触发被挡 ────────────────────────────────

    #[test]
    fn processing_is_persisted_and_blocks_a_second_trigger() {
        let env = Env::new("scrape-processing");
        let song = env.seed_one();
        let release = env.work.join("release");
        let body = r#"while [ ! -f 'RELEASE_PATH' ]; do sleep 0.05; done
  printf '{"id":"%s","protocol":1,"action":"scrape","ok":true,"confidence":0.9,"tags":{"title":"After Hold"}}\n' "$id""#
            .replace("RELEASE_PATH", &release.display().to_string());
        let service = env.service(vec![plugin(&env, "hold", &body)]);
        let worker_service = Arc::clone(&service);
        let song_id = song.id;
        let handle = std::thread::spawn(move || worker_service.scrape_song(song_id));

        // 轮询：processing 必须**真的**写进库（并发去重全靠它），并且插件确实已经开始
        // 处理这一次刮削。两件事都要等到 —— 占位发生在调用插件之前，只等状态会有竞态。
        let mut seen_processing = false;
        let mut plugin_started = false;
        for _ in 0..200 {
            {
                let conn = env.conn();
                if let Some(row) = songs::get(&conn, song_id, false).expect("查") {
                    if row.scrape_status == ScrapeStatus::Processing {
                        seen_processing = true;
                    }
                }
            }
            if calls(&env, "hold") >= 1 {
                plugin_started = true;
            }
            if seen_processing && plugin_started {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(seen_processing, "scrape_status = processing 必须真的落库");
        assert!(plugin_started, "这一次刮削必须已经进入插件调用（插件正被 hold 住）");

        // 第二次触发被中间态挡下：一次插件调用都不会发生。
        let second = service.scrape_song(song_id).expect("第二次触发");
        assert_eq!(second, ScrapeOutcome::SkippedBusy { id: song_id });
        assert_eq!(calls(&env, "hold"), 1, "重复触发不能再调用插件");

        std::fs::write(&release, b"go").expect("放行插件");
        let first = handle.join().expect("等工作线程");
        assert!(first.expect("第一次结果").is_done());
        assert_eq!(calls(&env, "hold"), 1, "整个流程只该有一次插件调用");
    }

    // ── 8. 失败不自动重试；批量失败项重刮 ──────────────────────────────────

    #[test]
    fn failed_songs_are_not_retried_until_explicitly_resubmitted() {
        let env = Env::new("scrape-no-retry");
        let song = env.seed_one();
        let service = env.service(vec![plugin(&env, "notfound", &body_error("NOT_FOUND"))]);
        assert!(matches!(
            service.scrape_song(song.id).expect("首次刮削"),
            ScrapeOutcome::Failed { .. }
        ));

        let runner = BatchRunner::new(Arc::clone(&service));
        let batch = runner.run(true).expect("普通批次");
        assert_eq!(batch.total, 0, "failed 的歌不该出现在 pending 队列里");
        assert_eq!(calls(&env, "notfound"), 1, "失败不自动重试：不该有第二次调用");

        let retry = runner.run_failed(true).expect("重刮失败项");
        assert_eq!(retry.total, 1, "批量失败项重刮必须取到这首歌");
        assert_eq!(retry.failed, 1);
        assert_eq!(calls(&env, "notfound"), 2, "显式重新提交才允许再次调用插件");
    }

    // ── 9. 批量：队列就是 songs 表 ─────────────────────────────────────────

    #[test]
    fn batch_runner_processes_pending_songs_and_reports_progress() {
        let env = Env::new("scrape-batch");
        let songs = env.seed(2);
        let service = env.service(vec![plugin(&env, "hit", &body_hit("0.90", "{\"title\":\"Batch\"}"))]);
        let runner = BatchRunner::new(Arc::clone(&service));

        let report = runner.run(true).expect("批次");
        assert_eq!(report.total, 2);
        assert_eq!(report.done, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(
            runner.progress(),
            ProgressSnapshot { total: 2, done: 2, failed: 0, skipped: 0 }
        );
        assert!(report.to_string().contains("刮削批次完成"));

        let conn = env.conn();
        for song in &songs {
            let row = songs::get(&conn, song.id, false).expect("查").expect("行");
            assert_eq!(row.scrape_status, ScrapeStatus::Done);
        }
        assert_eq!(runner.run(true).expect("空批次").total, 0, "跑完 pending 队列就空了");
    }

    // ── 10. 缺省 = 不修改，null = 清除 ─────────────────────────────────────

    #[test]
    fn absent_fields_are_untouched_and_null_clears() {
        let env = Env::new("scrape-tri-state");
        let song = env.seed_one();
        assert!(song.title.is_some(), "样本必须有标题，否则测不出「清空」");
        let (before_album, before_year) = (song.album_id, song.year);

        let service = env.service(vec![plugin(
            &env,
            "tristate",
            &body_hit("0.95", "{\"title\":null,\"artist\":\"New Artist\"}"),
        )]);
        assert!(service.scrape_song(song.id).expect("刮削").is_done());

        let conn = env.conn();
        let row = songs::get(&conn, song.id, false).expect("查").expect("行");
        assert_eq!(row.title, None, "显式 null 必须清空标题");
        assert_eq!(row.artists.as_deref(), Some("New Artist"), "有值的字段必须写入");
        assert_eq!(row.album_id, before_album, "缺省字段必须原样不动（专辑）");
        assert_eq!(row.year, before_year, "缺省字段必须原样不动（年份）");

        let after = read_tags(Path::new(&song.file_path)).expect("读回文件标签");
        assert!(
            after.title.unwrap_or_default().is_empty(),
            "文件里的标题也要被清掉"
        );
        assert_eq!(after.artists, vec!["New Artist".to_string()]);
    }

    // ── 11. 封面：校验通过才写进文件 ───────────────────────────────────────

    #[test]
    fn cover_is_validated_and_written_into_the_file() {
        let env = Env::new("scrape-cover");
        let song = env.seed_one();
        let body = r#"printf 'FAKEJPEGBYTES' > "$wd/cover.jpg"
  printf '{"id":"%s","protocol":1,"action":"scrape","ok":true,"confidence":0.95,"tags":{"title":"With Cover"},"cover":{"path":"cover.jpg","mime":"image/jpeg"}}\n' "$id""#;
        let service = env.service(vec![plugin(&env, "cover", body)]);
        assert!(service.scrape_song(song.id).expect("刮削").is_done());

        let after = read_tags(Path::new(&song.file_path)).expect("读回文件标签");
        assert_eq!(after.pictures.len(), 1, "封面必须写进文件");
        assert_eq!(after.pictures[0].data, b"FAKEJPEGBYTES");
        assert_eq!(after.pictures[0].mime_type, "image/jpeg");
        assert_eq!(after.title.as_deref(), Some("With Cover"));
    }

    #[test]
    fn unusable_cover_falls_back_instead_of_stuffing_a_bad_path() {
        let env = Env::new("scrape-bad-cover");
        let song = env.seed_one();
        let before = read_tags(Path::new(&song.file_path)).expect("读原标签");
        let service = env.service(vec![
            // 绝对路径：协议层 validate_cover_path 直接拒（decode_response 判协议错误）
            plugin(&env, "absolute", &body_cover("/etc/passwd", "0.99")),
            // 合法相对路径但文件不存在：服务层 PathSandbox 拒绝
            plugin(&env, "missing", &body_cover("missing.jpg", "0.98")),
            plugin(&env, "good", &body_hit("0.90", "{\"title\":\"Fallback\"}")),
        ]);

        let outcome = service.scrape_song(song.id).expect("刮削");
        match &outcome {
            ScrapeOutcome::Done { plugin, .. } => assert_eq!(plugin, "good"),
            other => panic!("封面不可用必须回退到下一个插件，实际 {other:?}"),
        }
        assert_eq!(calls(&env, "absolute"), 1);
        assert_eq!(calls(&env, "missing"), 1);
        assert_eq!(calls(&env, "good"), 1);

        let after = read_tags(Path::new(&song.file_path)).expect("读回文件标签");
        assert_eq!(
            after.pictures.len(),
            before.pictures.len(),
            "不可用的封面绝不能硬塞进文件，原有的封面也不能被动"
        );
    }

    // ── 12. 错误与报告的文案 ───────────────────────────────────────────────

    #[test]
    fn errors_and_reports_display_are_chinese() {
        let cases = [
            ScrapeError::Pool(DbPoolError::Closed),
            ScrapeError::Repo(RepoError::Conflict { constraint: "songs.file_path".to_string() }),
            ScrapeError::SongNotFound { id: 7 },
        ];
        for e in &cases {
            let text = e.to_string();
            assert!(!text.is_empty());
            assert!(text.contains("刮削"), "错误消息应为中文并提到刮削：{text}");
        }
        assert!(std::error::Error::source(&cases[0]).is_some(), "Pool 应暴露底层错误");
        assert!(std::error::Error::source(&cases[1]).is_some(), "Repo 应暴露底层错误");
        assert!(std::error::Error::source(&cases[2]).is_none());

        let report = BatchReport {
            total: 3,
            done: 1,
            failed: 1,
            skipped: 1,
            issues: vec![ScrapeIssue { song_id: 1, message: "插件全都未命中".to_string() }],
        };
        assert!(report.to_string().contains("刮削批次完成"));

        let done = ScrapeOutcome::Done { id: 4, plugin: "p".to_string(), confidence: 0.9 };
        assert_eq!(done.song_id(), 4);
        assert!(done.is_done());
        assert!(done.to_string().contains("刮削命中"));
        assert!(ScrapeOutcome::Failed { id: 4, error: "x".to_string() }
            .to_string()
            .contains("刮削失败"));
        assert!(ScrapeOutcome::SkippedBusy { id: 4 }.to_string().contains("刮削跳过"));

        let _boxed: Box<dyn std::error::Error> = Box::new(ScrapeError::SongNotFound { id: 1 });
    }

    /// 日志里那句「改动 N 处」的判定逻辑。这条是**唯一**能事后还原「插件改了什么」的地方
    /// （文件已被 rename 覆盖、DB 旧值已被 UPDATE），所以逐个分支钉住。
    #[test]
    fn snapshot_changes_lists_only_real_changes() {
        let before = TagSnapshot {
            title: Some("老男孩".to_string()),
            artists: Some("公众号：阿乐资源库".to_string()),
            genres: None,
            year: Some(2011),
            track: None,
            album_id: Some(7),
        };
        // 只有歌手 + 专辑真的变了；标题/风格/年份/轨道原样。
        let after = TagSnapshot {
            title: Some("老男孩".to_string()),
            artists: Some("示例歌手".to_string()),
            genres: None,
            year: Some(2011),
            track: None,
            album_id: Some(9),
        };
        let changes = before.changes(
            &after,
            Some("2015江苏卫视新年演唱会".to_string()),
            Some("示例专辑".to_string()),
            vec![],
        );
        assert_eq!(
            changes,
            vec![
                "歌手「公众号：阿乐资源库」→「示例歌手」".to_string(),
                "专辑「2015江苏卫视新年演唱会」→「示例专辑」".to_string(),
            ],
            "只该报歌手与专辑，没变的字段一个都不许出现"
        );

        // 一个字段都没变 → 空 Vec（调用方据此说「无字段变化」，而不是打一行空改动）
        let same = before.clone();
        assert!(before
            .changes(&same, Some("同名".to_string()), Some("同名".to_string()), vec![])
            .is_empty());

        // 清空（Some → None）与填充（None → Some）都要报，且空值渲染成「(空)」
        let cleared = TagSnapshot { artists: None, ..after.clone() };
        assert_eq!(
            after.changes(&cleared, None, None, vec![]),
            vec!["歌手「示例歌手」→「(空)」".to_string()]
        );

        // 专辑「id 变了但名字没变」不报 —— 对用户来说等于没改
        let renamed_id = TagSnapshot { album_id: Some(99), ..before.clone() };
        assert!(before
            .changes(&renamed_id, Some("同名".to_string()), Some("同名".to_string()), vec![])
            .is_empty());

        // extra（歌词那类非字段改动）原样追加在后面
        let with_extra =
            before.changes(&same, None, None, vec!["歌词「已清空」".to_string()]);
        assert_eq!(with_extra, vec!["歌词「已清空」".to_string()]);

        // 空串也算「空」：插件回了个空字符串，日志不该打出「歌手「」→「x」」
        assert_eq!(show(Some("")), "(空)");
        assert_eq!(show(Some("  ")), "(空)");
        assert_eq!(show(Some("白兀")), "白兀");
        assert_eq!(show(None), "(空)");
    }

    /// 专辑名解析失败**不能**让刮削失败 —— 它只是日志的装饰。
    #[test]
    fn album_name_is_none_when_missing() {
        let env = Env::new("scrape-album-name");
        let conn = env.conn();
        assert_eq!(album_name(&conn, None), None, "没专辑就是 None，不该去查库");
        assert_eq!(album_name(&conn, Some(999_999)), None, "查不到的 id 不该报错");
    }

    // ─────────────────── 只入库不写文件（write_files = false）───────────────────

    /// 「只入库」必须**一个字节都不动**原文件，但 DB 要更新。
    ///
    /// 这个档位是用户明确要求的：刮削不可撤销（原文件被 rename 覆盖、DB 旧值被 UPDATE），
    /// 所以界面上要能选「只入库 / 也写文件」。
    #[test]
    fn dry_run_updates_the_database_and_leaves_the_file_byte_identical() {
        let env = Env::new("scrape-dry-run");
        let song = env.seed_one();
        let service = env.service(vec![plugin(
            &env,
            "dryhit",
            &body_hit("0.95", "{\"title\":\"Dry Run Title\",\"artist\":\"Dry Artist\"}"),
        )]);

        let path = PathBuf::from(&song.file_path);
        let before = std::fs::read(&path).expect("读原文件");

        let outcome = service.scrape_song_with(song.id, false).expect("刮削");
        assert!(outcome.is_done(), "只入库也算成功：{outcome:?}");

        let after = std::fs::read(&path).expect("再读原文件");
        assert_eq!(before, after, "只入库模式**绝不能**改动原文件 —— 一个字节都不行");

        // 但 DB 必须更新，否则这个档位毫无意义
        let conn = env.conn();
        let row = songs::get(&conn, song.id, false).expect("查库").expect("有这行");
        assert_eq!(row.scrape_status, ScrapeStatus::Done);
        assert_eq!(row.title.as_deref(), Some("Dry Run Title"), "标签应当进了库");
        assert_eq!(row.artists.as_deref(), Some("Dry Artist"));
    }

    /// 对照组：默认模式必须**真的**写回文件。
    ///
    /// ⚠️ 少了这一条，上面那条测试可能因为「写文件功能整体坏掉」而假通过 ——
    /// 一条只断言「没变」的测试，证明不了「该变的时候会变」。
    #[test]
    fn write_files_true_still_rewrites_the_file() {
        let env = Env::new("scrape-write-files");
        let song = env.seed_one();
        let service = env.service(vec![plugin(
            &env,
            "realhit",
            &body_hit("0.95", "{\"title\":\"Written Title\",\"artist\":\"Written Artist\"}"),
        )]);

        let path = PathBuf::from(&song.file_path);
        let before = std::fs::read(&path).expect("读原文件");

        let outcome = service.scrape_song_with(song.id, true).expect("刮削");
        assert!(outcome.is_done(), "{outcome:?}");

        let after = std::fs::read(&path).expect("再读原文件");
        assert_ne!(before, after, "默认模式必须真的写回文件，否则上面那条测试证明不了任何事");

        // 写进去的确实是插件给的标签（不是「文件变了但内容不对」）
        let tags = crate::tag::read::read_tags(&path).expect("重新读标签");
        assert_eq!(tags.title.as_deref(), Some("Written Title"));
    }
}
