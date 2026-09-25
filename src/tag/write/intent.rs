//! 字段意图组装与差异计算 —— 移植自 src/tag/write/intent.ts（局部编辑语义核心）
//!
//! 分层：这是**能力层**。CLI 的 `--title/--unset` 只是它的一种外壳形态，Web 直接调用本模块。
use std::collections::HashSet;

use super::super::read::{AudioMetadata, Picture};

/// `--unset` 白名单。未知键必须明确报错（ARCHITECTURE §11.4），不能静默忽略。
pub const UNSET_KEYS: &[&str] = &[
    "title", "artist", "album", "album-artist", "track", "track-total", "disc", "disc-total",
    "year", "genre", "composer", "comment", "lyrics", "lyrics-timed",
];
pub fn is_unset_key(k: &str) -> bool { UNSET_KEYS.contains(&k) }

/// 能力层写入契约（对应 TS WriteMeta + Id3EditMeta）。
///
/// ⚠️ 与 `WritableFields` 是**两个不同类型**——TS 里靠运行时检查防误传（round6 P2-3），
/// Rust 由类型系统直接区分，那处防御代码不需要移植。
#[derive(Debug, Clone, Default)]
pub struct WriteMeta {
    pub title: Option<String>,
    pub artists: Option<Vec<String>>,
    pub albums: Option<Vec<String>>,
    pub album_artist: Option<String>,
    pub track: Option<i64>,
    pub track_total: Option<i64>,
    pub disc: Option<i64>,
    pub disc_total: Option<i64>,
    pub year: Option<String>,
    pub genres: Option<Vec<String>>,
    pub composers: Option<Vec<String>>,
    pub comment: Option<String>,
    pub lyrics: Option<String>,
    pub lyrics_timed: Option<String>,
    /// 要删除的能力层字段名（'title'/'artists'/…）
    pub unset: Vec<String>,
    pub unset_cover: bool,
    pub replace_cover: Option<Picture>,
    /// blank：重建成标准空标签（ID3v2.4 零帧 + 空 ID3v1；APEv2 一并清除）
    pub blank_all: bool,
}

/// CLI 形态的原始意图（未点名的 = None，与"点名设为空"可区分）
#[derive(Debug, Clone, Default)]
pub struct WritableFields {
    pub title: Option<String>,
    pub artists: Option<Vec<String>>,
    pub albums: Option<Vec<String>>,
    pub album_artist: Option<String>,
    pub track: Option<i64>,
    pub track_total: Option<i64>,
    pub disc: Option<i64>,
    pub disc_total: Option<i64>,
    pub year: Option<String>,
    pub genres: Option<Vec<String>>,
    pub composers: Option<Vec<String>>,
    pub comment: Option<String>,
    pub lyrics: Option<String>,
    pub lyrics_timed: Option<String>,
    pub unset: Vec<String>,
    pub unset_cover: bool,
    pub replace_cover: Option<Picture>,
}

