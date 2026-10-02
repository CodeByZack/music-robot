//! S5 · 扫描编排 —— LibraryService::scan：把「遍历 / 读标签 / 落库」串成一轮扫描。
//!
//! ## 画布区块 ③ 的判定规则（逐条对应到实现）
//!
//! | 画布规则 | 实现位置 |
//! |---|---|
//! | 新文件 → read_tags → INSERT (pending) | apply_file 的 None 分支 |
//! | 每首歌 upsert albums（name + album_artist 唯一） | upsert_album |
//! | 已存在 → mtime / size 有变化 → 重读 → UPDATE | apply_file 的 Some 分支 |
//! | 已存在 → 无变化 → 跳过 | 同上，unchanged 提前返回 FileOutcome::Skipped |
//! | 磁盘消失 → 标记删除 | prune_missing |
//! | 读标签失败 → 记录错误，跳过 | StepFailure::Tags → ScanReport::tag_failed / issues |
//! | 多曲库重复（audio_hash 相同）→ 合并，保留音质最好的 | **S6**：apply_file 的 None 分支（判重 + 原地改写，见下方「S6 去重」）|
//!
//! ## 最重要的安全不变式（做错会毁掉整个曲库）
//!
//! **扫描结果为空 / 任一库根读取失败 / 遍历不完整 → 本轮整轮绝不触发「标记删除」。**
//!
//! 移动硬盘没挂载、NFS 抖动、权限配错，都会让「数据库里有 5000 首、这轮扫到 0 首」。
//! 天真地按「没扫到 = 磁盘消失」标记删除，就是把用户整个曲库一次清空。
//! 判据复用 scanner 的 WalkStats::is_suspicious / may_prune：
//!   · 根目录没打开（ScanError::RootNotDir / RootUnreadable）→ 可疑；
//!   · 有子目录读不开（树不完整）→ 可疑；
//!   · files_seen == 0（挂载闪断与真空目录无法区分）→ 可疑。
//! 本轮只要有**任何一个**库根可疑，就整轮跳过 prune（宁可漏删，不可误删），
//! 并在 ScanReport::prune_skipped 里带回原因。
//!
//! ## 事务粒度
//!
//! 一首歌的「upsert 专辑 + 写 songs」在同一个事务里提交；**不做**整轮大事务
//! （几千首歌会长时间持库锁）。prune 也是每批（500 行）一个事务。
//!
//! ## 软删除
//!
//! 磁盘上文件消失只调 repos::songs::mark_deleted 写 deleted_at，**绝不**用 purge
//! （物理删会按外键级联清掉用户的歌单条目 / 收藏 / 播放历史）。文件重新出现时走
//! restore。因此这里刻意用 find_by_file_path —— 它不过滤软删行，正是为了能找回
//! 那一行去 restore，而不是当新歌重插（重插会撞 file_path 唯一约束）。
//!
//! 【偏离说明】遍历用 AudioWalker（只产出路径）而不是 ScanIter（产出 ScanEntry）：
//! inspect_file 已经读过一次标签、还把整个文件读进内存算 warnings，
//! 扫描编排只需要路径，标签由本模块按 mtime / size 决定要不要读，避免重复解析。
//!
//! ## audio_hash（跨步骤依赖：S6 去重 / S19 转码缓存的地基）
//!
//! read_tags 返回的 AudioMetadata 里**没有** hash 字段，所以入库时由本模块按扩展名
//! 选 tag::write 的裸音频 sha256 现算并写入 songs.audio_hash。代价是每个新 / 变更
//! 文件会被读两遍（read_tags 一遍、算 hash 一遍）；这个代价暂时接受，优化方向写在
//! compute_audio_hash 上方的注释里。
//!
//! ## S6 去重（多目录里的同一份音频只留一条记录）
//!
//! 判重键就是 audio_hash（与标签无关的裸音频 sha256）。按 file_path 查不到时：
//!
//!   * 哈希为 None / 空串 → **照常插入**（没有哈希就判不了重，不能因此丢文件）；
//!   * 否则用 find_by_audio_hash(.., include_deleted = false) 找在库的同曲
//!     （软删行不算候选 —— 已经删除的歌不该挡住新文件入库）：
//!       - 没找到 → 照常插入；
//!       - 已有记录胜 → 一行都不写，计入 ScanReport::deduped；
//!       - 新文件胜 → **原地改写**已有行（绝不删旧插新），计入 updated。
//!
//! 胜负规则完全确定：bitrate_bps 高者胜 → added_at 小者胜 → id 小者胜。
//! 新文件的 added_at 是「现在」、id 只会比已有行大，所以平局一律判已有记录胜
//! （先入库者胜）—— 这让结果可复现，连扫多遍也不会抖动。
//!
//! 为什么必须「原地改写」而不是「删旧插新」：playlist_items / favorites /
//! play_history 都有外键指向 songs.id 且是 ON DELETE CASCADE，删掉旧行会把用户的
//! 歌单条目、收藏、播放历史一起带走，不可逆。原地改写让 song_id 保持不变，
//! 所有引用自动继续有效。

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use crate::config::StorageConfig;
use crate::db::models::{Album, ScrapeStatus, Song};
use crate::db::pool::{DbPool, DbPoolError};
use crate::db::repos::{albums, songs, RepoError};
use crate::scanner::{AudioWalker, WalkStats};
use crate::tag::read::{read_tags, AudioMetadata};

// ─────────────────────────────────────────────────────────────────────────────
// 错误类型
// ─────────────────────────────────────────────────────────────────────────────

/// 扫描编排层的错误。单文件的读标签 / 读元信息失败**不算**这里的错误 ——
/// 那些是「跳过并记一笔」的正常分支，记在 ScanReport 里；这里只报让整轮无法
/// 继续的故障（没配库根、借不到连接、数据库读写失败）。
#[derive(Debug)]
pub enum LibraryError {
    /// 没有配置任何曲库根（空列表 / 全是空白串）
    NoRoots,
    /// 从连接池借连接失败
    Pool(DbPoolError),
    /// 数据库读写失败
    Repo(RepoError),
}

impl fmt::Display for LibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LibraryError::NoRoots => write!(f, "没有配置任何曲库根，无法扫描"),
            LibraryError::Pool(e) => write!(f, "扫描时借数据库连接失败：{e}"),
            LibraryError::Repo(e) => write!(f, "扫描时数据库读写失败：{e}"),
        }
    }
}

impl std::error::Error for LibraryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LibraryError::NoRoots => None,
            LibraryError::Pool(e) => Some(e),
            LibraryError::Repo(e) => Some(e),
        }
    }
}

impl From<DbPoolError> for LibraryError {
    fn from(e: DbPoolError) -> Self {
        LibraryError::Pool(e)
    }
}

impl From<RepoError> for LibraryError {
    fn from(e: RepoError) -> Self {
        LibraryError::Repo(e)
    }
}

/// 裸 rusqlite 错误（事务 begin / commit 这类不走 repo 的调用）按同一口径归类。
impl From<rusqlite::Error> for LibraryError {
    fn from(e: rusqlite::Error) -> Self {
        LibraryError::Repo(e.into())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 结果结构
// ─────────────────────────────────────────────────────────────────────────────

/// 单个文件的异常记录（读标签失败、读元信息失败、保守不标记删除等）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanIssue {
    /// 出问题的文件路径
    pub path: String,
    /// 中文说明，可直接进日志
    pub message: String,
}

/// 一轮扫描的统计回报。
///
/// 计数之间不要求互斥，但各自语义明确：
///   · added / updated / skipped / deduped 描述「磁盘上的文件怎么处理了」，四者互斥
///     （deduped = 同一份音频已有更好的记录在库，本轮一行都没写）；
///   · restored 是「软删行因文件回来而复原」的次数，可以是 updated 的一部分
///     （文件回来且 mtime / size 也变了 → updated + 1 且 restored + 1）；
///   · marked_deleted 是本轮新标记软删的行数（幂等，已经是删除态的不重复计）；
///   · tag_failed / io_failed / db_failed 是三类跳过原因，互斥；
///   · hash_missing 是「已入库、但 audio_hash 只能写 NULL」的可观测计数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// 实际尝试遍历的库根数（含读取失败的）
    pub roots_scanned: usize,
    /// 遍历器产出的音频文件数
    pub files_seen: usize,
    /// 新入库（scrape_status = pending）
    pub added: usize,
    /// 同一份音频（audio_hash 相同）已在库、且已有记录音质更好 → 跳过，未写任何行
    pub deduped: usize,
    /// 重读标签并覆盖已有行；「新文件音质更好 → 原地改写已有行」也计在这里
    pub updated: usize,
    /// 软删行因文件重新出现而复原
    pub restored: usize,
    /// mtime 与 size 都没变，直接跳过
    pub skipped: usize,
    /// 本轮新标记为「磁盘上已消失」
    pub marked_deleted: usize,
    /// 读标签失败而被跳过的文件数
    pub tag_failed: usize,
    /// 读文件元信息失败而被跳过的文件数
    pub io_failed: usize,
    /// 数据库读写失败而被跳过的文件数
    pub db_failed: usize,
    /// 已入库但没能算出 audio_hash 的文件数（不认识的格式 / 读文件失败）
    pub hash_missing: usize,
    /// 库根级异常（不存在 / 不可读），中文描述
    pub root_errors: Vec<String>,
    /// 单文件级异常明细
    pub issues: Vec<ScanIssue>,
    /// None = 本轮允许标记删除；Some(原因) = 本轮整轮跳过标记删除及其原因
    pub prune_skipped: Option<String>,
}

impl ScanReport {
    /// 是否出现了任何异常（库根级或单文件级）。
    pub fn has_issues(&self) -> bool {
        !self.root_errors.is_empty() || !self.issues.is_empty()
    }
}

