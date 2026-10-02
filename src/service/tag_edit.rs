//! 手工编辑标签（Web 端）—— **预览 → 应用** 两段式，复用 CLI `write` 的那套差异计算。
//!
//! ## 为什么天生是两段
//!
//! 写标签是本项目**最危险**的操作：`atomic_replace` 是 copy → tmp → verify → rename，
//! **没有备份、不可撤销**（和刮削写回原文件是同一件事）。所以：
//!
//! * [`TagEditService::preview`] 只算差异，**一个字节都不碰**（等价 CLI 的 `--preview`）；
//! * [`TagEditService::apply`] 才落盘，且**先自写登记再写**，写完重新读回文件同步 DB。
//! * 路由层的 `dry_run` **默认为 true** —— 忘了传也不会误写。
//!
//! ## 复用清单（刻意不另写一套，抄一遍必然漂移）
//!
//! | 复用什么 | 为什么 |
//! |---|---|
//! | `tag::write::intent` | `WritableFields` / `merge_fields` / `preview_view` / `diff_fields` 是 CLI 与 Web 共用的能力层 |
//! | `service::library::build_song` | 「标签 → songs 行」的**唯一**映射（含 search_text、格式回退） |
//! | `service::library::upsert_album` | 专辑换名的唯一正确处理（年份只补不覆盖、冲突复用行） |
//! | `watcher::SelfWriteRegistry` | 不登记的话 watcher 会把这次写回当成新文件 |

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use crate::db::models::Song;
use crate::db::pool::DbPool;
use crate::db::repos::songs;
use crate::service::library::{build_song, upsert_album};
use crate::tag::read::{read_tags, AudioMetadata};
use crate::tag::write::intent::{
    diff_fields, merge_fields, preview_view, DiffLine, WritableFields,
};
use crate::tag::write::write_tags;
use crate::watcher::SelfWriteRegistry;

/// 编辑过程中的失败。**不把底层路径透给客户端**（部署结构），
/// 文件系统 / 标签引擎的原始报错只进日志，对外只给中文摘要。
#[derive(Debug)]
pub enum TagEditError {
    /// 曲目不存在（或已软删）
    NotFound,
    /// 读文件标签失败
    Read(String),
    /// 写文件标签失败（格式不支持、权限、磁盘……）
    Write(String),
    /// 数据库失败
    Db(String),
    /// 写前准备（备份等）失败
    Io(String),
}

impl std::fmt::Display for TagEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TagEditError::NotFound => write!(f, "曲目不存在"),
            TagEditError::Read(m) => write!(f, "读取标签失败：{m}"),
            TagEditError::Write(m) => write!(f, "写入标签失败：{m}"),
            TagEditError::Db(m) => write!(f, "数据库操作失败：{m}"),
            TagEditError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for TagEditError {}

/// 封面操作。
///
/// 为什么要单独记一笔：标签引擎的 `diff_fields` 对封面**只比张数**
/// （`("cover", before.pictures.len(), after.cover_count)`）。把一张封面换成另一张是
/// `1 → 1`，**一行 diff 都不会产生** —— `apply` 于是判定「无变化」而根本不写文件。
/// 所以封面操作必须在 diff 之外单独标记，两处一起参与「有没有变化」的判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverOp {
    /// 换一张新的
    Replace,
    /// 删掉封面
    Remove,
}

/// 编辑结果。
///
/// `diffs` 是**给人看的**（`before → after` 都已经是拼好的字符串），与 CLI
/// `write --preview` 的输出同源 —— 前端只做展示，不再自己比字段。
#[derive(Debug)]
pub struct EditResult {
    pub song_id: i64,
    /// 只是文件名，**不是绝对路径** —— 对用户的用途是「确认改的是哪个文件」。
    pub file_name: String,
    pub diffs: Vec<DiffLine>,
    /// 封面操作（换 / 删）。没有就是 None。
    pub cover_op: Option<CoverOp>,
    /// 真的动过盘没有：preview 恒 false；apply 在「没有任何字段变化」时也是 false
    /// （文件重写不可撤销，不该做无用功）。
    pub applied: bool,
}

impl EditResult {
    /// 有没有任何改动。**必须把封面操作也算进来** —— 见 [`CoverOp`] 的说明。
    pub fn has_changes(&self) -> bool {
        !self.diffs.is_empty() || self.cover_op.is_some()
    }
}

pub struct TagEditService {
    db: Arc<DbPool>,
    self_write: Arc<SelfWriteRegistry>,
}

impl TagEditService {
    pub fn new(db: Arc<DbPool>, self_write: Arc<SelfWriteRegistry>) -> Self {
        Self { db, self_write }
    }

    /// 读库里的那一行（需要 `file_path`）。
    fn load_song(&self, song_id: i64) -> Result<Song, TagEditError> {
        let conn = self.db.acquire().map_err(db_err)?;
        match songs::get(&conn, song_id, false).map_err(db_err)? {
            Some(song) => Ok(song),
            None => Err(TagEditError::NotFound),
        }
    }

    /// 当前**文件**标签 + 库里的行。
    ///
    /// 编辑器显示原值用这个而不是 DB 值：**文件才是真源**，DB 只是给搜索 / 列表用的镜像。
    /// （`composers` / `comment` / `track_total` 这类字段压根没有 DB 列，只能从文件读。）
    pub fn current(&self, song_id: i64) -> Result<(Song, AudioMetadata), TagEditError> {
        let song = self.load_song(song_id)?;
        let meta = read_tags(Path::new(&song.file_path))
            .map_err(|e| TagEditError::Read(e.to_string()))?;
        Ok((song, meta))
    }