/// 意图 → 能力层元数据。**unset 优先于同名赋值**（TS :43-70 的 if/else-if 顺序）。
pub fn merge_fields(f: WritableFields) -> WriteMeta {
    let mut out = WriteMeta::default();
    let unset: HashSet<String> = f.unset.iter().cloned().collect();
    let mut push_unset = |flag: &str, field: &str| {
        if unset.contains(flag) && !out.unset.iter().any(|x| x == field) {
            out.unset.push(field.to_string());
        }
    };

    // 逐项照搬 TS :43-70 的分支顺序：unset 命中则记删除，否则"给了值才写"。
    if unset.contains("title") { push_unset("title", "title"); } else if f.title.is_some() { out.title = f.title; }
    if unset.contains("artist") { push_unset("artist", "artists"); } else if f.artists.is_some() { out.artists = f.artists; }
    if unset.contains("album") { push_unset("album", "albums"); } else if f.albums.is_some() { out.albums = f.albums; }
    if unset.contains("album-artist") { push_unset("album-artist", "albumArtist"); } else if f.album_artist.is_some() { out.album_artist = f.album_artist; }
    if unset.contains("track") { push_unset("track", "track"); } else if f.track.is_some() { out.track = f.track; }
    if unset.contains("track-total") { push_unset("track-total", "trackTotal"); } else if f.track_total.is_some() { out.track_total = f.track_total; }
    if unset.contains("disc") { push_unset("disc", "disc"); } else if f.disc.is_some() { out.disc = f.disc; }
    if unset.contains("disc-total") { push_unset("disc-total", "discTotal"); } else if f.disc_total.is_some() { out.disc_total = f.disc_total; }
    if unset.contains("year") { push_unset("year", "year"); } else if f.year.is_some() { out.year = f.year; }
    if unset.contains("genre") { push_unset("genre", "genres"); } else if f.genres.is_some() { out.genres = f.genres; }
    if unset.contains("composer") { push_unset("composer", "composers"); } else if f.composers.is_some() { out.composers = f.composers; }
    if unset.contains("comment") { push_unset("comment", "comment"); } else if f.comment.is_some() { out.comment = f.comment; }
    if unset.contains("lyrics") { push_unset("lyrics", "lyrics"); } else if f.lyrics.is_some() { out.lyrics = f.lyrics; }
    if unset.contains("lyrics-timed") { push_unset("lyrics-timed", "lyricsTimed"); } else if f.lyrics_timed.is_some() { out.lyrics_timed = f.lyrics_timed; }

    if f.unset_cover { out.unset_cover = true } else if f.replace_cover.is_some() { out.replace_cover = f.replace_cover.clone() }
    out
}

/// 图片 magic 嗅探（长度不足绝不越界——TS 用 buf.length>=n 守卫，此处同样先判长）
pub fn sniff_image_mime(buf: &[u8]) -> Option<String> {
    if buf.len() >= 3 && buf[0] == 0xFF && buf[1] == 0xD8 && buf[2] == 0xFF { return Some("image/jpeg".into()) }
    if buf.len() >= 4 && buf[0] == 0x89 && buf[1] == 0x50 && buf[2] == 0x4E && buf[3] == 0x47 { return Some("image/png".into()) }
    if buf.len() >= 3 && buf[0] == b'G' && buf[1] == b'I' && buf[2] == b'F' { return Some("image/gif".into()) }
    None
}

#[derive(Debug, Clone)]
pub struct DiffLine { pub key: String, pub before: String, pub after: String }

/// 与 TS fmt() 一致：undefined/null/'' → "(无)"；数组 join ' / '；空数组 → "(无)"
fn fmt(v: &str) -> String { if v.is_empty() { "(无)".into() } else { v.to_string() } }
fn fmt_opt(o: &Option<String>) -> String { fmt(o.as_deref().unwrap_or("")) }
fn fmt_list(l: &[String]) -> String { if l.is_empty() { "(无)".into() } else { l.join(" / ") } }
fn fmt_num(o: &Option<i64>) -> String { o.map(|n| n.to_string()).unwrap_or_else(|| "(无)".into()) }
fn track_str(m: &AudioMetadata) -> String {
    match m.track { Some(t) => fmt(&format!("{}{}", t, m.track_total.map(|x| format!("/{x}")).unwrap_or_default())), None => "(无)".into() }
}

