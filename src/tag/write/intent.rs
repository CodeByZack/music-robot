//! 字段意图组装与差异计算 —— 移植自 src/tag/write/intent.ts（局部编辑语义核心）
//!
//! 分层：这是**能力层**。CLI 的 `--title/--unset` 只是它的一种外壳形态，Web 直接调用本模块。
use std::collections::HashSet;
use std::str::FromStr;

use super::super::read::{AudioMetadata, Picture};

/// `--unset` 白名单。未知键必须明确报错（ARCHITECTURE §11.4），不能静默忽略。
pub const UNSET_KEYS: &[&str] = &[
    "title", "artist", "album", "album-artist", "track", "track-total", "disc", "disc-total",
    "year", "genre", "composer", "comment", "lyrics", "lyrics-timed",
];
pub fn is_unset_key(k: &str) -> bool { UNSET_KEYS.contains(&k) }

/// 能力层写入契约 = `Id3EditMeta`（TS 里 WriteMeta extends Id3EditMeta 是同一形态）。
///
/// ⚠️ 早期版本在这里另定义了一份 `WriteMeta` 结构体，与 writer 实际使用的
/// `Id3EditMeta` 重复——`merge_fields` 产出的类型根本喂不进 `write_tags`。
/// 已合并为类型别名，避免两套契约漂移。
pub use super::id3v2_editor::Id3EditMeta as WriteMeta;

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
    // --unset 用 CLI 名（kebab-case），unset_fields 用能力层名（camelCase）
    let map_unset: &[(&str, &str)] = &[
        ("title", "title"), ("artist", "artists"), ("album", "albums"),
        ("album-artist", "albumArtist"), ("track", "track"), ("track-total", "trackTotal"),
        ("disc", "disc"), ("disc-total", "discTotal"), ("year", "year"),
        ("genre", "genres"), ("composer", "composers"), ("comment", "comment"),
        ("lyrics", "lyrics"), ("lyrics-timed", "lyricsTimed"),
    ];
    let mut add_unset = |cli: &str| {
        let Some((_, cap)) = map_unset.iter().find(|(k, _)| *k == cli) else { return };
        if unset.contains(cli) && !out.unset_fields.iter().any(|x| x == cap) {
            out.unset_fields.push(cap.to_string());
        }
    };

    // 逐项照搬 TS mergeFields 的分支顺序：unset 命中则记删除，否则「给了值才写」。
    for (cli, cap) in map_unset {
        let set_value = match *cli {
            "title" => f.title.is_some(), "artist" => f.artists.is_some(),
            "album" => f.albums.is_some(), "album-artist" => f.album_artist.is_some(),
            "track" => f.track.is_some(), "track-total" => f.track_total.is_some(),
            "disc" => f.disc.is_some(), "disc-total" => f.disc_total.is_some(),
            "year" => f.year.is_some(), "genre" => f.genres.is_some(),
            "composer" => f.composers.is_some(), "comment" => f.comment.is_some(),
            "lyrics" => f.lyrics.is_some(), "lyrics-timed" => f.lyrics_timed.is_some(),
            _ => false,
        };
        if unset.contains(*cli) { add_unset(cli); } else if set_value {
            match *cap {
                "title" => out.title = f.title.clone(),
                "artists" => out.artists = f.artists.clone(),
                "albums" => out.albums = f.albums.clone(),
                "albumArtist" => out.album_artist = f.album_artist.clone(),
                "track" => out.track = f.track,
                "trackTotal" => out.track_total = f.track_total,
                "disc" => out.disc = f.disc,
                "discTotal" => out.disc_total = f.disc_total,
                "year" => out.year = f.year.clone(),
                "genres" => out.genres = f.genres.clone(),
                "composers" => out.composers = f.composers.clone(),
                "comment" => out.comment = f.comment.clone(),
                "lyrics" => out.lyrics = f.lyrics.clone(),
                "lyricsTimed" => out.lyrics_timed = f.lyrics_timed.clone(),
                _ => {}
            }
        }
    }
    if f.unset_cover {
        if !out.unset_fields.iter().any(|x| x == "pictures") { out.unset_fields.push("pictures".into()) }
    } else if f.replace_cover.is_some() {
        out.pictures = Some(vec![f.replace_cover.clone().unwrap()]);
    }
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
        ("discTotal".into(), fmt_num(&before.disc_total), fmt_num(&after.disc_total)),
        ("year".into(), fmt_opt(&before.year), fmt_opt(&after.year)),
        ("genre".into(), fmt_list(&before.genres), after.genres.clone()),
        ("composer".into(), fmt_list(&before.composers), after.composers.clone()),
        ("comment".into(), fmt_opt(&before.comment), fmt_opt(&after.comment)),
        // 两种歌词各比各的 —— 以前这里把 timed 当成 lyrics 的兜底，于是「只换了带时间轴那份」
        // 会显示成「歌词」变了，看不出到底动的是哪一个字段。
        ("lyrics".into(), fmt_opt(&before.lyrics), fmt_opt(&after.lyrics)),
        ("lyricsTimed".into(), fmt_opt(&before.lyrics_timed), fmt_opt(&after.lyrics_timed)),
        ("cover".into(), fmt(&before.pictures.len().to_string()), fmt(&after.cover_count.to_string())),
    ];
    rows.into_iter().filter(|(_, a, b)| a != b).map(|(key, before, after)| DiffLine { key, before, after }).collect()
}