impl fmt::Display for ScanReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "扫描完成：遍历 {} 个文件，新增 {}，去重跳过 {}，更新 {}（其中复原 {}），跳过 {}，标记删除 {}，读标签失败 {}，元信息失败 {}，数据库失败 {}，未算出音频哈希 {}",
            self.files_seen,
            self.added,
            self.deduped,
            self.updated,
            self.restored,
            self.skipped,
            self.marked_deleted,
            self.tag_failed,
            self.io_failed,
            self.db_failed,
            self.hash_missing,
        )?;
        if let Some(reason) = &self.prune_skipped {
            write!(f, "；本轮跳过标记删除：{reason}")?;
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 服务
// ─────────────────────────────────────────────────────────────────────────────

/// 曲库服务：目前只有一轮扫描编排。
///
/// 持有一个连接池句柄与若干个曲库根；每次 scan 借一条连接串完整个流程
/// （单线程顺序扫描，不做并发 —— SQLite 写本来就是串行的）。
pub struct LibraryService {
    pool: Arc<DbPool>,
    roots: Vec<PathBuf>,
}

impl LibraryService {
    /// 用库根字符串构造（与 config 的 storage.library_roots 同形）。
    /// 空白项会被丢掉；一个都不剩时 scan 返回 LibraryError::NoRoots。
    pub fn new(pool: Arc<DbPool>, roots: Vec<String>) -> LibraryService {
        let roots = roots
            .into_iter()
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .map(PathBuf::from)
            .collect();
        LibraryService { pool, roots }
    }

    /// 从 storage 配置段构造。
    pub fn from_config(pool: Arc<DbPool>, cfg: &StorageConfig) -> LibraryService {
        LibraryService::new(pool, cfg.library_roots.clone())
    }

    /// 生效的库根列表。
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// 跑一轮扫描。
    ///
    /// 流程：遍历每个库根 → 逐个音频文件与库里比对（新增 / 更新 / 跳过）→
    /// 一个点都不漏地走完后，只有在「扫描可信」时才把磁盘上已消失的行标记软删。
    pub fn scan(&self) -> Result<ScanReport, LibraryError> {
        if self.roots.is_empty() {
            return Err(LibraryError::NoRoots);
        }

        let mut conn = self.pool.acquire()?;
        let mut report = ScanReport::default();
        // 收集「本轮不可信」的理由：只要非空，整轮不碰标记删除
        let mut prune_blockers: Vec<String> = Vec::new();

        for root in &self.roots {
            report.roots_scanned += 1;

            let mut walker = match AudioWalker::new(root) {
                Ok(w) => w,
                Err(e) => {
                    // 根目录不存在 / 不是目录 / 打不开：这里绝不退化成「空结果」
                    report.root_errors.push(e.to_string());
                    prune_blockers.push(format!("库根 {} 无法读取", root.display()));
                    continue;
                }
            };

            for path in walker.by_ref() {
                report.files_seen += 1;
                match apply_file(&mut conn, &path) {
                    Ok(FileOutcome::Added { hash_missing }) => {
                        report.added += 1;
                        record_hash_issue(&mut report, &path, hash_missing);
                    }
                    Ok(FileOutcome::Updated { restored, hash_missing }) => {
                        report.updated += 1;
                        if restored {
                            report.restored += 1;
                        }
                        record_hash_issue(&mut report, &path, hash_missing);
                    }
                    Ok(FileOutcome::Restored { hash_missing }) => {
                        report.restored += 1;
                        record_hash_issue(&mut report, &path, hash_missing);
                    }
                    Ok(FileOutcome::Deduped) => report.deduped += 1,
                    Ok(FileOutcome::Skipped) => report.skipped += 1,
                    Err(failure) => {
                        match &failure {
                            StepFailure::Io(_) => report.io_failed += 1,
                            StepFailure::Tags(_) => report.tag_failed += 1,
                            StepFailure::Db(_) => report.db_failed += 1,
                        }
                        report.issues.push(ScanIssue {
                            path: path.display().to_string(),
                            message: failure.message().to_string(),
                        });
                    }
                }
            }

            // 迭代必须跑到底，files_seen 才是完整值（WalkStats 的惰性语义）
            let stats = walker.stats();
            if !stats.may_prune() {
                prune_blockers.push(describe_suspicion(root, stats));
            }
        }

        if prune_blockers.is_empty() {
            prune_missing(&mut conn, &mut report)?;
        } else {
            report.prune_skipped = Some(prune_blockers.join("；"));
        }

        Ok(report)
    }
}

/// 记一笔「入库了但没算出 audio_hash」：计数 + 明细，绝不因此丢掉这个文件。
fn record_hash_issue(report: &mut ScanReport, path: &Path, hash_missing: Option<String>) {
    if let Some(message) = hash_missing {
        report.hash_missing += 1;
        report.issues.push(ScanIssue {
            path: path.display().to_string(),
            message,
        });
    }
}

/// 把一个库根判定为「不可信」的原因写成人话（日志 / 报告要能直接看懂）。
fn describe_suspicion(root: &Path, stats: WalkStats) -> String {
    if !stats.root_ok {
        format!("库根 {} 未成功打开", root.display())
    } else if stats.dirs_read_failed > 0 {
        format!(
            "库根 {} 有 {} 个子目录读不开，遍历结果不完整",
            root.display(),
            stats.dirs_read_failed
        )
    } else {
        format!(
            "库根 {} 本轮零命中（空库与掉盘无法区分）",
            root.display()
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 单文件处理
// ─────────────────────────────────────────────────────────────────────────────

/// 一个文件处理完的归类。
#[derive(Debug, Clone, PartialEq, Eq)]
enum FileOutcome {
    /// 新入库
    Added {
        /// Some(原因) = 行已写入，但 audio_hash 只能写 NULL
        hash_missing: Option<String>,
    },
    /// 重读标签并覆盖
    Updated {
        /// 这一行原本是软删状态，顺带复原了
        restored: bool,
        /// Some(原因) = 行已写入，但 audio_hash 只能写 NULL
        hash_missing: Option<String>,
    },
    /// 同一份音频已有更好的记录在库 → 一行都不写，直接跳过（S6 去重判负）
    Deduped,
    /// 内容没变，但原本是软删行，只做复原
    Restored {
        /// Some(原因) = 行已写入，但 audio_hash 只能写 NULL
        hash_missing: Option<String>,
    },
    /// mtime 与 size 都没变，跳过
    Skipped,
}

/// 单文件失败分类，直接决定往哪个计数里加一。
#[derive(Debug)]
enum StepFailure {
    /// 读文件元信息失败 / 目标不是普通文件
    Io(String),
    /// 读标签失败
    Tags(String),
    /// 数据库读写失败
    Db(String),
}

impl StepFailure {
    fn message(&self) -> &str {
        match self {
            StepFailure::Io(m) | StepFailure::Tags(m) | StepFailure::Db(m) => m,
        }
    }
}

/// 处理一个磁盘上的音频文件。
///
/// 无论哪条分支，只要写库就是「upsert 专辑 + 写 songs」同一个事务。
fn apply_file(conn: &mut Connection, path: &Path) -> Result<FileOutcome, StepFailure> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| StepFailure::Io(format!("读取文件元信息失败：{e}")))?;
    if !metadata.is_file() {
        return Err(StepFailure::Io("目标不是普通文件，跳过".to_string()));
    }
    let file_size = metadata.len() as i64;
    let file_mtime = metadata.modified().ok().and_then(system_time_to_ms);
    let file_path = canonical_path_string(path);

    let existing = songs::find_by_file_path(conn, &file_path)
        .map_err(|e| StepFailure::Db(format!("按路径查询歌曲失败：{e}")))?;

    match existing {
        // 新文件 → read_tags → （S6 判重）→ INSERT 或原地改写
        None => {
            let meta = read_metadata(path)?;
            let (audio_hash, hash_missing) = compute_audio_hash(path);

            // ── S6 去重：只有算出了哈希才能判重 ─────────────────────────
            // 没有哈希（不认识的扩展名 / 读文件失败）时一律直接插入：判不了重
            // 不是丢文件的理由。
            if let Some(hash) = dedup_key(audio_hash.as_deref()) {
                let rivals = songs::find_by_audio_hash(conn, hash, false)
                    .map_err(|e| StepFailure::Db(format!("按音频哈希查询同曲失败：{e}")))?;
                // 只把**在库**的行当候选（include_deleted = false）：已删除的歌
                // 不该挡住新文件入库。
                if let Some(incumbent) = best_existing(&rivals) {
                    // 平局一律判已有记录胜（新文件的 added_at 是「现在」、
                    // id 只会更大），所以同码率的拷贝永远不会顶掉先入库的那份。
                    if !DedupRank::newcomer(meta.bitrate_bps).wins_over(&DedupRank::of(&incumbent)) {
                        return Ok(FileOutcome::Deduped);
                    }
                    // 新文件胜：**原地改写**已有行，绝不删旧插新 ——
                    // 删行会按外键 CASCADE 带走歌单条目 / 收藏 / 播放历史。
                    // 专辑 upsert 与改写必须在同一个事务里。
                    let tx = conn
                        .transaction()
                        .map_err(|e| StepFailure::Db(format!("开启事务失败：{e}")))?;
                    let album_id = upsert_album(&tx, &meta).map_err(repo_failure)?;
                    let song = build_song(
                        incumbent.id,
                        file_path,
                        file_size,
                        file_mtime,
                        &meta,
                        album_id,
                        audio_hash,
                    );
                    repoint_song_to_file(&tx, incumbent.id, &song).map_err(repo_failure)?;
                    tx.commit()
                        .map_err(|e| StepFailure::Db(format!("提交事务失败：{e}")))?;
                    // 记 updated：被改写的是**库里已有的那一行**（song_id 不变、
                    // 歌单/收藏/历史继续有效），不是新插一行。
                    return Ok(FileOutcome::Updated {
                        restored: false,
                        hash_missing,
                    });
                }
            }

            let tx = conn
                .transaction()
                .map_err(|e| StepFailure::Db(format!("开启事务失败：{e}")))?;
            let album_id = upsert_album(&tx, &meta).map_err(repo_failure)?;
            let song = build_song(
                0,
                file_path,
                file_size,
                file_mtime,
                &meta,
                album_id,
                audio_hash,
            );
            songs::insert(&tx, &song).map_err(repo_failure)?;
            tx.commit()
                .map_err(|e| StepFailure::Db(format!("提交事务失败：{e}")))?;
            Ok(FileOutcome::Added { hash_missing })
        }
        // 已存在（含软删行）→ 看 mtime / size 决定重读还是跳过
        Some(row) => {
            let unchanged = row.file_size == Some(file_size) && row.file_mtime == file_mtime;
            if unchanged && row.deleted_at.is_none() {
                return Ok(FileOutcome::Skipped);
            }
            let meta = read_metadata(path)?;
            let (audio_hash, hash_missing) = compute_audio_hash(path);
            let was_deleted = row.deleted_at.is_some();

            let tx = conn
                .transaction()
                .map_err(|e| StepFailure::Db(format!("开启事务失败：{e}")))?;
            let album_id = upsert_album(&tx, &meta).map_err(repo_failure)?;
            let song = build_song(
                row.id,
                file_path,
                file_size,
                file_mtime,
                &meta,
                album_id,
                audio_hash,
            );
            // update_tags 只覆盖标签字段，scrape_status / scrape_error / scrape_at
            // 不在 SET 列表里 —— 元数据变更绝不重置刮削状态（画布明确要求）。
            songs::update_tags(&tx, &song).map_err(repo_failure)?;
            if was_deleted {
                songs::restore(&tx, row.id).map_err(repo_failure)?;
            }
            tx.commit()
                .map_err(|e| StepFailure::Db(format!("提交事务失败：{e}")))?;

            if unchanged {
                Ok(FileOutcome::Restored { hash_missing })
            } else {
                Ok(FileOutcome::Updated {
                    restored: was_deleted,
                    hash_missing,
                })
            }
        }
    }
}

/// 把 repo 错误包成单文件失败（记一笔、跳过这个文件，不拖垮整轮）。
fn repo_failure(e: RepoError) -> StepFailure {
    StepFailure::Db(format!("数据库写入失败：{e}"))
}

/// 读标签；失败归类为 Tags（画布：读标签失败 → 记录错误，跳过）。
fn read_metadata(path: &Path) -> Result<AudioMetadata, StepFailure> {
    read_tags(path).map_err(|e| StepFailure::Tags(format!("读取标签失败：{e}")))
}

/// 计算裸音频 sha256（与标签无关：同一份音频换标签，哈希不变 —— S6 去重的地基）。
///
/// ⚠️ 为了拿 audio_hash，这里把文件读了**第二遍**（read_tags 已经读过一遍）。
/// 将来若扫描 IO 成为瓶颈，应在 tag 读层提供「一次读取同时返回 metadata 与 hash」
/// 的接口，而不是在这里再补一次读取。
///
/// 返回 (哈希, 未算出的原因)：算不出时哈希为 None（入库写 NULL），原因非空，
/// 由调用方记进 ScanReport::hash_missing；三种格式之外一律 NULL，绝不 panic。
fn compute_audio_hash(path: &Path) -> (Option<String>, Option<String>) {
    let ext = match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => ext.to_ascii_lowercase(),
        None => {
            return (
                None,
                Some("文件没有扩展名，无法判断格式，未计算 audio_hash".to_string()),
            )
        }
    };
    let buf = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return (
                None,
                Some(format!("为计算 audio_hash 读取文件失败：{e}")),
            )
        }
    };
    match ext.as_str() {
        "mp3" => (Some(crate::tag::write::audio_hash(&buf)), None),
        "flac" => (Some(crate::tag::write::audio_hash_flac(&buf)), None),
        "wav" => (Some(crate::tag::write::audio_hash_wav(&buf)), None),
        other => (
            None,
            Some(format!(
                "不认识的扩展名 .{other}，未计算 audio_hash（已按 NULL 入库）"
            )),
        ),
    }
}

