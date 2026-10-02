//! 手工编辑标签路由（S26 · 标签编辑）。
//!
//! ## 为什么 GET 和 PATCH 的权限不一样
//!
//! * **读**标签无害 —— 和 `/api/songs/{id}` 同档，登录即可。
//! * **写**标签会**直接覆盖原文件**（`atomic_replace`：copy → tmp → verify → rename，
//!   没有备份、不可撤销）。这与扫描 / 刮削同一量级，所以 **PATCH 仅 admin**
//!   （`AdminUser` 提取器，与 `/api/scrape` 一致）。前端据此隐藏非管理员的编辑入口。
//!
//! ## dry_run 默认 **true**
//!
//! 请求体不写 `dry_run` 时**只算差异、一个字节都不写**（等价 CLI `write --preview`）。
//! 忘了传参数不会误写文件 —— 安全的那一档是默认值，和 `write_files` 默认 false 同一个口径。
//!
//! ## 字段名
//!
//! 读写都用**复数规范名**（与 `AudioMetadata` 一致）：`artists` / `genres` / `composers`。
//! 为了好记，PATCH 也接受 CLI 的单数别名 `artist` / `genre` / `composer`。
//! 值可以是字符串或字符串数组；**`null` 表示清空该字段**（与「不传 = 保持原样」区分开）。

use axum::extract::{Path, State};
use axum::Json;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Value};

use crate::server::auth::{AdminUser, AuthUser};
use crate::server::error::{ApiError, ApiResult};
use crate::server::state::AppState;
use crate::service::{CoverOp, TagEditError};
use crate::tag::read::{AudioMetadata, Picture};
use crate::tag::write::intent::{is_unset_key, sniff_image_mime, WritableFields};

use super::library::parse_id;

/// 封面数据的上限。
///
/// 为什么要设：封面走 base64 传（**不引新依赖** —— `base64` 本来就在，JWT 在用；
/// 也不用开 axum 的 multipart 特性），代价是体积膨胀 1/3。
/// 路由级 body 上限要比它宽（见 `routes/mod.rs` 的 `TAGS_MAX_BODY`）。
pub(crate) const MAX_COVER_BYTES: usize = 8 * 1024 * 1024;

/// 「JSON 字段名 → CLI 风格的 unset 键」对照表。
///
/// 为什么需要两套名字：`WritableFields.unset` 收的是 **CLI 风格**键
/// （`album-artist` / `track-total` …，见 `tag::write::intent::UNSET_KEYS` 与
/// `merge_fields` 的映射），而 HTTP 侧用更自然的蛇形复数名。转换集中在这一张表里，
/// 免得散落在解析代码各处。
const FIELD_TO_UNSET_KEY: &[(&str, &str)] = &[
    ("title", "title"),
    ("artists", "artist"),
    ("album", "album"),
    ("album_artist", "album-artist"),
    ("track", "track"),
    ("track_total", "track-total"),
    ("disc", "disc"),
    ("disc_total", "disc-total"),
    ("year", "year"),
    ("genres", "genre"),
    ("composers", "composer"),
    ("comment", "comment"),
    ("lyrics", "lyrics"),
    ("lyrics_timed", "lyrics-timed"),
];

/// 单数别名 → 规范名（CLI 用单数，Web 用复数，两个都收）。
const ALIASES: &[(&str, &str)] = &[
    ("artist", "artists"),
    ("genre", "genres"),
    ("composer", "composers"),
    ("albumartist", "album_artist"),
    ("tracktotal", "track_total"),
    ("disctotal", "disc_total"),
    ("lyricstimed", "lyrics_timed"),
];

/// 字符串字段（只列真需要单值的；多值字段另走 `str_list`）。
const TEXT_FIELDS: &[&str] = &[
    "title",
    "album",
    "album_artist",
    "year",
    "comment",
    "lyrics",
    "lyrics_timed",
];
/// 多值字段（字符串或字符串数组都收）。
const LIST_FIELDS: &[&str] = &["artists", "genres", "composers"];
/// 数值字段。
const NUM_FIELDS: &[&str] = &["track", "track_total", "disc", "disc_total"];

/// `null` 之外的标量 → 字符串。数字也收（年份写成 `2015` 而不是 `"2015"` 很常见）。
fn as_text(value: &Value, key: &str) -> Result<String, ApiError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(_) => Err(ApiError::bad_request(format!("字段 {key} 期望字符串"))),
        _ => Err(ApiError::bad_request(format!("字段 {key} 期望字符串"))),
    }
}