    /// 只算差异，**一个字节都不写**。
    pub fn preview(
        &self,
        song_id: i64,
        fields: WritableFields,
    ) -> Result<EditResult, TagEditError> {
        let (song, before) = self.current(song_id)?;
        Ok(self.diff_result(&song, &before, fields))
    }

    /// 落盘：写文件标签 → **重新读回** → 同步 DB。
    ///
    /// `backup` 为真时先复制一份 `<file>.bak`（与 CLI 的 `--bak` 同语义）。
    pub fn apply(
        &self,
        song_id: i64,
        fields: WritableFields,
        backup: bool,
    ) -> Result<EditResult, TagEditError> {
        let (song, before) = self.current(song_id)?;
        let result = self.diff_result(&song, &before, fields.clone());
        // 没有变化就不写盘、不动库。重写文件不可撤销，不做无用功。
        // ⚠️ 判断必须包含封面操作：换封面是 1 → 1，diffs 是空的。
        if !result.has_changes() {
            return Ok(result);
        }

        let intent = merge_fields(fields);
        let path = PathBuf::from(&song.file_path);

        if backup {
            let bak = format!("{}.bak", song.file_path);
            std::fs::copy(&path, &bak)
                .map_err(|e| TagEditError::Io(format!("备份失败，已中止（原文件未改动）：{e}")))?;
        }

        // 自写抑制：**必须先登记再写**，否则 watcher 会把这次写回当成新文件。
        self.self_write.note_write(&path);
        write_tags(&path, &intent).map_err(|e| TagEditError::Write(e.to_string()))?;

        // 写完**重新读回文件**，拿它当唯一真源更新 DB。
        // 为什么不拿 intent 手工映射成 Song：unset / 多值 / 专辑换名 这些分支在
        // build_song + upsert_album 里已经有唯一一份正确实现，抄一遍必然漂移。
        let fresh = read_tags(&path).map_err(|e| TagEditError::Read(e.to_string()))?;
        self.sync_db(&song, &fresh, &intent)?;

        Ok(EditResult { applied: true, ..result })
    }

    /// 用「应用后」的视图算差异。`WritableFields` 会被消费，调用方要 clone。
    fn diff_result(&self, song: &Song, before: &AudioMetadata, fields: WritableFields) -> EditResult {
        // 封面操作要在 merge_fields 消费 fields 之前取出来。
        let cover_op = if fields.unset_cover {
            Some(CoverOp::Remove)
        } else if fields.replace_cover.is_some() {
            Some(CoverOp::Replace)
        } else {
            None
        };
        let intent = merge_fields(fields);
        let diffs = diff_fields(before, &preview_view(before, &intent));
        EditResult {
            song_id: song.id,
            file_name: file_name_of(&song.file_path),
            diffs,
            cover_op,
            applied: false,
        }
    }

    /// 把「应用后的文件标签」同步进数据库。
    ///
    /// 歌词列有个特例：**刮削来的歌词只入 DB、从不写文件**（见 `service::scrape`）。
    /// 所以不能因为「刚重新读了文件、文件里没歌词」就把库里那份抹掉 ——
    /// 只有在本次**确实改了**对应的歌词字段时才用文件值覆盖。
    ///
    /// 两种歌词**各自判断**：只改纯歌词时绝不能动同步歌词那一列（反之亦然）。
    fn sync_db(
        &self,
        old: &Song,
        fresh: &AudioMetadata,
        intent: &crate::tag::write::Id3EditMeta,
    ) -> Result<(), TagEditError> {
        let touched = |field: &str, val_set: bool| {
            val_set || intent.unset_fields.iter().any(|k| k == field)
        };
        let lyrics_touched = touched("lyrics", intent.lyrics.is_some());
        let timed_touched = touched("lyricsTimed", intent.lyrics_timed.is_some());

        let mut conn = self.db.acquire().map_err(db_err)?;
        let tx = conn.transaction().map_err(db_err)?;

        let album_id = upsert_album(&tx, fresh).map_err(db_err)?;
        let (size, mtime) = stat(&old.file_path);
        // audio_hash 沿用原值：标签变了，**裸音频没变**，重算既费 CPU 又没意义。
        let mut updated = build_song(
            old.id,
            old.file_path.clone(),
            size,
            mtime,
            fresh,
            album_id,
            old.audio_hash.clone(),
        );
        // build_song 的默认值面向「新入库」（scrape_status = Pending / added_at = 0）。
        // 这些列 `update_tags` 目前并不写，但仍然显式带上原值 ——
        // 将来那个 UPDATE 扩列时不至于静默把它们重置掉。
        updated.scrape_status = old.scrape_status;
        updated.scrape_error = old.scrape_error.clone();
        updated.added_at = old.added_at;
        if !lyrics_touched {
            updated.lyrics = old.lyrics.clone();
        }
        if !timed_touched {
            updated.timed_lyrics = old.timed_lyrics.clone();
        }

        songs::update_tags(&tx, &updated).map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(())
    }
}

fn db_err(e: impl std::fmt::Display) -> TagEditError {
    TagEditError::Db(e.to_string())
}

/// 文件名（含扩展名）。取不到就退回整串 —— 这里只用于给用户看「改的是哪个文件」。
fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

/// 文件的 (字节数, 修改时间毫秒)。取不到就给 (0, None) —— 与扫描侧同样不致命。
fn stat(path: &str) -> (i64, Option<i64>) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (0, None);
    };
    let size = i64::try_from(meta.len()).unwrap_or(0);
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok());
    (size, mtime)
}