/// 落库用的文件路径：解析 symlink 并转成绝对路径，保证同一文件在库里只有一种写法。
/// canonicalize 失败（极少数竞态）就退回原始路径 —— 绝不因此丢掉这个文件。
fn canonical_path_string(path: &Path) -> String {
    match std::fs::canonicalize(path) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// SystemTime → Unix 毫秒；早于 1970 时给 None（与 db 的时间戳约定一致）。
fn system_time_to_ms(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => Some(d.as_millis() as i64),
        Err(_) => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// S6 去重：胜负判定与原地改写
// ─────────────────────────────────────────────────────────────────────────────

/// 判重用的哈希：None 与空串一律视为「没有哈希」。
///
/// 空串是脏数据防线 —— 拿空串去 find_by_audio_hash 会命中另一条同样空哈希的行，
/// 把本该入库的文件误判成重复。
fn dedup_key(audio_hash: Option<&str>) -> Option<&str> {
    audio_hash.filter(|h| !h.is_empty())
}

/// 未知码率排在所有已知码率之后 —— 判不出音质就当作最差，
/// 绝不能让 NULL 反而胜出（否则「先入库者胜」的平局规则会被绕过）。
fn bitrate_key(bitrate_bps: Option<i64>) -> i64 {
    bitrate_bps.unwrap_or(i64::MIN)
}

/// 去重胜负键：决定「同一份音频的多份拷贝谁留在库里」。
///
/// 排序规则完全确定（用户拍板）：
///   1. bitrate_bps 高者胜；
///   2. 相同则 added_at 小者胜（先入库者胜）；
///   3. 再相同则 id 小者胜（最终兜底，保证任何情况下结果唯一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DedupRank {
    bitrate_bps: Option<i64>,
    added_at: i64,
    id: i64,
}

impl DedupRank {
    /// 库里已有行的胜负键。
    fn of(song: &Song) -> DedupRank {
        DedupRank {
            bitrate_bps: song.bitrate_bps,
            added_at: song.added_at,
            id: song.id,
        }
    }

    /// 待插入的新文件：它「现在」才入库，added_at 取当前时刻、id 只会比任何已有行大。
    /// 这样平局（含码率相同）自动判已有记录胜 —— 先入库者胜。
    fn newcomer(bitrate_bps: Option<i64>) -> DedupRank {
        DedupRank {
            bitrate_bps,
            added_at: crate::db::now_unix_ms(),
            id: i64::MAX,
        }
    }

    /// self 是否严格优于 other（只有更好才返回 true，平局不算赢）。
    fn wins_over(&self, other: &DedupRank) -> bool {
        let mine = bitrate_key(self.bitrate_bps);
        let theirs = bitrate_key(other.bitrate_bps);
        if mine != theirs {
            return mine > theirs;
        }
        if self.added_at != other.added_at {
            return self.added_at < other.added_at;
        }
        self.id < other.id
    }
}

/// 已有同曲里最该留下的那一条。
///
/// audio_hash 上没有唯一索引，理论上一个哈希可以对应多行；正常流程永远是 0 / 1 行，
/// 但库里若已有历史脏数据，这里按同一套胜负规则挑出第一名，结果依旧确定。
fn best_existing(candidates: &[Song]) -> Option<Song> {
    let mut best: Option<Song> = None;
    for song in candidates {
        best = Some(match best {
            Some(current) if !DedupRank::of(song).wins_over(&DedupRank::of(&current)) => current,
            _ => song.clone(),
        });
    }
    best
}

/// 原地改写：把已有行改成指向新文件（S6「新文件胜」的落库动作）。
///
/// 只改内容，**绝不换 id、绝不删行** —— playlist_items / favorites / play_history
/// 都有外键指向 songs.id 且是 ON DELETE CASCADE，删行会把用户的歌单条目、收藏、
/// 播放历史一起带走。改了 file_path 之后，所有引用仍然指向同一条记录。
///
/// file_path 不在 songs::update_tags 的 SET 列表里（那个函数是为「同一路径重读标签」
/// 设计的），所以这里单独下发一条 UPDATE。之所以把这条 SQL 写在服务层而不是给
/// repo 加函数：本步的改动范围限定在 library.rs。
///
/// 两条 UPDATE 都在调用方的事务里，要么一起成功要么一起回滚。
fn repoint_song_to_file(conn: &Connection, id: i64, song: &Song) -> Result<(), RepoError> {
    conn.execute(
        "UPDATE songs SET file_path = ?1 WHERE id = ?2",
        params![song.file_path, id],
    )?;
    // update_tags 覆盖其余全部标签字段（含 audio_hash / bitrate_bps / album_id /
    // file_size / file_mtime），且刻意不碰 added_at / deleted_at / scrape_status ——
    // 原地改写后这一行仍然是「原来那首歌」，刮削成果不该被重置。
    songs::update_tags(conn, song)?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// 专辑 upsert 与 Song 组装
// ─────────────────────────────────────────────────────────────────────────────

/// 按 (name, album_artist) upsert 专辑，返回专辑 id；没有专辑信息时返回 None。
///
/// 已有专辑只补缺失的年份，不覆盖封面 / 年份等已有成果 —— 专辑上的封面多半是
/// 刮削写进去的，扫描顺手把它抹掉是不可接受的。album_artist 未知时用空串，
/// 这是 repo 的约定（SQLite 的 UNIQUE 不把多个 NULL 当冲突）。
///
/// `pub(crate)`：手工编辑标签（`service::tag_edit`）改完专辑名也要走这条 ——
/// 专辑换名 / 年份只补不覆盖 / 并发冲突复用行这些规则同样只能有一份实现。
pub(crate) fn upsert_album(conn: &Connection, meta: &AudioMetadata) -> Result<Option<i64>, RepoError> {
    let name = match meta
        .albums
        .iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
    {
        Some(n) => n.to_string(),
        None => return Ok(None),
    };
    let album_artist = album_artist_of(meta);
    let year = meta.year.as_deref().and_then(parse_year);

    if let Some(existing) = albums::find_by_name_artist(conn, &name, &album_artist)? {
        if existing.year.is_none() {
            if let Some(y) = year {
                let mut updated = existing.clone();
                updated.year = Some(y);
                albums::update(conn, &updated)?;
            }
        }
        return Ok(Some(existing.id));
    }

    let album = Album {
        id: 0,
        name,
        album_artist,
        year,
        cover_data: None,
        cover_mime: None,
        updated_at: 0,
    };
    match albums::insert(conn, &album) {
        Ok(id) => Ok(Some(id)),
        // 并发写者抢先插了同一张：复用那一行，不把这次扫描当失败
        Err(RepoError::Conflict { constraint }) => {
            match albums::find_by_name_artist(conn, &album.name, &album.album_artist)? {
                Some(found) => Ok(Some(found.id)),
                None => Err(RepoError::Conflict { constraint }),
            }
        }
        Err(e) => Err(e),
    }
}

/// 专辑艺术家：显式 album_artist 优先，退回第一个非空歌手，都没有就是空串。
fn album_artist_of(meta: &AudioMetadata) -> String {
    if let Some(a) = meta.album_artist.as_deref() {
        let trimmed = a.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    for artist in &meta.artists {
        let trimmed = artist.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    String::new()
}

/// 从任意年份文本里抠出一个四位年份（"2019"、"2019/05"、"circa 199?" 都要能处理）。
/// 只认长度恰好 4 的数字段，且落在 1000..=2999，避免把 "01" / "12345" 当成年份。
fn parse_year(raw: &str) -> Option<i64> {
    let mut run = String::new();
    // 末尾补一个非数字哨兵，保证最后一段数字也会被检查
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

/// 组装要落库的 Song（new_file 时 id = 0，insert 会忽略 id 取新行）。
///
/// `pub(crate)`：手工编辑标签（`service::tag_edit`）写完文件后也走这条映射 ——
/// 「标签 → songs 行」必须只有一份实现，否则两处迟早漂移。
pub(crate) fn build_song(
    id: i64,
    file_path: String,
    file_size: i64,
    file_mtime: Option<i64>,
    meta: &AudioMetadata,
    album_id: Option<i64>,
    audio_hash: Option<String>,
) -> Song {
    let duration_ms = if meta.duration_ms > 0 {
        Some(meta.duration_ms)
    } else {
        None
    };
    let format = match meta.detected_format.clone() {
        Some(f) => Some(f),
        None => Path::new(&file_path)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase()),
    };
    Song {
        id,
        file_path: file_path.clone(),
        album_id,
        title: meta.title.clone(),
        artists: join_non_empty(&meta.artists, " / "),
        album_artist: meta.album_artist.clone(),
        year: meta.year.as_deref().and_then(parse_year),
        genres: join_non_empty(&meta.genres, " / "),
        track: meta.track,
        disc: meta.disc,
        duration_ms,
        bitrate_bps: meta.bitrate_bps,
        format,
        // 由 compute_audio_hash 现算（与标签无关的裸音频 sha256），算不出时是 None
        audio_hash,
        file_size: Some(file_size),
        file_mtime,
        search_text: build_search_text(meta, &file_path),
        lyrics: meta.lyrics.clone(),
        timed_lyrics: meta.lyrics_timed.clone(),
        // 新入库一律 pending（画布：这是刮削队列的来源）；已有行走 update_tags，
        // 该函数不碰 scrape_status，所以更新路径不会重置它。
        scrape_status: ScrapeStatus::Pending,
        scrape_error: None,
        scrape_at: None,
        deleted_at: None,
        added_at: 0,
        updated_at: 0,
    }
}

/// 用分隔符拼接非空项；一项都没有时给 None（不写空串进库）。
fn join_non_empty(items: &[String], sep: &str) -> Option<String> {
    let joined = items
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(sep);
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// 拼装 search_text：标题、歌手、专辑艺术家、专辑、流派 + 文件名主干。
/// 用户经常按文件名搜，所以文件名也是检索面；重复项只保留一次。
fn build_search_text(meta: &AudioMetadata, file_path: &str) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return;
        }
        if !parts.iter().any(|p| p == trimmed) {
            parts.push(trimmed.to_string());
        }
    };

    if let Some(title) = meta.title.as_deref() {
        push(title);
    }
    for artist in &meta.artists {
        push(artist);
    }
    if let Some(album_artist) = meta.album_artist.as_deref() {
        push(album_artist);
    }
    for album in &meta.albums {
        push(album);
    }
    for genre in &meta.genres {
        push(genre);
    }
    if let Some(stem) = Path::new(file_path).file_stem().and_then(|s| s.to_str()) {
        push(stem);
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 磁盘消失 → 软删除
// ─────────────────────────────────────────────────────────────────────────────

/// 把磁盘上已经消失的行标记软删。
///
/// 调用前提：本轮**所有**库根都可信（prune_blockers 为空）。这里的判断用
/// std::fs::metadata，只在明确 NotFound / 已不是普通文件时才标记；权限不足等
/// 其它错误保守放过（宁可漏删，不可误删），并记进 issues。
///
/// 分页用 include_deleted = true：prune 只写 deleted_at，如果按「只列在库」分页，
/// 边标记边翻页会让结果集自己缩小、漏掉后面的行（经典 offset 分页陷阱）。
fn prune_missing(conn: &mut Connection, report: &mut ScanReport) -> Result<(), LibraryError> {
    const BATCH: i64 = 500;
    let mut offset: i64 = 0;

    loop {
        let batch = songs::list(conn, BATCH, offset, true)?;
        if batch.is_empty() {
            break;
        }
        offset += batch.len() as i64;

        let mut to_mark: Vec<i64> = Vec::new();
        for song in &batch {
            if song.deleted_at.is_some() {
                continue; // 已经是软删态：幂等，不再重复处理
            }
            match std::fs::metadata(&song.file_path) {
                Ok(metadata) if metadata.is_file() => {}
                // 存在但已经不是普通文件（变成目录 / 设备等）→ 同样视为消失
                Ok(_) => to_mark.push(song.id),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => to_mark.push(song.id),
                Err(e) => report.issues.push(ScanIssue {
                    path: song.file_path.clone(),
                    message: format!("检查文件是否存在失败，保守起见不标记删除：{e}"),
                }),
            }
        }

        if to_mark.is_empty() {
            continue;
        }

        // 每批一个事务：只标记，绝不 purge
        let tx = conn.transaction()?;
        let mut marked = 0usize;
        for id in &to_mark {
            marked += songs::mark_deleted(&tx, *id)?;
        }
        tx.commit()?;
        report.marked_deleted += marked;
    }

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// 测试
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::db::models::{Playlist, Role, User};
    use crate::db::pool::{DbGuard, TempDb};
    use crate::db::repos::{favorites, history, playlists, songs as songs_repo, users};
    use crate::tag::write::{write_tags, Id3EditMeta};
    use crate::watcher::test_support::TempDir;
    use std::os::unix::fs::PermissionsExt;

    /// 两个标题、专辑都不同的真实样本（fixtures/ 是真拷贝，不依赖 TS 目录）
    const FIXTURE_A: &str = "华夏传说 - 凤凰传奇.mp3";
    const FIXTURE_B: &str = "盛夏-毛不易.mp3";

    struct Env {
        pool: Arc<DbPool>,
        /// 必须留在结构体里：TempDb 一析构就删掉库文件
        _temp: TempDb,
        /// 必须留在结构体里：TempDir 一析构就递归删库根
        _dir: TempDir,
        root: PathBuf,
    }

    impl Env {
        /// 一个跑完全部迁移的临时文件库 + 一个真实临时库根。
        fn new(tag: &str) -> Env {
            let (pool, temp) = DbPool::open_temp(tag).expect("建临时文件库池");
            {
                let mut guard = pool.acquire().expect("借连接");
                migrations::apply(&mut guard).expect("应用迁移");
            }
            let dir = TempDir::new(tag);
            let root = dir.path().join("lib");
            std::fs::create_dir_all(&root).expect("建库根");
            Env {
                pool: Arc::new(pool),
                _temp: temp,
                _dir: dir,
                root,
            }
        }

        fn service(&self) -> LibraryService {
            LibraryService::new(
                Arc::clone(&self.pool),
                vec![self.root.to_string_lossy().into_owned()],
            )
        }

        fn conn(&self) -> DbGuard<'_> {
            self.pool.acquire().expect("借连接")
        }
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    fn copy_fixture(name: &str, dst: &Path) {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).expect("建父目录");
        }
        std::fs::copy(fixture(name), dst).expect("拷贝样本");
    }

    /// 落库时用的规范路径（与 canonical_path_string 同口径）
    fn canonical(path: &Path) -> String {
        std::fs::canonicalize(path)
            .expect("canonicalize")
            .to_string_lossy()
            .into_owned()
    }

    fn sample_song(file_path: &str) -> Song {
        Song {
            id: 0,
            file_path: file_path.to_string(),
            album_id: None,
            title: None,
            artists: None,
            album_artist: None,
            year: None,
            genres: None,
            track: None,
            disc: None,
            duration_ms: None,
            bitrate_bps: None,
            format: None,
            audio_hash: None,
            file_size: None,
            file_mtime: None,
            search_text: None,
            lyrics: None,
            timed_lyrics: None,
            scrape_status: ScrapeStatus::Pending,
            scrape_error: None,
            scrape_at: None,
            deleted_at: None,
            added_at: 0,
            updated_at: 0,
        }
    }

    /// 把文件 mtime 往后推 seconds 秒（内容不动）。
    fn bump_mtime(path: &Path, seconds: u64) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("打开文件改 mtime");
        file.set_modified(SystemTime::now() + std::time::Duration::from_secs(seconds))
            .expect("设置 mtime");
    }

    // ── 1. 新文件入库(pending) ─────────────────────────────────────────────

    #[test]
    fn new_file_is_inserted_pending_and_album_is_upserted() {
        let env = Env::new("scan-new");
        let song_path = env.root.join("华夏传说.mp3");
        copy_fixture(FIXTURE_A, &song_path);

        let report = env.service().scan().expect("扫描");
        assert_eq!(report.added, 1, "新文件必须入库");
        assert_eq!(report.tag_failed, 0);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.marked_deleted, 0);

        let conn = env.conn();
        let stored = canonical(&song_path);
        let row = songs_repo::find_by_file_path(&conn, &stored)
            .expect("按路径查")
            .expect("新文件必须能查到");
        assert_eq!(row.scrape_status, ScrapeStatus::Pending, "新入库必须是 pending");
        assert_eq!(row.title.as_deref(), Some("华夏传说"));
        assert!(row.deleted_at.is_none(), "新入库必须是在库状态");
        assert!(row.file_size.is_some());
        assert!(row.search_text.as_deref().unwrap_or("").contains("华夏传说"));

        let album_id = row.album_id.expect("必须挂了专辑");
        let album = albums::get(&conn, album_id)
            .expect("查专辑")
            .expect("专辑必须存在");
        assert_eq!(album.name, "华夏传说");
        assert!(!album.album_artist.is_empty());
        assert_eq!(songs_repo::count(&conn, false).expect("默认计数"), 1);
    }

    // ── 2. mtime 变化重读 ──────────────────────────────────────────────────

    #[test]
    fn changed_content_and_mtime_are_reread_and_updated() {
        let env = Env::new("scan-update");
        let song_path = env.root.join("track.mp3");
        copy_fixture(FIXTURE_A, &song_path);
        assert_eq!(env.service().scan().expect("首扫").added, 1);

        // 换成另一首歌的内容，并把 mtime 往前推
        std::fs::copy(fixture(FIXTURE_B), &song_path).expect("覆盖为新内容");
        bump_mtime(&song_path, 120);

        let report = env.service().scan().expect("重扫");
        assert_eq!(report.updated, 1, "mtime/size 有变化必须重读并 UPDATE");
        assert_eq!(report.added, 0, "不能当新文件重插");
        assert_eq!(report.skipped, 0);

        let conn = env.conn();
        let row = songs_repo::find_by_file_path(&conn, &canonical(&song_path))
            .expect("查")
            .expect("行必须还在");
        assert_eq!(row.title.as_deref(), Some("盛夏"), "标签内容必须被更新");
        // 注意：读层从 TPE2 拿到的 album_artist 末尾带 NUL（"优乐美\0"），
        // 本模块原样落库（与 scanner 的 artists_summary 口径一致），这里去掉 NUL 再比
        let album_artist = row.album_artist.as_deref().unwrap_or("").replace('\0', "");
        assert_eq!(album_artist.trim(), "优乐美");
        assert_eq!(songs_repo::count(&conn, false).expect("计数"), 1);
    }

    #[test]
    fn mtime_change_alone_triggers_reread() {
        let env = Env::new("scan-mtime-only");
        let song_path = env.root.join("track.mp3");
        copy_fixture(FIXTURE_A, &song_path);
        assert_eq!(env.service().scan().expect("首扫").added, 1);

        // 内容一字不动，只推 mtime
        bump_mtime(&song_path, 300);

        let report = env.service().scan().expect("重扫");
        assert_eq!(report.updated, 1, "只看 mtime 也必须判定为有变化");
        assert_eq!(report.skipped, 0);
    }

    // ── 3. 未变跳过 ────────────────────────────────────────────────────────

    #[test]
    fn unchanged_file_is_skipped_not_updated() {
        let env = Env::new("scan-skip");
        copy_fixture(FIXTURE_A, &env.root.join("a.mp3"));
        assert_eq!(env.service().scan().expect("首扫").added, 1);

        let report = env.service().scan().expect("重扫");
        assert_eq!(report.skipped, 1, "未改动必须计入跳过（用计数断言，不只看行数）");
        assert_eq!(report.updated, 0, "未改动不能误判成更新");
        assert_eq!(report.added, 0);

        let conn = env.conn();
        assert_eq!(songs_repo::count(&conn, false).expect("计数"), 1);
    }

    // ── 4. 磁盘消失标记（软删）─────────────────────────────────────────────

    #[test]
    fn vanished_file_is_marked_deleted_softly() {
        let env = Env::new("scan-gone");
        let keep = env.root.join("keep.mp3");
        let gone = env.root.join("gone.mp3");
        copy_fixture(FIXTURE_A, &keep);
        copy_fixture(FIXTURE_B, &gone);
        let stored_root = canonical(&env.root);
        let gone_stored = format!("{stored_root}/gone.mp3");

        assert_eq!(env.service().scan().expect("首扫").added, 2);

        std::fs::remove_file(&gone).expect("删掉一个文件");
        let report = env.service().scan().expect("重扫");
        assert_eq!(report.marked_deleted, 1);
        assert!(
            report.prune_skipped.is_none(),
            "还有一个文件命中，扫描是可信的，不该跳过标记删除"
        );

        let conn = env.conn();
        let row = songs_repo::find_by_file_path(&conn, &gone_stored)
            .expect("按路径查")
            .expect("软删行必须仍能按路径查到（否则文件回来时无法复原）");
        assert!(row.deleted_at.is_some(), "deleted_at 必须非 NULL");

        // 默认（面向用户）列表查不到，但行还在
        assert!(
            songs_repo::list(&conn, 10, 0, false)
                .expect("默认列表")
                .iter()
                .all(|s| s.id != row.id),
            "默认列表不该出现软删行"
        );
        assert_eq!(songs_repo::count(&conn, false).expect("默认计数"), 1);
        assert_eq!(songs_repo::count_deleted(&conn).expect("软删计数"), 1);
        assert_eq!(songs_repo::count(&conn, true).expect("含软删计数"), 2);
    }

    #[test]
    fn returning_file_is_restored_not_reinserted() {
        let env = Env::new("scan-restore");
        let song_path = env.root.join("back.mp3");
        let keep = env.root.join("keep.mp3");
        copy_fixture(FIXTURE_A, &song_path);
        // 留一个不动摇的文件：删光之后 files_seen == 0 会被判可疑而跳过 prune，
        // 那是刻意的安全不变式（见 empty_scan_result_never_marks_anything_deleted）
        copy_fixture(FIXTURE_B, &keep);
        let stored = canonical(&song_path);

        assert_eq!(env.service().scan().expect("首扫").added, 2);
        std::fs::remove_file(&song_path).expect("删文件");
        assert_eq!(env.service().scan().expect("二扫").marked_deleted, 1);
        {
            let conn = env.conn();
            let row = songs_repo::find_by_file_path(&conn, &stored)
                .expect("查")
                .expect("行还在");
            assert!(row.deleted_at.is_some());
        }

        // 文件回来 → restore，而不是重插
        copy_fixture(FIXTURE_A, &song_path);
        let report = env.service().scan().expect("三扫");
        assert_eq!(report.added, 0, "文件回来不能当新文件重插");
        assert_eq!(report.restored, 1, "必须走 restore");

        let conn = env.conn();
        let row = songs_repo::find_by_file_path(&conn, &stored)
            .expect("查")
            .expect("行还在");
        assert!(row.deleted_at.is_none(), "复原后必须是在库状态");
        assert_eq!(
            songs_repo::count(&conn, false).expect("默认计数"),
            2,
            "回来的那首与一直在的那首都应在库"
        );
        assert_eq!(songs_repo::count_deleted(&conn).expect("软删计数"), 0);
    }

    // ── 5. 读标签失败跳过 ──────────────────────────────────────────────────

    #[test]
    fn unreadable_tags_are_counted_and_do_not_block_other_files() {
        let env = Env::new("scan-badtag");
        let good = env.root.join("good.mp3");
        copy_fixture(FIXTURE_A, &good);
        // .flac 扩展名但内容是纯文本：read_tags 会返回 Unrecognized
        let bad = env.root.join("bad.flac");
        std::fs::write(&bad, b"this is definitely not audio").expect("写垃圾文件");

        let report = env.service().scan().expect("扫描不能因单文件失败而整体失败");
        assert_eq!(report.tag_failed, 1, "读标签失败必须计入失败数");
        assert_eq!(report.added, 1, "其它文件必须照常入库");
        assert_eq!(report.issues.len(), 1, "失败要留下一条可读的错误记录");
        assert!(report.issues[0].path.ends_with("bad.flac"));
        assert!(
            report.issues[0]
                .message
                .chars()
                .any(|c| ('一'..='鿿').contains(&c)),
            "错误信息必须是中文：{}",
            report.issues[0].message
        );

        let conn = env.conn();
        assert_eq!(songs_repo::count(&conn, false).expect("计数"), 1);
        assert!(songs_repo::find_by_file_path(&conn, &canonical(&bad))
            .expect("查坏文件")
            .is_none(), "读标签失败的文件不该入库");
    }

    // ── 6. 安全不变式：不可信的扫描绝不标记删除 ────────────────────────────

    #[test]
    fn empty_scan_result_never_marks_anything_deleted() {
        let env = Env::new("scan-guard-empty");
        // 库里先放一首「库根下已经不存在」的歌 —— 天真实现会把它标记删除
        let ghost_path = env.root.join("ghost.mp3").to_string_lossy().into_owned();
        {
            let conn = env.conn();
            songs_repo::insert(&conn, &sample_song(&ghost_path)).expect("预置幽灵歌");
        }

        // 库根可读，但一个音频文件都没有（挂载闪断的典型形态）
        let report = env.service().scan().expect("扫描");
        assert_eq!(report.files_seen, 0);
        assert_eq!(report.added, 0);
        assert_eq!(report.marked_deleted, 0, "零命中时绝不允许标记删除");
        assert!(
            report.prune_skipped.is_some(),
            "零命中必须整轮跳过标记删除并说明原因"
        );

        let conn = env.conn();
        let row = songs_repo::find_by_file_path(&conn, &ghost_path)
            .expect("查")
            .expect("行必须还在");
        assert!(
            row.deleted_at.is_none(),
            "安全不变式：扫描结果为空时数据库里的歌一条都不能被标记删除"
        );
        assert_eq!(songs_repo::count(&conn, false).expect("默认计数"), 1);
    }

    #[test]
    fn missing_root_never_marks_anything_deleted() {
        let env = Env::new("scan-guard-missing");
        let ghost_path = env.root.join("ghost.mp3").to_string_lossy().into_owned();
        {
            let conn = env.conn();
            songs_repo::insert(&conn, &sample_song(&ghost_path)).expect("预置幽灵歌");
        }

        // 库根整个不存在 —— 移动硬盘没挂载的形态
        let missing = env._dir.path().join("not-mounted");
        let service = LibraryService::new(
            Arc::clone(&env.pool),
            vec![missing.to_string_lossy().into_owned()],
        );
        let report = service.scan().expect("扫描本身要能返回报告，而不是炸掉");
        assert!(!report.root_errors.is_empty(), "根不存在必须回报异常");
        assert_eq!(report.marked_deleted, 0);
        assert!(report.prune_skipped.is_some());

        let conn = env.conn();
        let row = songs_repo::find_by_file_path(&conn, &ghost_path)
            .expect("查")
            .expect("行必须还在");
        assert!(row.deleted_at.is_none(), "库根读不开时绝不能标记删除");
    }

    /// chmod 000 守卫：离开作用域一定把权限改回去，否则 TempDir 删不掉。
    struct DenyMode {
        path: PathBuf,
        original: u32,
    }

    impl DenyMode {
        fn new(path: &Path) -> DenyMode {
            let original = std::fs::metadata(path)
                .map(|m| m.permissions().mode() & 0o7777)
                .expect("读取原权限");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000))
                .expect("设 000");
            DenyMode {
                path: path.to_path_buf(),
                original,
            }
        }
    }

    impl Drop for DenyMode {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.original));
        }
    }

    #[test]
    fn unreadable_root_never_marks_anything_deleted() {
        // SAFETY: geteuid 无参数、无副作用，永远成功
        let is_root = unsafe { libc::geteuid() } == 0;
        if is_root {
            eprintln!("[service::library::tests] 以 root 运行：000 目录仍可读，跳过权限类断言");
            return;
        }

        let env = Env::new("scan-guard-000");
        let ghost_path = env.root.join("ghost.mp3").to_string_lossy().into_owned();
        {
            let conn = env.conn();
            songs_repo::insert(&conn, &sample_song(&ghost_path)).expect("预置幽灵歌");
        }

        let guard = DenyMode::new(&env.root);
        // 前置条件自检：确认 000 真的拦住了当前用户
        assert!(std::fs::read_dir(&env.root).is_err(), "前置条件不成立：000 目录竟然可读");

        let report = env.service().scan().expect("扫描要返回报告");
        assert!(!report.root_errors.is_empty(), "根不可读必须回报异常");
        assert_eq!(report.files_seen, 0);
        assert_eq!(report.marked_deleted, 0);
        assert!(report.prune_skipped.is_some());
        drop(guard);

        let conn = env.conn();
        let row = songs_repo::find_by_file_path(&conn, &ghost_path)
            .expect("查")
            .expect("行必须还在");
        assert!(
            row.deleted_at.is_none(),
            "安全不变式：库根读不开时绝不能把整库标记为已删除"
        );
    }

    // ── 7. 其它约定 ────────────────────────────────────────────────────────

    #[test]
    fn metadata_update_does_not_reset_scrape_status() {
        let env = Env::new("scan-scrape-status");
        let song_path = env.root.join("track.mp3");
        copy_fixture(FIXTURE_A, &song_path);
        let stored = canonical(&song_path);
        assert_eq!(env.service().scan().expect("首扫").added, 1);

        {
            let conn = env.conn();
            let row = songs_repo::find_by_file_path(&conn, &stored)
                .expect("查")
                .expect("行在");
            songs_repo::update_scrape_status(&conn, row.id, ScrapeStatus::Done, None)
                .expect("标记为已刮削");
        }

        std::fs::copy(fixture(FIXTURE_B), &song_path).expect("覆盖内容");
        bump_mtime(&song_path, 60);
        assert_eq!(env.service().scan().expect("重扫").updated, 1);

        let conn = env.conn();
        let row = songs_repo::find_by_file_path(&conn, &stored)
            .expect("查")
            .expect("行在");
        assert_eq!(row.title.as_deref(), Some("盛夏"), "标签要更新");
        assert_eq!(
            row.scrape_status,
            ScrapeStatus::Done,
            "元数据变更绝不能把 scrape_status 重置回 pending"
        );
    }

    #[test]
    fn duplicate_path_inside_one_scan_is_updated_not_a_hard_failure() {
        let env = Env::new("scan-dup-path");
        copy_fixture(FIXTURE_A, &env.root.join("a.mp3"));
        let first = env.service().scan().expect("首扫");
        assert_eq!(first.added, 1);
        // 同路径再扫一次：走「已存在」分支，绝不能撞 UNIQUE 变成硬失败
        let second = env.service().scan().expect("重扫");
        assert_eq!(second.db_failed, 0);
        assert_eq!(second.added, 0);
    }

    #[test]
    fn no_roots_is_an_error_and_touches_nothing() {
        let env = Env::new("scan-no-roots");
        let ghost_path = env.root.join("ghost.mp3").to_string_lossy().into_owned();
        {
            let conn = env.conn();
            songs_repo::insert(&conn, &sample_song(&ghost_path)).expect("预置");
        }

        let service = LibraryService::new(Arc::clone(&env.pool), vec!["   ".to_string()]);
        let outcome = service.scan();
        assert!(matches!(outcome, Err(LibraryError::NoRoots)));

        let conn = env.conn();
        let row = songs_repo::find_by_file_path(&conn, &ghost_path)
            .expect("查")
            .expect("行在");
        assert!(row.deleted_at.is_none());
    }

    #[test]
    fn parse_year_handles_the_shapes_that_show_up_in_tags() {
        assert_eq!(parse_year("2019"), Some(2019));
        assert_eq!(parse_year("2019/05"), Some(2019));
        assert_eq!(parse_year("circa 199?"), None);
        assert_eq!(parse_year(""), None);
        assert_eq!(parse_year("01"), None);
        assert_eq!(parse_year("12345"), None);
        assert_eq!(parse_year("发行于 2005 年"), Some(2005));
    }

    #[test]
    fn service_exposes_roots_and_config_constructor() {
        let env = Env::new("scan-roots");
        let service = LibraryService::new(
            Arc::clone(&env.pool),
            vec!["  /a  ".to_string(), "".to_string(), "/b".to_string()],
        );
        assert_eq!(service.roots().len(), 2, "空白项应被丢弃");
        assert_eq!(service.roots()[0], PathBuf::from("/a"), "两侧空白应被裁掉");

        let cfg = StorageConfig {
            kind: "local".to_string(),
            library_roots: vec!["/m".to_string()],
        };
        let from_cfg = LibraryService::from_config(Arc::clone(&env.pool), &cfg);
        assert_eq!(from_cfg.roots(), &[PathBuf::from("/m")]);
    }

    #[test]
    fn error_and_report_display_are_chinese() {
        let cases: Vec<LibraryError> = vec![
            LibraryError::NoRoots,
            LibraryError::Repo(RepoError::Invariant {
                message: "刚写入的行查不到".to_string(),
            }),
        ];
        for err in &cases {
            let text = err.to_string();
            assert!(
                text.chars().any(|c| ('一'..='鿿').contains(&c)),
                "错误信息必须是中文：{text}"
            );
        }
        assert!(std::error::Error::source(&LibraryError::NoRoots).is_none());
        assert!(std::error::Error::source(&cases[1]).is_some());

        let mut report = ScanReport {
            files_seen: 2,
            added: 1,
            tag_failed: 1,
            issues: vec![ScanIssue {
                path: "/m/bad.flac".to_string(),
                message: "读取标签失败".to_string(),
            }],
            ..ScanReport::default()
        };
        report.prune_skipped = Some("零命中".to_string());
        let text = report.to_string();
        assert!(text.contains("新增 1"), "报告要能直接打日志：{text}");
        assert!(text.contains("读标签失败 1"));
        assert!(text.contains("零命中"), "跳过标记删除的原因必须带出来：{text}");
        assert!(report.has_issues());
    }

    // ── 8. audio_hash（S6 去重 / S19 转码缓存的地基）───────────────────────

    /// 同一份音频、只改标签 → 哈希相同；内容不同 → 哈希不同。
    #[test]
    fn audio_hash_ignores_tags_but_tracks_audio_content() {
        let env = Env::new("scan-audio-hash");
        let original = env.root.join("same-a.mp3");
        let retagged = env.root.join("same-b.mp3");
        let other = env.root.join("other.mp3");
        copy_fixture(FIXTURE_A, &original);
        copy_fixture(FIXTURE_A, &retagged);
        copy_fixture(FIXTURE_B, &other);

        // 只改标签：写层的契约是「音频字节不动」
        let edited = Id3EditMeta {
            title: Some("换了个标题".to_string()),
            ..Default::default()
        };
        write_tags(&retagged, &edited).expect("只改标签");

        // 先证明两个文件的标签确实不同，否则这条测试是空的
        let tag_a = read_tags(&original).expect("读原文件标签");
        let tag_b = read_tags(&retagged).expect("读改过的标签");
        assert_ne!(tag_a.title, tag_b.title, "标签必须真的改掉了");
        assert_eq!(tag_b.title.as_deref(), Some("换了个标题"));

        // 哈希本身：同一份音频只换标签 → 相同；内容不同 → 不同。
        // ⚠️ 这里直接调 compute_audio_hash 而不是只看库里的行：S6 去重上线之后，
        //    哈希相同的两个文件会被合并成一条，库里已经看不到「第二份」的哈希了。
        let hash_of_file = |p: &Path| compute_audio_hash(p).0.expect("三种格式都应算出哈希");
        let hash_a = hash_of_file(&original);
        let hash_b = hash_of_file(&retagged);
        let hash_other = hash_of_file(&other);
        assert_eq!(hash_a.len(), 64, "应是裸小写 hex（sha256）");
        assert_eq!(
            hash_a, hash_b,
            "同一份音频、只换标签，audio_hash 必须相同（S6 去重的地基）"
        );
        assert_ne!(hash_a, hash_other, "内容不同，哈希必须不同");

        // 扫描按这个哈希去重：same-a 与 same-b 是同一份音频 → 合并成一条，
        // other 内容不同 → 另起一条。three 份文件 → 两条记录 + 一次 deduped。
        let report = env.service().scan().expect("扫描");
        assert_eq!(report.added, 2, "same-a / same-b 合并成一条，other 另算一条");
        assert_eq!(report.deduped, 1, "哈希相同的第二份必须被判重跳过");
        assert_eq!(report.hash_missing, 0, "三种格式都应算出哈希");

        let conn = env.conn();
        // ⚠️ AudioWalker 刻意不排序，同一个目录里先扫到谁由 readdir 顺序决定；
        //    两份的哈希本来就相同，所以留哪一份等价 —— 关键是「只留一份、哈希不变」。
        let rows: Vec<Song> = [&original, &retagged]
            .iter()
            .filter_map(|p| {
                songs_repo::find_by_file_path(&conn, &canonical(p)).expect("按路径查")
            })
            .collect();
        assert_eq!(rows.len(), 1, "同哈希的两份只能留下一行");
        assert_eq!(
            rows[0].audio_hash.as_deref(),
            Some(hash_a.as_str()),
            "库里存的必须就是这个哈希"
        );
        assert_eq!(songs_repo::count(&conn, true).expect("含软删计数"), 2);
    }

    /// 三种格式都要能算出哈希（mp3 / flac / wav 各走一个分支）。
    #[test]
    fn audio_hash_is_computed_for_every_supported_format() {
        let env = Env::new("scan-audio-hash-formats");
        copy_fixture(FIXTURE_A, &env.root.join("a.mp3"));
        copy_fixture("牵丝戏 - 白兀.flac", &env.root.join("b.flac"));
        copy_fixture("tagged.wav", &env.root.join("c.wav"));

        let report = env.service().scan().expect("扫描");
        assert_eq!(report.added, 3);
        assert_eq!(report.hash_missing, 0);

        let conn = env.conn();
        for name in ["a.mp3", "b.flac", "c.wav"] {
            let row = songs_repo::find_by_file_path(&conn, &canonical(&env.root.join(name)))
                .expect("按路径查")
                .expect("应有该行");
            let hash = row.audio_hash.expect("三种格式都必须算出哈希");
            assert_eq!(hash.len(), 64, "{name} 的哈希应是 64 位小写 hex");
            assert!(
                hash.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{name} 的哈希应是小写 hex：{hash}"
            );
        }
    }

    /// 扩展名不认识：不 panic、写 NULL、给出原因（理论上 walker 进不来）。
    #[test]
    fn unknown_extension_yields_no_hash_and_a_recorded_reason() {
        let env = Env::new("scan-hash-unknown");
        let weird = env.root.join("mystery.txt");
        std::fs::write(&weird, b"not audio").expect("写文件");

        let (hash, reason) = compute_audio_hash(&weird);
        assert!(hash.is_none(), "不认识的格式必须写 NULL，而不是瞎猜");
        assert!(reason.is_some(), "要能说明为什么没算出来");

        // txt 不是音频，walker 根本不会遍历到它；扫描仍要正常返回
        let report = env.service().scan().expect("扫描不能 panic");
        assert_eq!(report.files_seen, 0);
        assert_eq!(report.tag_failed, 0);
    }

    // ── 9. S6 多目录去重 ───────────────────────────────────────────────────

    /// 造一对「同一份音频、码率不同」的 WAV 拷贝。
    ///
    /// data chunk 逐字节相同（audio_hash 因此相同），但一份清空标签、一份保留原标签，
    /// 文件大小不同 —— 有 ffprobe 时 bitrate_bps = 文件大小 × 8 ÷ 时长，于是码率不同。
    /// 同内容的音频没法直接改出不同码率（码率由音频本身决定），这是本机能造出的
    /// 唯一真实差异。无 ffprobe（MR_NO_FFPROBE）时 native_probe 的 WAV 码率 =
    /// fmt.byte_rate × 8，两份相等；调用方用 real_bitrate 自检，不成立就跳过码率断言。
    fn write_wav_pair(low: &Path, high: &Path) {
        copy_fixture("tagged.wav", low);
        write_tags(
            low,
            &Id3EditMeta {
                blank_all: true,
                ..Default::default()
            },
        )
        .expect("清空 WAV 标签");
        copy_fixture("tagged.wav", high);
    }

    /// 走生产读层读一个文件真实的 bitrate_bps（与扫描用的是同一条路径）。
    fn real_bitrate(path: &Path) -> Option<i64> {
        read_tags(path).expect("读标签").bitrate_bps
    }

    /// 库里全部行的 (id, file_path, audio_hash)，用来断言「多扫几遍没有抖动」。
    fn song_snapshot(conn: &Connection) -> Vec<(i64, String, Option<String>)> {
        songs_repo::list(conn, 1000, 0, true)
            .expect("列出全部歌曲")
            .into_iter()
            .map(|s| (s.id, s.file_path, s.audio_hash))
            .collect()
    }

    /// 两个根各放一份**内容相同**的音频 → 只留一条记录，另一份计入 deduped。
    #[test]
    fn same_audio_in_two_roots_collapses_to_one_row() {
        let env = Env::new("scan-dedup-roots");
        let root_b = env._dir.path().join("lib-b");
        std::fs::create_dir_all(&root_b).expect("建第二个库根");
        let first = env.root.join("one.mp3");
        let second = root_b.join("two.mp3");
        copy_fixture(FIXTURE_A, &first);
        copy_fixture(FIXTURE_A, &second);

        let service = LibraryService::new(
            Arc::clone(&env.pool),
            vec![
                env.root.to_string_lossy().into_owned(),
                root_b.to_string_lossy().into_owned(),
            ],
        );
        let report = service.scan().expect("扫描");
        assert_eq!(report.files_seen, 2, "两个根各有一份");
        assert_eq!(report.added, 1, "内容相同的两份只应入库一条");
        assert_eq!(report.deduped, 1, "第二份必须判重跳过");
        assert_eq!(report.updated, 0, "没有发生原地改写");

        let conn = env.conn();
        assert_eq!(songs_repo::count(&conn, true).expect("含软删计数"), 1);
        assert_eq!(songs_repo::count(&conn, false).expect("在库计数"), 1);
        assert!(
            songs_repo::find_by_file_path(&conn, &canonical(&first))
                .expect("按路径查第一份")
                .is_some(),
            "先扫到的那份必须在库"
        );
        assert!(
            songs_repo::find_by_file_path(&conn, &canonical(&second))
                .expect("按路径查第二份")
                .is_none(),
            "被判重跳过的那份不该留下任何行"
        );
    }

    /// 已有记录 + 新文件码率更高 → 新文件胜，那一行**原地改写**成指向新文件。
    #[test]
    fn higher_bitrate_copy_wins_and_the_row_points_at_it() {
        let env = Env::new("scan-dedup-bitrate");
        let root_b = env._dir.path().join("lib-b");
        std::fs::create_dir_all(&root_b).expect("建第二个库根");
        let low = env.root.join("low.wav");
        let high = root_b.join("high.wav");
        write_wav_pair(&low, &high);

        // 前置条件：这一对拷贝必须真的码率不同，否则这条测试是空的
        let (br_low, br_high) = (real_bitrate(&low), real_bitrate(&high));
        match (br_low, br_high) {
            (Some(l), Some(h)) if h > l => {}
            other => {
                eprintln!(
                    "[service::library::tests] 本机造不出码率不同的同内容文件（{other:?}），跳过码率类断言；胜负规则由 dedup_winner_rule_is_bitrate_then_added_at_then_id 覆盖"
                );
                return;
            }
        }

        // 低码率那份先入库（第一个根先扫），高码率那份后到
        let service = LibraryService::new(
            Arc::clone(&env.pool),
            vec![
                env.root.to_string_lossy().into_owned(),
                root_b.to_string_lossy().into_owned(),
            ],
        );
        let report = service.scan().expect("扫描");
        assert_eq!(report.added, 1, "低码率那份先入库");
        assert_eq!(report.updated, 1, "高码率那份应原地改写已有行");
        assert_eq!(report.deduped, 0, "胜出的一方不是「被跳过」");
        assert_eq!(report.marked_deleted, 0, "低码率文件还在磁盘上，不该被标记删除");

        let conn = env.conn();
        assert_eq!(songs_repo::count(&conn, true).expect("含软删计数"), 1, "库里始终只有一条");
        assert!(
            songs_repo::find_by_file_path(&conn, &canonical(&low))
                .expect("查低码率")
                .is_none(),
            "落败的那份不该留下自己的行"
        );
        let row = songs_repo::find_by_file_path(&conn, &canonical(&high))
            .expect("查高码率")
            .expect("胜出者必须留下");
        assert_eq!(row.bitrate_bps, br_high, "留下的必须是码率更高的那一份");
        assert_eq!(row.file_size, Some(std::fs::metadata(&high).expect("元信息").len() as i64));
    }

    /// 反过来：先入库的是高码率，后到的是低码率 → 新文件判负，一行都不写。
    /// 这条不依赖 ffprobe（码率相同也会因为「先入库者胜」判负）。
    #[test]
    fn lower_bitrate_newcomer_is_deduped_and_incumbent_is_untouched() {
        let env = Env::new("scan-dedup-bitrate-lose");
        let root_b = env._dir.path().join("lib-b");
        std::fs::create_dir_all(&root_b).expect("建第二个库根");
        let high = env.root.join("high.wav");
        let low = root_b.join("low.wav");
        write_wav_pair(&low, &high);

        let service = LibraryService::new(
            Arc::clone(&env.pool),
            vec![
                env.root.to_string_lossy().into_owned(),
                root_b.to_string_lossy().into_owned(),
            ],
        );
        let report = service.scan().expect("扫描");
        assert_eq!(report.added, 1, "高码率那份先入库");
        assert_eq!(report.deduped, 1, "后到的低码率那份必须判重跳过");
        assert_eq!(report.updated, 0, "判负的一方一行都不许写");

        let conn = env.conn();
        assert_eq!(song_snapshot(&conn).len(), 1);
        let row = songs_repo::find_by_file_path(&conn, &canonical(&high))
            .expect("查")
            .expect("先入库的那条必须原样留着");
        assert_eq!(row.bitrate_bps, real_bitrate(&high));
        assert!(
            songs_repo::find_by_file_path(&conn, &canonical(&low))
                .expect("查")
                .is_none()
        );
    }

    /// 「多余记录被清理」：库里从头到尾就没有出现过第二条记录（不是先有两条再删一条）。
    #[test]
    fn deduped_copy_never_creates_a_second_row() {
        let env = Env::new("scan-dedup-no-second-row");
        let root_b = env._dir.path().join("lib-b");
        std::fs::create_dir_all(&root_b).expect("建第二个库根");
        copy_fixture(FIXTURE_A, &env.root.join("one.mp3"));
        copy_fixture(FIXTURE_A, &root_b.join("two.mp3"));

        let service = LibraryService::new(
            Arc::clone(&env.pool),
            vec![
                env.root.to_string_lossy().into_owned(),
                root_b.to_string_lossy().into_owned(),
            ],
        );
        assert_eq!(service.scan().expect("扫描").deduped, 1);

        let conn = env.conn();
        assert_eq!(songs_repo::count(&conn, true).expect("含软删计数"), 1, "库里只能有一条记录");
        assert_eq!(
            songs_repo::count_deleted(&conn).expect("软删计数"),
            0,
            "不存在「先插后删」的痕迹"
        );
        // songs.id 是 AUTOINCREMENT：只要第二行真的被 INSERT 过（哪怕随后被删掉），
        // sqlite_sequence 就会前进到 2。这里必须还是 1。
        let seq: i64 = conn
            .query_row(
                "SELECT seq FROM sqlite_sequence WHERE name = 'songs'",
                [],
                |row| row.get(0),
            )
            .expect("读 songs 的自增序列");
        assert_eq!(seq, 1, "第二条记录从未被插入过（不是先有两条再删一条）");
    }

    /// 稳定性：连扫两遍（三遍）收敛，added / updated / marked_deleted 都是 0，
    /// 行数与 file_path / audio_hash 一个字都不变。
    #[test]
    fn rescanning_after_dedup_is_idempotent() {
        let env = Env::new("scan-dedup-stable");
        let root_b = env._dir.path().join("lib-b");
        std::fs::create_dir_all(&root_b).expect("建第二个库根");
        copy_fixture(FIXTURE_A, &env.root.join("one.mp3"));
        copy_fixture(FIXTURE_A, &root_b.join("two.mp3"));

        let service = LibraryService::new(
            Arc::clone(&env.pool),
            vec![
                env.root.to_string_lossy().into_owned(),
                root_b.to_string_lossy().into_owned(),
            ],
        );
        let first = service.scan().expect("首扫");
        assert_eq!(first.added, 1);
        assert_eq!(first.deduped, 1);

        let before = {
            let conn = env.conn();
            song_snapshot(&conn)
        };

        let second = service.scan().expect("二扫");
        assert_eq!(second.added, 0, "再扫不该新增");
        assert_eq!(second.updated, 0, "再扫不该改写");
        assert_eq!(second.marked_deleted, 0, "再扫不该标记删除");
        assert_eq!(
            second.deduped, 1,
            "被跳过的那份每轮都会重新走判重再判负 —— 这是预期行为，不是抖动"
        );
        assert_eq!(second.skipped, 1, "胜出的那份按路径命中且未变化");
        assert_eq!(
            {
                let conn = env.conn();
                song_snapshot(&conn)
            },
            before,
            "库必须收敛：行数与 file_path / audio_hash 都不能变"
        );

        let third = service.scan().expect("三扫");
        assert_eq!((third.added, third.updated, third.marked_deleted), (0, 0, 0));
        assert_eq!(
            {
                let conn = env.conn();
                song_snapshot(&conn)
            },
            before
        );
    }

    /// 引用不丢：歌单条目 / 收藏 / 播放历史仍然存在且指向同一个 song_id，
    /// 而那一行的 file_path 已经变成音质更好的新文件（验证「原地改写」）。
    #[test]
    fn in_place_rewrite_keeps_playlist_favorite_and_history() {
        let env = Env::new("scan-dedup-refs");
        let root_b = env._dir.path().join("lib-b");
        std::fs::create_dir_all(&root_b).expect("建第二个库根");
        let low = env.root.join("low.wav");
        let high = root_b.join("high.wav");
        write_wav_pair(&low, &high);

        // 1) A（低码率）先入库
        let only_a = LibraryService::new(
            Arc::clone(&env.pool),
            vec![env.root.to_string_lossy().into_owned()],
        );
        assert_eq!(only_a.scan().expect("首扫").added, 1);

        // 2) 把 A 加进歌单 / 收藏 / 播放历史
        let (song_id, user_id, playlist_id) = {
            let conn = env.conn();
            let row = songs_repo::find_by_file_path(&conn, &canonical(&low))
                .expect("按路径查")
                .expect("A 必须在库");
            let user_id = users::insert(
                &conn,
                &User {
                    id: 0,
                    username: "听歌的人".to_string(),
                    password_hash: "无".to_string(),
                    role: Role::User,
                    created_at: 0,
                    last_login: None,
                },
            )
            .expect("插用户");
            let playlist_id = playlists::insert(
                &conn,
                &Playlist {
                    id: 0,
                    user_id,
                    name: "我的歌单".to_string(),
                    description: None,
                    is_public: false,
                    created_at: 0,
                    updated_at: 0,
                },
            )
            .expect("插歌单");
            playlists::add_item(&conn, playlist_id, row.id, None).expect("加歌单条目");
            favorites::add(&conn, user_id, row.id).expect("加收藏");
            history::record(&conn, user_id, row.id, Some(1000)).expect("记播放历史");

            // 把库里 A 的码率压到 B 之下，B 胜出因此完全确定 ——
            // 这条测试只关心「原地改写不丢引用」，不该随本机有无 ffprobe 飘。
            let br_high = match real_bitrate(&high) {
                Some(b) => b,
                None => {
                    eprintln!("[service::library::tests] 读不到 B 的码率，跳过引用不丢的断言");
                    return;
                }
            };
            conn.execute(
                "UPDATE songs SET bitrate_bps = ?1 WHERE id = ?2",
                params![br_high - 1, row.id],
            )
            .expect("压低 A 的码率");
            (row.id, user_id, playlist_id)
        };

        // 3) 放入音质更好的 B，重扫
        let both = LibraryService::new(
            Arc::clone(&env.pool),
            vec![
                env.root.to_string_lossy().into_owned(),
                root_b.to_string_lossy().into_owned(),
            ],
        );
        let report = both.scan().expect("重扫");
        assert_eq!(report.updated, 1, "B 应原地改写已有行");
        assert_eq!(report.added, 0, "绝不能新插一行");
        assert_eq!(report.marked_deleted, 0, "两份文件都还在磁盘上，不该标记删除");

        let conn = env.conn();
        let row = songs_repo::get(&conn, song_id, false)
            .expect("查")
            .expect("同一行必须还在");
        assert_eq!(row.id, song_id, "song_id 绝不能变（变号就等于换了首歌）");
        assert_eq!(
            row.file_path,
            canonical(&high),
            "这一行必须已经指向音质更好的 B"
        );
        assert_eq!(row.bitrate_bps, real_bitrate(&high));

        // 三张引用表都还在，且仍然指向同一个 song_id
        let items = playlists::list_items(&conn, playlist_id).expect("列歌单条目");
        assert_eq!(items.len(), 1, "歌单条目不能被外键 CASCADE 带走");
        assert_eq!(items[0].song_id, song_id, "歌单条目必须仍指向原 song_id");
        assert!(
            favorites::is_favorited(&conn, user_id, song_id).expect("查收藏"),
            "收藏不能被外键 CASCADE 带走"
        );
        let played = history::recent(&conn, user_id, 10, 0).expect("列播放历史");
        assert_eq!(played.len(), 1, "播放历史不能被外键 CASCADE 带走");
        assert_eq!(played[0].song_id, song_id, "播放历史必须仍指向原 song_id");
        assert_eq!(songs_repo::count(&conn, true).expect("含软删计数"), 1, "库里仍然只有一条");
    }

    /// 无哈希不误伤：audio_hash 为 NULL 的行不参与判重，也算不出哈希的文件照常入库。
    #[test]
    fn rows_without_audio_hash_never_join_dedup() {
        let env = Env::new("scan-dedup-nohash");

        // 判重键本身：None 与空串都不算哈希（空串是脏数据防线）
        assert!(dedup_key(None).is_none());
        assert!(dedup_key(Some("")).is_none());
        assert_eq!(dedup_key(Some("abc")), Some("abc"));

        // 库里先躺着一条哈希为 NULL 的行（模拟读不出哈希的历史数据）
        let ghost = env.root.join("ghost.mp3").to_string_lossy().into_owned();
        {
            let conn = env.conn();
            songs_repo::insert(&conn, &sample_song(&ghost)).expect("预置无哈希行");
        }

        // 真实音频：就算库里有一条 NULL 哈希的行，也必须照常入库
        let real = env.root.join("real.mp3");
        copy_fixture(FIXTURE_A, &real);
        {
            let mut conn = env.pool.acquire().expect("借连接");
            let outcome = apply_file(&mut conn, &real).expect("真实音频必须入库");
            assert!(
                matches!(outcome, FileOutcome::Added { hash_missing: None }),
                "NULL 哈希的行不该成为判重候选，实际 {outcome:?}"
            );
        }

        // 一个「magic 是合法 WAV、扩展名不认识」的文件：read_tags 靠 magic 读得出，
        // compute_audio_hash 认不出扩展名 → None。这样才够到「无哈希仍要入库」那条路径。
        let weird = env.root.join("mystery.xyz");
        copy_fixture("tagged.wav", &weird);
        let (hash, reason) = compute_audio_hash(&weird);
        assert!(hash.is_none() && reason.is_some(), "前置条件：这个扩展名算不出哈希");
        {
            let mut conn = env.pool.acquire().expect("借连接");
            let outcome = apply_file(&mut conn, &weird).expect("无哈希文件也必须入库");
            match outcome {
                FileOutcome::Added { hash_missing: Some(_) } => {}
                other => panic!("无哈希时应当照常插入并记下原因，实际 {other:?}"),
            }
        }

        let conn = env.conn();
        assert_eq!(
            songs_repo::count(&conn, true).expect("含软删计数"),
            3,
            "幽灵行 + 真实音频 + 无哈希音频，一条都不能少"
        );
        let untouched = songs_repo::find_by_file_path(&conn, &ghost)
            .expect("查")
            .expect("幽灵行必须原封不动");
        assert!(untouched.audio_hash.is_none());
        assert_eq!(untouched.file_path, ghost, "NULL 哈希的行不该被当成同曲改写");
    }

    /// 软删的行不算判重候选：已删除的歌不该挡住同一个文件重新入库。
    #[test]
    fn soft_deleted_song_does_not_block_a_new_copy() {
        let env = Env::new("scan-dedup-soft-deleted");
        let root_b = env._dir.path().join("lib-b");
        std::fs::create_dir_all(&root_b).expect("建第二个库根");
        let gone = env.root.join("gone.mp3");
        let keep = env.root.join("keep.mp3");
        copy_fixture(FIXTURE_A, &gone);
        copy_fixture(FIXTURE_B, &keep);
        // 落库用的规范路径要先存下来：文件删掉之后 canonicalize 就拿不到了
        let gone_stored = canonical(&gone);

        // 1) 两份都入库，然后把 gone.mp3 从磁盘上删掉 → 那一行被标记软删
        assert_eq!(env.service().scan().expect("首扫").added, 2);
        std::fs::remove_file(&gone).expect("删掉第一份");
        assert_eq!(env.service().scan().expect("二扫").marked_deleted, 1);

        // 2) 同一份音频换个目录重新出现：判重查询不带软删行 → 应当照常入库
        let reborn = root_b.join("reborn.mp3");
        copy_fixture(FIXTURE_A, &reborn);
        let service = LibraryService::new(
            Arc::clone(&env.pool),
            vec![
                env.root.to_string_lossy().into_owned(),
                root_b.to_string_lossy().into_owned(),
            ],
        );
        let report = service.scan().expect("三扫");
        assert_eq!(report.added, 1, "软删的行不该把新文件判成重复");
        assert_eq!(report.deduped, 0);
        assert_eq!(report.marked_deleted, 0);

        let conn = env.conn();
        assert_eq!(songs_repo::count(&conn, false).expect("在库计数"), 2);
        assert_eq!(songs_repo::count_deleted(&conn).expect("软删计数"), 1);
        assert_eq!(songs_repo::count(&conn, true).expect("含软删计数"), 3);
        // 两行确实是同一份音频（同一 audio_hash），只是软删那行不参与判重
        let reborn_row = songs_repo::find_by_file_path(&conn, &canonical(&reborn))
            .expect("查")
            .expect("重新出现的文件必须入库");
        let deleted_row = songs_repo::find_by_file_path(&conn, &gone_stored)
            .expect("查软删行")
            .expect("软删行必须还在（文件回来时还要靠它 restore）");
        assert!(deleted_row.deleted_at.is_some());
        assert_eq!(
            deleted_row.audio_hash, reborn_row.audio_hash,
            "两行本来就该是同一份音频"
        );
    }

    /// 胜负函数本身：码率 → added_at → id，平局归先入库者。
    #[test]
    fn dedup_winner_rule_is_bitrate_then_added_at_then_id() {
        let rank = |bitrate_bps, added_at, id| DedupRank {
            bitrate_bps,
            added_at,
            id,
        };

        // 1) 码率高者胜
        assert!(rank(Some(320_000), 9, 9).wins_over(&rank(Some(128_000), 1, 1)));
        // 2) 码率相同 → added_at 小者胜
        assert!(rank(Some(320_000), 1, 9).wins_over(&rank(Some(320_000), 2, 1)));
        // 3) added_at 也相同 → id 小者胜（最终兜底）
        assert!(rank(Some(320_000), 5, 1).wins_over(&rank(Some(320_000), 5, 2)));
        // 未知码率是最差，不能让 NULL 靠「先入库」胜出
        assert!(rank(Some(1), 9, 9).wins_over(&rank(None, 1, 1)));
        assert!(!rank(None, 1, 1).wins_over(&rank(Some(1), 9, 9)));
        // 完全相同是谁都不赢，避免抖动
        assert!(!rank(Some(320_000), 5, 1).wins_over(&rank(Some(320_000), 5, 1)));
        // 新文件在平局时永远判负（added_at 是「现在」、id 是 MAX）
        let incumbent = rank(Some(320_000), 5, 1);
        assert!(!DedupRank::newcomer(Some(320_000)).wins_over(&incumbent));
        assert!(incumbent.wins_over(&DedupRank::newcomer(Some(320_000))));
        // 码率更高时新文件胜
        assert!(DedupRank::newcomer(Some(320_000)).wins_over(&rank(Some(128_000), 5, 1)));
    }
}