/// 字符串或字符串数组 → `Vec<String>`。数组里的非字符串项直接报错，不做静默丢弃。
fn as_text_list(value: &Value, key: &str) -> Result<Vec<String>, ApiError> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| as_text(item, key))
            .collect::<Result<Vec<_>, _>>(),
        other => Ok(vec![as_text(other, key)?]),
    }
}

fn as_number(value: &Value, key: &str) -> Result<i64, ApiError> {
    value
        .as_i64()
        .ok_or_else(|| ApiError::bad_request(format!("字段 {key} 期望整数")))
}

/// 请求体 → `WritableFields`。
///
/// 三种语义：**不传 = 保持原样**、**传 null = 清空**、**传值 = 设为该值**。
pub(crate) fn parse_fields(body: Option<&Value>) -> Result<(WritableFields, bool, bool), ApiError> {
    let Some(root) = body.and_then(Value::as_object) else {
        return Err(ApiError::bad_request("请求体必须是一个 JSON 对象"));
    };

    // dry_run 默认 **true**（安全那一档）；backup 默认 false。
    let dry_run = match root.get("dry_run") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(ApiError::bad_request("dry_run 期望布尔值")),
    };
    let backup = match root.get("backup") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(ApiError::bad_request("backup 期望布尔值")),
    };

    // 字段放在 `fields` 子对象里（与 dry_run 等控制项分开，避免名字撞车）。
    let raw = root
        .get("fields")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiError::bad_request("缺少 fields 对象（可以给空对象 {}）"))?;

    let mut fields = WritableFields::default();
    let mut unset: Vec<String> = Vec::new();

    for (raw_key, value) in raw {
        // `unset` 是控制项（在字段区里，因为它是「要清空哪些字段」的列表），
        // 单独在循环外处理 —— 不挡在这里的话会被下面的未知字段校验拒掉。
        if raw_key == "unset" {
            continue;
        }
        // 封面自己一套：`null` = 删除、对象 = 替换（+ base64 数据）。
        if raw_key == "cover" {
            parse_cover(value, &mut fields)?;
            continue;
        }
        let key = ALIASES
            .iter()
            .find(|(alias, _)| alias == raw_key)
            .map(|(_, canonical)| *canonical)
            .unwrap_or(raw_key.as_str());

        // 未知字段明确报错，不静默忽略 —— 否则拼错一个字段名会变成「保存成功了但没生效」。
        if !TEXT_FIELDS.contains(&key) && !LIST_FIELDS.contains(&key) && !NUM_FIELDS.contains(&key) {
            return Err(ApiError::bad_request(format!(
                "未知字段：{raw_key}（可用：{}）",
                [TEXT_FIELDS, LIST_FIELDS, NUM_FIELDS].concat().join(", ")
            )));
        }

        if value.is_null() {
            let unset_key = FIELD_TO_UNSET_KEY
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, cli)| *cli)
                .ok_or_else(|| ApiError::bad_request(format!("字段 {key} 不支持清空")))?;
            unset.push(unset_key.to_string());
            continue;
        }

        match key {
            "title" => fields.title = Some(as_text(value, key)?),
            "album" => fields.albums = Some(vec![as_text(value, key)?]),
            "album_artist" => fields.album_artist = Some(as_text(value, key)?),
            "year" => fields.year = Some(as_text(value, key)?),
            "comment" => fields.comment = Some(as_text(value, key)?),
            "lyrics" => fields.lyrics = Some(as_text(value, key)?),
            "lyrics_timed" => fields.lyrics_timed = Some(as_text(value, key)?),
            "artists" => fields.artists = Some(as_text_list(value, key)?),
            "genres" => fields.genres = Some(as_text_list(value, key)?),
            "composers" => fields.composers = Some(as_text_list(value, key)?),
            "track" => fields.track = Some(as_number(value, key)?),
            "track_total" => fields.track_total = Some(as_number(value, key)?),
            "disc" => fields.disc = Some(as_number(value, key)?),
            "disc_total" => fields.disc_total = Some(as_number(value, key)?),
            _ => return Err(ApiError::bad_request(format!("未处理的字段：{key}"))),
        }
    }

    // 显式的 `unset` 数组（与「传 null」等价，给批量清空用）。
    if let Some(list) = raw.get("unset") {
        let items = list
            .as_array()
            .ok_or_else(|| ApiError::bad_request("unset 期望字符串数组"))?;
        for item in items {
            let name = item
                .as_str()
                .ok_or_else(|| ApiError::bad_request("unset 期望字符串数组"))?;
            let canonical = ALIASES
                .iter()
                .find(|(alias, _)| *alias == name)
                .map(|(_, c)| *c)
                .unwrap_or(name);
            let unset_key = FIELD_TO_UNSET_KEY
                .iter()
                .find(|(n, _)| *n == canonical)
                .map(|(_, cli)| *cli)
                .ok_or_else(|| ApiError::bad_request(format!("unset 里的未知字段：{name}")))?;
            unset.push(unset_key.to_string());
        }
    }

    // 交给能力层再校验一次：`merge_fields` 只认 UNSET_KEYS，这里挡住拼错的名字。
    for key in &unset {
        if !is_unset_key(key) {
            return Err(ApiError::bad_request(format!("不支持清空的字段：{key}")));
        }
    }
    fields.unset = unset;

    Ok((fields, dry_run, backup))
}