/// 计算变化行（只输出真正不同的字段）。after 侧支持 AudioMetadata 或 WriteMeta，
/// 这里统一转成"可显示字符串"再比。
pub fn diff_fields(before: &AudioMetadata, after: &AfterView) -> Vec<DiffLine> {
    let rows: Vec<(String, String, String)> = vec![
        ("title".into(), fmt_opt(&before.title), fmt_opt(&after.title)),
        ("artist".into(), fmt_list(&before.artists), after.artists.clone()),
        ("album".into(), fmt_list(&before.albums), after.albums.clone()),
        ("albumArtist".into(), fmt_opt(&before.album_artist), fmt_opt(&after.album_artist)),
        ("track".into(), track_str(before), after.track_str()),
        ("disc".into(), fmt_num(&before.disc), fmt_num(&after.disc)),
        ("year".into(), fmt_opt(&before.year), fmt_opt(&after.year)),
        ("genre".into(), fmt_list(&before.genres), after.genres.clone()),
        ("composer".into(), fmt_list(&before.composers), after.composers.clone()),
        ("comment".into(), fmt_opt(&before.comment), fmt_opt(&after.comment)),
        ("lyrics".into(), fmt_opt(&before.lyrics.clone().or_else(|| before.lyrics_timed.clone())), fmt_opt(&after.lyrics.clone().or_else(|| after.lyrics_timed.clone()))),
        ("cover".into(), fmt(&before.pictures.len().to_string()), fmt(&after.cover_count.to_string())),
    ];
    rows.into_iter().filter(|(_, a, b)| a != b).map(|(key, before, after)| DiffLine { key, before, after }).collect()
}

/// diffFields 的 after 侧统一视图（AudioMetadata 与 WriteMeta 都能喂进来）
#[derive(Debug, Clone, Default)]
pub struct AfterView {
    pub title: Option<String>, pub artists: String, pub albums: String,
    pub album_artist: Option<String>, pub track: Option<i64>, pub track_total: Option<i64>,
    pub disc: Option<i64>, pub year: Option<String>, pub genres: String, pub composers: String,
    pub comment: Option<String>, pub lyrics: Option<String>, pub lyrics_timed: Option<String>,
    pub cover_count: usize,
}
impl AfterView { pub fn track_str(&self) -> String { match self.track { Some(t) => fmt(&format!("{}{}", t, self.track_total.map(|x| format!("/{x}")).unwrap_or_default())), None => "(无)".into() } } }

impl From<&AudioMetadata> for AfterView {
    fn from(m: &AudioMetadata) -> Self {
        AfterView { title: m.title.clone(), artists: fmt_list(&m.artists), albums: fmt_list(&m.albums),
            album_artist: m.album_artist.clone(), track: m.track, track_total: m.track_total, disc: m.disc,
            year: m.year.clone(), genres: fmt_list(&m.genres), composers: fmt_list(&m.composers),
            comment: m.comment.clone(), lyrics: m.lyrics.clone(), lyrics_timed: m.lyrics_timed.clone(),
            cover_count: m.pictures.len() }
    }
}
impl From<&WriteMeta> for AfterView {
    fn from(w: &WriteMeta) -> Self {
        let list = |o: &Option<Vec<String>>| o.clone().map(|v| fmt_list(&v)).unwrap_or_else(|| "(无)".into());
        AfterView { title: w.title.clone(), artists: list(&w.artists), albums: list(&w.albums),
            album_artist: w.album_artist.clone(), track: w.track, track_total: w.track_total, disc: w.disc,
            year: w.year.clone(), genres: list(&w.genres), composers: list(&w.composers),
            comment: w.comment.clone(), lyrics: w.lyrics.clone(), lyrics_timed: w.lyrics_timed.clone(),
            cover_count: if w.replace_cover.is_some() { 1 } else { 0 } }
    }
}

/// 渲染差异文本（TS formatDiff：含尾行的「音频完整性」提示 + 结尾换行）
pub fn format_diff(diffs: &[DiffLine]) -> String {
    let mut lines: Vec<String> = Vec::new();
    if diffs.is_empty() { lines.push("无变化".into()) }
    else {
        lines.push("变化:".into());
        for d in diffs { lines.push(format!("  {}: {} → {}", d.key, d.before, d.after)) }
    }
    lines.push("音频完整性: apply 时校验（裸流 hash 不变）".into());
    lines.join("\n") + "\n"
}