/// diffFields 的 after 侧统一视图（AudioMetadata 与 WriteMeta 都能喂进来）
#[derive(Debug, Clone, Default)]
pub struct AfterView {
    pub title: Option<String>, pub artists: String, pub albums: String,
    pub album_artist: Option<String>, pub track: Option<i64>, pub track_total: Option<i64>,
    pub disc: Option<i64>, pub disc_total: Option<i64>, pub year: Option<String>,
    pub genres: String, pub composers: String,
    pub comment: Option<String>, pub lyrics: Option<String>, pub lyrics_timed: Option<String>,
    pub cover_count: usize,
}
impl AfterView { pub fn track_str(&self) -> String { match self.track { Some(t) => fmt(&format!("{}{}", t, self.track_total.map(|x| format!("/{x}")).unwrap_or_default())), None => "(无)".into() } } }

impl From<&AudioMetadata> for AfterView {
    fn from(m: &AudioMetadata) -> Self {
        AfterView { title: m.title.clone(), artists: fmt_list(&m.artists), albums: fmt_list(&m.albums),
            album_artist: m.album_artist.clone(), track: m.track, track_total: m.track_total, disc: m.disc, disc_total: m.disc_total,
            year: m.year.clone(), genres: fmt_list(&m.genres), composers: fmt_list(&m.composers),
            comment: m.comment.clone(), lyrics: m.lyrics.clone(), lyrics_timed: m.lyrics_timed.clone(),
            cover_count: m.pictures.len() }
    }
}
impl From<&WriteMeta> for AfterView {
    fn from(w: &WriteMeta) -> Self {
        if w.blank_all { return AfterView::default() }
        let list = |o: &Option<Vec<String>>| o.clone().map(|v| fmt_list(&v)).unwrap_or_else(|| "(无)".into());
        AfterView {
            title: w.title.clone(), artists: list(&w.artists), albums: list(&w.albums),
            album_artist: w.album_artist.clone(), track: w.track, track_total: w.track_total, disc: w.disc, disc_total: w.disc_total,
            year: w.year.clone(), genres: list(&w.genres), composers: list(&w.composers),
            comment: w.comment.clone(), lyrics: w.lyrics.clone(), lyrics_timed: w.lyrics_timed.clone(),
            cover_count: w.pictures.as_ref().map(|p| p.len()).unwrap_or(0),
        }
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

/// 展示视图：current 之上**只叠加点名字段**（未点名的保持原值，不显示成"(无)"）。
/// 写层不依赖此视图，仅用于 --preview 的 diff 展示。
pub fn preview_view(current: &AudioMetadata, m: &WriteMeta) -> AfterView {
    if m.blank_all { return AfterView::default() }
    let mut v: AfterView = current.into();
    let u = |field: &str| m.unset_fields.iter().any(|x| x == field);
    let opt_field = |slot: &mut Option<String>, val: &Option<String>, field: &str| {
        if let Some(x) = val { *slot = Some(x.clone()); } else if u(field) { *slot = Some(String::new()); }
    };
    let list_field = |slot: &mut String, val: &Option<Vec<String>>, field: &str| {
        if let Some(x) = val { *slot = fmt_list(x); } else if u(field) { *slot = "(无)".into(); }
    };
    let num_field = |slot: &mut Option<i64>, val: &Option<i64>, field: &str| {
        if let Some(x) = val { *slot = Some(*x); } else if u(field) { *slot = None; }
    };

    opt_field(&mut v.title, &m.title, "title");
    list_field(&mut v.artists, &m.artists, "artists");
    list_field(&mut v.albums, &m.albums, "albums");
    opt_field(&mut v.album_artist, &m.album_artist, "albumArtist");
    num_field(&mut v.track, &m.track, "track");
    num_field(&mut v.track_total, &m.track_total, "trackTotal");
    num_field(&mut v.disc, &m.disc, "disc");
    num_field(&mut v.disc_total, &m.disc_total, "discTotal");
    opt_field(&mut v.year, &m.year, "year");
    list_field(&mut v.genres, &m.genres, "genres");
    list_field(&mut v.composers, &m.composers, "composers");
    opt_field(&mut v.comment, &m.comment, "comment");
    opt_field(&mut v.lyrics, &m.lyrics, "lyrics");
    opt_field(&mut v.lyrics_timed, &m.lyrics_timed, "lyricsTimed");
    match &m.pictures {
        Some(pics) if !pics.is_empty() => v.cover_count = pics.len(),
        Some(_) => v.cover_count = 0,
        None if u("pictures") => v.cover_count = 0,
        _ => {}
    }
    v
}

/// blank 的 diff 基线：全空视图（对应 TS blankView，未使用 file 参数）
pub fn blank_view() -> AfterView { AfterView::default() }

use serde_json::{json, Value};

/// --preview 的 JSON 输出（TS runWrite/runBlank 的 preview 分支）
pub fn format_json_diff(file: &str, preview: bool, diffs: &[DiffLine]) -> String {
    json!({ "file": file, "preview": preview, "diffs": diffs.iter().map(|d| json!({
        "key": d.key, "before": d.before, "after": d.after
    })).collect::<Vec<_>>(), "audioHashOk": true }).to_string()
}

/// 完整 JSON 输出（apply 后：before / after / diffs / audioHashOk）
pub fn format_json_diff_full(
    file: &str,
    before: &AudioMetadata,
    after: &AudioMetadata,
    diffs: &[DiffLine],
) -> String {
    let dj = |m: &AudioMetadata| crate::tag::read::read_json(m, file, &[]);
    let parse = |x: &str| Value::from_str(x).unwrap_or(Value::Null);
    json!({
        "file": file,
        "before": parse(&dj(before)),
        "after": parse(&dj(after)),
        "diffs": diffs.iter().map(|d| json!({ "key": d.key, "before": d.before, "after": d.after })).collect::<Vec<_>>(),
        "audioHashOk": true,
    }).to_string()
}