/// `cover` 字段：
///
/// * `null`                         —— **删除**封面
/// * `{ "data": "<base64>" }`      —— **替换**封面（mime 按**文件头**嗅探，不信声明）
/// * `{ "data": "data:image/jpeg;base64,..." }` —— 也收 data URL
///   （前端 `FileReader.readAsDataURL` 拿到的就是这个形状，省得前端再切一刀）
fn parse_cover(value: &Value, fields: &mut WritableFields) -> Result<(), ApiError> {
    if value.is_null() {
        fields.unset_cover = true;
        return Ok(());
    }
    let obj = value
        .as_object()
        .ok_or_else(|| ApiError::bad_request("cover 期望对象或 null"))?;
    let mut data = obj
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("cover.data 期望 base64 字符串"))?;

    // data URL 前缀剥掉（顺便拿到声明的 mime，但下面以魔数为准）。
    if let Some(rest) = data.strip_prefix("data:") {
        if let Some((_, body)) = rest.split_once(',') {
            data = body;
        }
    }
    let bytes = B64
        .decode(data.trim())
        .map_err(|e| ApiError::bad_request(format!("封面的 base64 解不开：{e}")))?;
    if bytes.is_empty() {
        return Err(ApiError::bad_request("封面数据是空的"));
    }
    if bytes.len() > MAX_COVER_BYTES {
        return Err(ApiError::bad_request(format!(
            "封面太大（{:.1} MB），上限 {} MB",
            bytes.len() as f64 / 1048576.0,
            MAX_COVER_BYTES / 1048576
        )));
    }
    // 以魔数判定类型，不信请求里声明的 —— 与 CLI `--cover` 同一口径。
    let mime = sniff_image_mime(&bytes)
        .ok_or_else(|| ApiError::bad_request("封面不是 JPEG / PNG / GIF（按文件头判断）"))?;
    fields.replace_cover = Some(Picture {
        mime_type: mime,
        pic_type: 3, // 3 = Cover (front)，与 CLI 一致
        description: "front".to_string(),
        data: bytes,
    });
    Ok(())
}

/// 把标签 JSON 化（编辑器初值）。
///
/// **歌词以文件为准**：这个编辑器改的就是文件，
/// 展示库里的值再提交就会把文件里原本的歌词静默覆盖掉（上一版就是这么错的）。
///
/// 库里那份**只在和文件不同时**才另外给（`db_lyrics`），供界面提示
/// 「库里还有一份不一样的歌词，要用它吗」—— 刮削的歌词可能只入过库、从没写过文件。
fn tags_json(meta: &AudioMetadata, db_lyrics: Option<&str>) -> Value {
    let file_lyrics = meta.lyrics.as_deref().unwrap_or("").trim().to_string();
    let db_text = db_lyrics.unwrap_or("").trim();
    // 内容相同就不重复传（歌词动辄几百字）。
    let db_extra = if !db_text.is_empty() && db_text != file_lyrics {
        Some(db_text.to_string())
    } else {
        None
    };

    json!({
        "title": meta.title,
        "artists": meta.artists,
        "album": meta.albums.iter().map(|s| s.trim()).find(|s| !s.is_empty()),
        "album_artist": meta.album_artist,
        "track": meta.track,
        "track_total": meta.track_total,
        "disc": meta.disc,
        "disc_total": meta.disc_total,
        "year": meta.year,
        "genres": meta.genres,
        "composers": meta.composers,
        "comment": meta.comment,
        "lyrics": file_lyrics,
        "lyrics_source": if meta.lyrics.as_deref().unwrap_or("").trim().is_empty() { "none" } else { "file" },
        "db_lyrics": db_extra,
        "lyrics_timed": meta.lyrics_timed,
        "has_cover": !meta.pictures.is_empty(),
        // 封面只给「有没有」与类型，不给字节 —— 图片由 /api/songs/{id}/cover 取。
        "cover_mime": meta.pictures.first().map(|p| p.mime_type.clone()),
    })
}

fn diff_json(result: &crate::service::EditResult) -> Value {
    let diffs: Vec<Value> = result
        .diffs
        .iter()
        .map(|d| json!({ "key": d.key, "before": d.before, "after": d.after }))
        .collect();
    json!({
        "song_id": result.song_id,
        "file_name": result.file_name,
        "diffs": diffs,
        // 封面操作单独给一个字段：引擎的 diff 只比张数，换封面是 1 → 1、没有 diff 行。
        "cover_op": match result.cover_op {
            Some(CoverOp::Replace) => Some("replace"),
            Some(CoverOp::Remove) => Some("remove"),
            None => None,
        },
        "changed": result.has_changes(),
        "applied": result.applied,
    })
}

/// 服务层错误 → HTTP。**不把文件系统原始报错透出去**，只把中文摘要给客户端。
fn map_error(e: TagEditError) -> ApiError {
    match e {
        TagEditError::NotFound => ApiError::not_found("请求的歌曲不存在"),
        TagEditError::Write(msg) => ApiError::bad_request(format!("写入标签失败：{msg}")),
        TagEditError::Read(msg) => ApiError::internal(format!("读取标签失败：{msg}")),
        TagEditError::Db(msg) => ApiError::internal(format!("数据库操作失败：{msg}")),
        TagEditError::Io(msg) => ApiError::internal(msg),
    }
}

/// GET /api/songs/{id}/tags/cover —— **文件内嵌**封面（不是专辑那张）。
///
/// 为什么另开一条而不是复用 `/api/songs/{id}/cover`：那条**优先返回专辑表里的封面**
/// （见 `routes::cover` 的数据来源表）。对列表 / 播放页那样挺好，但对**标签编辑器**
/// 是误导 —— 用户以为在看文件的封面，实际看到的是专辑那张，换掉以后界面也不变。
///
/// 文件里没有内嵌封面 → 404（界面显示「无内嵌封面」并提示专辑有封面）。
pub async fn file_cover(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> ApiResult<axum::response::Response> {
    let id = parse_id(&raw_id)?;
    let found = super::library::run_db(std::sync::Arc::clone(&state.db), move |conn| {
        Ok(crate::db::repos::songs::get(conn, id, false)?)
    })
    .await?;
    let Some(song) = found else {
        return Err(ApiError::not_found("请求的歌曲不存在"));
    };

    let config = std::sync::Arc::clone(&state.config);
    let path = song.file_path;
    let extracted = tokio::task::spawn_blocking(move || {
        super::cover::extract_from_file(&config, &path)
    })
    .await
    .map_err(|join| ApiError::internal(format!("封面提取任务异常退出：{join}")))??;

    match extracted {
        Some(image) => super::cover::cover_response(image, super::cover::CACHE_NO_STORE),
        None => Err(ApiError::not_found("这个文件没有内嵌封面")),
    }
}

/// GET /api/songs/{id}/tags —— 当前**文件**标签（登录即可）。
pub async fn get_tags(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    let service = std::sync::Arc::clone(&state.tag_edit);
    let result = tokio::task::spawn_blocking(move || {
        let (song, meta) = service.current(id)?;
        Ok::<_, TagEditError>((song, meta))
    })
    .await
    .map_err(|e| ApiError::internal(format!("读取标签任务失败：{e}")))?
    .map_err(map_error)?;

    let (song, meta) = result;
    Ok(Json(json!({
        "song_id": song.id,
        "file": {
            // 只给文件名，**不给绝对路径** —— 与 song_json 不暴露 file_path 同口径。
            "name": std::path::Path::new(&song.file_path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(""),
            "format": song.format,
            "size": song.file_size,
        },
        "tags": tags_json(&meta, song.lyrics.as_deref()),
    })))
}

/// PATCH /api/songs/{id}/tags —— 预览（默认）或应用标签改动（**仅 admin**）。
pub async fn patch_tags(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(raw_id): Path<String>,
    body: Option<Json<Value>>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    let (fields, dry_run, backup) = parse_fields(body.as_ref().map(|Json(v)| v))?;

    let service = std::sync::Arc::clone(&state.tag_edit);
    let result = tokio::task::spawn_blocking(move || {
        if dry_run {
            service.preview(id, fields)
        } else {
            service.apply(id, fields, backup)
        }
    })
    .await
    .map_err(|e| ApiError::internal(format!("标签编辑任务失败：{e}")))?
    .map_err(map_error)?;

    // 日志：写盘是重操作，事后必须能查「谁在什么时候改了哪首歌」。
    if result.applied {
        let mut keys: Vec<&str> = result.diffs.iter().map(|d| d.key.as_str()).collect();
        // 封面不在 diffs 里（引擎只比张数，换封面是 1 → 1），漏掉会记成「0 处改动」。
        match result.cover_op {
            Some(crate::service::CoverOp::Replace) => keys.push("cover=替换"),
            Some(crate::service::CoverOp::Remove) => keys.push("cover=移除"),
            None => {}
        }
        crate::serverlog::info(
            "tags",
            format!(
                "手工编辑曲目 {}（{}）：共 {} 处改动：{}",
                result.song_id,
                result.file_name,
                keys.len(),
                if keys.is_empty() {
                    // 理论上到不了这里（applied 就说明有改动），留一句兜底比空着强。
                    "（无）".to_string()
                } else {
                    keys.join("、")
                }
            ),
        );
    }

    Ok(Json(diff_json(&result)))
}

// ─────────────────────────────────────────────────────────────────────────────
// 测试：请求体解析（纯函数，不碰数据库 / 磁盘）
//
// 这一段最容易写错的是**三种语义的区分**：不传 = 保持原样、传 null = 清空、
// 传值 = 设为该值。JSON 的 null 与「键不存在」在 serde 里都是 None（除非用
// Option<Option<T>>），所以必须自己按 `Value` 判，下面每个用例都对着这一点。
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: Value) -> Result<(WritableFields, bool, bool), ApiError> {
        parse_fields(Some(&v))
    }

    #[test]
    fn empty_fields_keeps_everything_and_defaults_to_dry_run() {
        let (f, dry, backup) = parse(json!({ "fields": {} })).expect("应解析成功");
        assert!(f.title.is_none(), "没点名 = 保持原样");
        assert!(f.artists.is_none());
        assert!(f.track.is_none());
        assert!(f.unset.is_empty());
        assert!(dry, "dry_run 默认必须是 true —— 忘了传也不能误写文件");
        assert!(!backup);
    }

    #[test]
    fn null_means_clear_and_is_distinct_from_absent() {
        let (f, _, _) = parse(json!({ "fields": { "title": null, "comment": null } })).expect("应解析成功");
        // null → 进 unset（清空），而不是「设为空串」
        assert!(f.title.is_none());
        assert!(f.unset.contains(&"title".to_string()));
        assert!(f.unset.contains(&"comment".to_string()));
        // 没点名的仍在 unset 之外
        assert!(!f.unset.contains(&"artists".to_string()));
    }

    #[test]
    fn values_are_set_and_aliases_are_accepted() {
        let (f, _, _) = parse(json!({
            "fields": {
                "title": "新标题",
                "artist": ["A", "B"],       // 单数别名 → artists
                "genre": "摇滚",             // 单值也能当列表
                "track": 3,
                "track_total": 12,
                "year": 2015,               // 数字也收
            }
        }))
        .expect("应解析成功");
        assert_eq!(f.title.as_deref(), Some("新标题"));
        assert_eq!(f.artists, Some(vec!["A".to_string(), "B".to_string()]));
        assert_eq!(f.genres, Some(vec!["摇滚".to_string()]));
        assert_eq!(f.track, Some(3));
        assert_eq!(f.track_total, Some(12));
        assert_eq!(f.year.as_deref(), Some("2015"));
    }

    #[test]
    fn album_is_singular_on_the_wire() {
        // 界面只有一个「专辑」输入框，内部是 albums: Vec
        let (f, _, _) = parse(json!({ "fields": { "album": "新专辑" } })).expect("应解析成功");
        assert_eq!(f.albums, Some(vec!["新专辑".to_string()]));
    }

    #[test]
    fn explicit_unset_array_works() {
        let (f, _, _) = parse(json!({ "fields": { "unset": ["comment", "genre"] } })).expect("应解析成功");
        assert!(f.unset.contains(&"comment".to_string()));
        // genre 是别名，映射到 CLI 的 "genre"
        assert!(f.unset.contains(&"genre".to_string()));
    }

    #[test]
    fn unknown_field_is_rejected_not_ignored() {
        // 拼错字段名必须报错 —— 静默忽略会变成「保存成功了但没生效」
        assert!(parse(json!({ "fields": { "titel": "x" } })).is_err());
    }

    #[test]
    fn wrong_type_is_rejected() {
        assert!(parse(json!({ "fields": { "track": "三" } })).is_err());
        assert!(parse(json!({ "fields": { "title": true } })).is_err());
    }

    #[test]
    fn dry_run_and_backup_are_read_from_the_root_not_fields() {
        let (_, dry, backup) = parse(json!({ "fields": {}, "dry_run": false, "backup": true }))
            .expect("应解析成功");
        assert!(!dry, "显式 false 才允许写盘");
        assert!(backup);
        // 放进 fields 里是无效的（那是字段区），会被当成未知字段
        assert!(parse(json!({ "fields": { "dry_run": false } })).is_err());
    }

    #[test]
    fn missing_fields_object_is_rejected() {
        assert!(parse(json!({})).is_err());
        assert!(parse(json!({ "dry_run": true })).is_err());
        assert!(parse_fields(None).is_err());
    }

    #[test]
    fn non_bool_dry_run_is_rejected() {
        assert!(parse(json!({ "fields": {}, "dry_run": "yes" })).is_err());
    }
}

#[cfg(test)]
mod cover_tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;

    /// 最小的「JPEG」：嗅探只看前 3 个魔数字节。
    fn jpeg_b64() -> String {
        let mut bytes = vec![0xFFu8, 0xD8, 0xFF];
        bytes.extend_from_slice(b"pretend-image-payload");
        B64.encode(bytes)
    }

    fn parse(json: Value) -> Result<(WritableFields, bool, bool), ApiError> {
        parse_fields(Some(&json))
    }

    #[test]
    fn cover_null_means_remove() {
        let (f, _, _) = parse(json!({ "fields": { "cover": null } })).expect("应解析成功");
        assert!(f.unset_cover, "cover: null 应表示删除封面");
        assert!(f.replace_cover.is_none());
    }

    #[test]
    fn cover_object_means_replace_and_mime_comes_from_magic_bytes() {
        let (f, _, _) = parse(json!({ "fields": { "cover": { "data": jpeg_b64() } } }))
            .expect("应解析成功");
        let pic = f.replace_cover.expect("应设置了新封面");
        // 声明的 mime 不被采信，按魔数判定
        assert_eq!(pic.mime_type, "image/jpeg");
        assert_eq!(pic.pic_type, 3, "front cover");
        assert!(pic.data.starts_with(&[0xFF, 0xD8, 0xFF]));
    }

    #[test]
    fn cover_accepts_data_url_form() {
        // 前端 FileReader.readAsDataURL 给的就是这个形状，后端直接收，省得前端切
        let url = format!("data:image/jpeg;base64,{}", jpeg_b64());
        let (f, _, _) = parse(json!({ "fields": { "cover": { "data": url } } })).expect("应解析成功");
        assert!(f.replace_cover.is_some());
    }

    #[test]
    fn cover_rejects_bad_base64() {
        assert!(parse(json!({ "fields": { "cover": { "data": "这显然不是 base64!!" } } })).is_err());
    }

    #[test]
    fn cover_rejects_non_image_payload() {
        let not_an_image = B64.encode(b"just some text, no image magic at all");
        assert!(parse(json!({ "fields": { "cover": { "data": not_an_image } } })).is_err());
    }

    #[test]
    fn cover_rejects_empty_payload() {
        assert!(parse(json!({ "fields": { "cover": { "data": "" } } })).is_err());
    }

    #[test]
    fn cover_must_be_object_or_null() {
        assert!(parse(json!({ "fields": { "cover": "image.jpg" } })).is_err());
        assert!(parse(json!({ "fields": { "cover": 42 } })).is_err());
        // 缺 data 也不行
        assert!(parse(json!({ "fields": { "cover": { "mime": "image/jpeg" } } })).is_err());
    }

    #[test]
    fn cover_and_other_fields_can_go_together() {
        let (f, _, _) = parse(json!({
            "fields": { "title": "新标题", "cover": { "data": jpeg_b64() } }
        }))
        .expect("应解析成功");
        assert_eq!(f.title.as_deref(), Some("新标题"));
        assert!(f.replace_cover.is_some());
    }
}
