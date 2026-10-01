//! S16 · 曲库 API：library / songs / albums / artists / search。
//!
//! 路径（取自画布 API 清单）：
//!
//! * GET /api/library?page=&page_size=&sort=  曲库分页列表
//! * GET /api/songs/:id                       单曲详情（不存在 404）
//! * GET /api/albums/:id                      专辑详情（含曲目列表；不存在 404）
//! * GET /api/artists/:name                   某歌手名下的歌曲 / 专辑
//! * GET /api/search?q=&page=&page_size=      搜索
//!
//! 响应体一律手写 serde_json::Value（项目规范：不引 serde derive）。
//!
//! ## 鉴权（画布：所有 API 先过 JWT）
//!
//! 这 5 条路由挂在 routes::build_router 的**受保护子 Router** 上（require_auth
//! 中间件）；此外每个 handler 都显式提取 AuthUser —— 双重保险：即使将来有人
//! 把路由顺手挪进公开 Router，handler 仍会 401，而不是悄悄匿名可用。
//!
//! ## 分页规范（画布「⚠ 补」第一条）
//!
//! * page 从 1 开始，默认 1；page_size 默认 50，**硬上限 200**。
//!   没有上限时 ?page_size=999999999 就是一次内存放大攻击。
//! * 非法输入（0 / 负数 / 非数字 / 超出上限 / 超出 i64 范围）一律 **400 +
//!   统一错误形状**，**不做静默夹取**。取舍：静默夹取（把 999999999 悄悄改成
//!   200）会让前端以为自己拿到了想要的页大小，行为不可预期；宁可明确报错。
//! * **越界页**（例如总共 3 页却请求 page=99）是 200 + 空数组 —— 不是 404，
//!   也不是错误：页号本身合法，只是没有数据。page 没有上限，超大值用
//!   saturating_mul 算 offset，SQLite 直接返回空页。
//! * 响应带分页元信息：{ items, page, page_size, total, total_pages }。
//!   total 由 repo 用与取行完全相同的 WHERE 现算（见 songs::list_page）。
//!
//! ## 排序字段白名单（画布「⚠ 补」第二条）
//!
//! * 支持的字段：added_at（默认）、title、artists、album_artist、year、
//!   duration_ms、bitrate_bps；白名单与 ORDER BY 片段都在
//!   songs::SongSort 里（**不把用户字符串拼进 SQL**）。
//! * 方向选择：**只支持 sort=-字段 的减号前缀**（减号 = 降序），不另外接受
//!   order=asc|desc。理由：一个参数就能表达「字段 + 方向」，前端不必维护两个
//!   参数的组合校验；order 参数被忽略（本接口不认识它）。
//! * 默认排序是 added_at **降序**（最新入库在前）。
//! * 白名单外 / 注入串（sort=id;DROP TABLE songs--）一律 400，且表还在
//!   （有专门的测试直接证明）。
//!
//! ## 不泄漏内部字段
//!
//! * 绝不对外暴露 file_path（服务器绝对路径 = 部署结构；播放走
//!   /api/stream/:id，前端有 id 就够），也不给 scrape_error（内部失败原因）、
//!   audio_hash / search_text / file_mtime / deleted_at 这些内部列；
//! * lyrics 只在**详情**接口返回：列表一页几十条，歌词动辄几十 KB，
//!   放进列表就是成倍放大的响应体；
//! * 专辑详情不内联封面字节（同样是体积问题），只给 has_cover 标记，
//!   封面由后续专门的封面接口按 id 取。
//!
//! ## 软删
//!
//! 所有查询都走 repo 默认的 include_deleted = false：磁盘上消失（mark_deleted）
//! 的歌不出现在任何用户可见接口里，按 id 取详情也是 404。
//!
//! ## 阻塞调用
//!
//! rusqlite 全是同步阻塞调用，统一用 run_db 包进 tokio::task::spawn_blocking
//! （S14 铁律），绝不在 async 上下文里直接碰连接池。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::Json;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::models::{Album, Song};
use crate::db::pool::DbPool;
use crate::db::repos::albums::{self, AlbumSummary};
use crate::db::repos::songs::{self, SongFilter, SongSort};
use crate::server::auth::AuthUser;
use crate::server::error::{ApiError, ApiResult};
use crate::server::state::AppState;

/// 默认每页条数。
const DEFAULT_PAGE_SIZE: i64 = 50;

/// 每页条数硬上限。超过一律 400，绝不静默夹取。
const MAX_PAGE_SIZE: i64 = 200;

/// 默认排序字段（最新入库在前）。
const DEFAULT_SORT: SongSort = SongSort::AddedAt;

/// 默认排序方向：降序。
const DEFAULT_DESCENDING: bool = true;

// ─────────────────────────────────────────────────────────────────────────────
// 查询串提取
// ─────────────────────────────────────────────────────────────────────────────

/// 查询串参数（键 -> 值）。
///
/// 直接用 Query<HashMap<String, String>> 时，查询串本身畸形（例如非法的
/// percent-encoding）会走 axum 默认的「纯文本 400」，与 S14 的统一错误形状
/// 不一致。这里包一层，把任何解析失败折叠成 ApiError::bad_request。
pub struct QueryParams(HashMap<String, String>);

impl<S: Send + Sync> FromRequestParts<S> for QueryParams {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(map) = Query::<HashMap<String, String>>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| {
                crate::serverlog::debug("http", format!("查询串解析失败：{rejection}"));
                ApiError::bad_request("查询参数格式不正确")
            })?;
        Ok(QueryParams(map))
    }
}

impl QueryParams {
    /// 取一个查询参数；键不存在返回 None。
    ///
    /// S19 的 /api/stream/{id} 用它读 `format`，复用这里「查询串畸形 → 统一 400 形状」
    /// 的既有语义，而不是在音频路由里另写一套解析。
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 参数校验
// ─────────────────────────────────────────────────────────────────────────────

/// 解析一个「从 1 开始的正整数」分页参数。
///
/// 缺省或空串取默认值；其余必须能解析成 i64、必须 >= 1；给了上限就必须不超。
/// 一律返回 400 + 统一错误形状，不做任何静默夹取。
fn parse_page_param(
    raw: Option<&String>,
    default: i64,
    max: Option<i64>,
    field: &'static str,
) -> Result<i64, ApiError> {
    let Some(text) = raw else {
        return Ok(default);
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(default);
    }
    let value: i64 = trimmed
        .parse()
        .map_err(|_| ApiError::bad_request(format!("分页参数 {field} 必须是整数")))?;
    if value < 1 {
        return Err(ApiError::bad_request(format!(
            "分页参数 {field} 必须大于等于 1"
        )));
    }
    if let Some(limit) = max {
        if value > limit {
            return Err(ApiError::bad_request(format!(
                "分页参数 {field} 不能超过 {limit}"
            )));
        }
    }
    Ok(value)
}

/// 解析 sort 参数里的字段名与方向。
///
/// 语法：sort=title 升序；sort=-added_at 降序（减号前缀）。字段名字符串只交给
/// songs::SongSort::parse 判白名单，不在这里做任何 SQL 相关处理。
///
/// 错误信息**不回显**调用方传来的字段值：注入串不必再原样反射回去。
fn parse_sort(raw: Option<&String>) -> Result<(SongSort, bool), ApiError> {
    let Some(text) = raw else {
        return Ok((DEFAULT_SORT, DEFAULT_DESCENDING));
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok((DEFAULT_SORT, DEFAULT_DESCENDING));
    }
    let (field, descending) = match trimmed.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (trimmed, false),
    };
    let sort = SongSort::parse(field).ok_or_else(|| {
        ApiError::bad_request(format!(
            "排序字段不在白名单里，允许的字段：{}",
            SongSort::allowed_fields().join("、")
        ))
    })?;
    Ok((sort, descending))
}

/// 路径参数里的 id：必须是十进制整数，否则 400（统一形状）。
///
/// S18 的 /api/stream/{id} 复用这里，保证两条按 id 取资源的路径口径一致。
pub(crate) fn parse_id(raw: &str) -> Result<i64, ApiError> {
    raw.trim()
        .parse::<i64>()
        .map_err(|_| ApiError::bad_request("路径参数必须是整数"))
}

/// 总页数；total = 0 时是 0（不是 1）。
fn total_pages(total: i64, page_size: i64) -> i64 {
    if total <= 0 || page_size <= 0 {
        return 0;
    }
    // 先减再除，避免 total + page_size - 1 在极端值上溢出。
    1 + (total - 1) / page_size
}

// ─────────────────────────────────────────────────────────────────────────────
// 同步查询的包装
// ─────────────────────────────────────────────────────────────────────────────

/// 在 spawn_blocking 里借一条连接执行同步查询（S14 铁律）。
///
/// 闭包只接收 &Connection，不自己借池；借连接失败与任务 panic 都会收敛成
/// ApiError（分别走 From<DbPoolError> 与 internal）。
pub(crate) async fn run_db<T, F>(db: Arc<DbPool>, work: F) -> ApiResult<T>
where
    F: FnOnce(&Connection) -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let conn = db.acquire()?;
        work(&conn)
    })
    .await
    .map_err(|join| ApiError::internal(format!("曲库查询任务异常退出：{join}")))?
}

// ─────────────────────────────────────────────────────────────────────────────
// 对外 JSON
// ─────────────────────────────────────────────────────────────────────────────

/// 歌曲的对外 JSON。
///
/// **刻意不含** file_path（服务器绝对路径 = 部署结构）、scrape_error（内部失败
/// 原因）、audio_hash / search_text / file_mtime / deleted_at（内部列）。
/// 前端播放只需要 id（走 /api/stream/:id）。
///
/// with_lyrics 只有单曲详情传 true；列表接口一页几十条，歌词太大不给。
///
/// S22 的歌单详情也用它（传 false）—— 曲目形状必须与曲库接口**同一个函数**产出，
/// 否则两边各写一份 JSON 迟早漂移，还可能顺手把内部列漏出去。
pub(crate) fn song_json(song: &Song, with_lyrics: bool) -> Value {
    let mut value = json!({
        "id": song.id,
        "album_id": song.album_id,
        "title": song.title,
        "artists": song.artists,
        "album_artist": song.album_artist,
        "year": song.year,
        "genres": song.genres,
        "track": song.track,
        "disc": song.disc,
        "duration_ms": song.duration_ms,
        "bitrate_bps": song.bitrate_bps,
        "format": song.format,
        "file_size": song.file_size,
        "scrape_status": song.scrape_status.as_str(),
        "added_at": song.added_at,
        "updated_at": song.updated_at,
    });
    if with_lyrics {
        if let Some(object) = value.as_object_mut() {
            let _ = object.insert("lyrics".to_string(), json!(song.lyrics));
        }
    }
    value
}

/// 专辑的对外 JSON（详情用）。
///
/// 不内联封面字节：封面动辄几十 KB，塞进 JSON 会拖垮响应；只给 has_cover 标记，
/// 封面由后续专门的封面接口按 id 取。
fn album_json(album: &Album) -> Value {
    json!({
        "id": album.id,
        "name": album.name,
        "album_artist": album.album_artist,
        "year": album.year,
        "has_cover": album.cover_data.is_some(),
        "updated_at": album.updated_at,
    })
}

/// 专辑摘要的对外 JSON（歌手页用，带现算的曲目数）。
fn album_summary_json(summary: &AlbumSummary) -> Value {
    json!({
        "id": summary.id,
        "name": summary.name,
        "album_artist": summary.album_artist,
        "year": summary.year,
        "song_count": summary.song_count,
        "updated_at": summary.updated_at,
    })
}

/// 组装分页响应：{ items, page, page_size, total, total_pages }。
fn paginated_json(items: Vec<Value>, page: i64, page_size: i64, total: i64) -> Value {
    json!({
        "items": items,
        "page": page,
        "page_size": page_size,
        "total": total,
        "total_pages": total_pages(total, page_size),
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/library?page=&page_size=&sort= —— 曲库分页列表。
///
/// 越界页返回 200 + 空数组（见模块文档）。
pub async fn library(
    _auth: AuthUser,
    State(state): State<AppState>,
    params: QueryParams,
) -> ApiResult<Json<Value>> {
    let page = parse_page_param(params.0.get("page"), 1, None, "page")?;
    let page_size = parse_page_param(
        params.0.get("page_size"),
        DEFAULT_PAGE_SIZE,
        Some(MAX_PAGE_SIZE),
        "page_size",
    )?;
    let (sort, descending) = parse_sort(params.0.get("sort"))?;
    // page >= 1 且 page_size >= 1；saturating_mul 让超大 page 饱和成 i64::MAX，
    // SQLite 对这样的 OFFSET 直接返回空页，不会 panic 也不会溢出。
    let offset = (page - 1).saturating_mul(page_size);

    let result = run_db(Arc::clone(&state.db), move |conn| {
        Ok(songs::list_page(
            conn,
            SongFilter::default(),
            sort,
            descending,
            page_size,
            offset,
            false,
        )?)
    })
    .await?;

    // 走 songs_json：顺手补上专辑名（列表要显示它，而 songs 表只有 album_id）
    let (items, total) = run_db(Arc::clone(&state.db), move |conn| {
        Ok((songs_json(conn, &result.songs, false), result.total))
    })
    .await?;
    Ok(Json(paginated_json(items, page, page_size, total)))
}

/// 一次把这批曲目用到的专辑名查出来（**一条 SQL，不是 N+1**）。
///
/// 为什么需要：`songs` 表只有 `album_id`，而列表要显示专辑名。
/// 不把名字塞进 `Song` 结构体（那会波及 scanner 等所有构造点），
/// 而是在出口处补一次批量查询。
fn album_names_for(conn: &Connection, songs: &[Song]) -> HashMap<i64, String> {
    let mut ids: Vec<i64> = songs.iter().filter_map(|s| s.album_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut map = HashMap::new();
    for id in ids {
        if let Ok(Some(album)) = albums::get(conn, id) {
            map.insert(id, album.name);
        }
    }
    map
}

/// 曲目数组 → 对外 JSON，顺带补上 `album`（专辑名，可能没有）。
///
/// **所有列表型接口都该走这里**，否则「有没有专辑名」会在各接口之间漂移。
pub(crate) fn songs_json(conn: &Connection, songs: &[Song], with_lyrics: bool) -> Vec<Value> {
    let names = album_names_for(conn, songs);
    songs
        .iter()
        .map(|song| {
            let mut value = song_json(song, with_lyrics);
            let album = song
                .album_id
                .and_then(|id| names.get(&id))
                .map(|name| Value::String(name.clone()))
                .unwrap_or(Value::Null);
            if let Some(object) = value.as_object_mut() {
                let _ = object.insert("album".to_string(), album);
            }
            value
        })
        .collect()
}

/// GET /api/albums —— 专辑列表（分页）。
pub async fn albums_list(
    _auth: AuthUser,
    State(state): State<AppState>,
    params: QueryParams,
) -> ApiResult<Json<Value>> {
    let page = parse_page_param(params.0.get("page"), 1, None, "page")?;
    let page_size = parse_page_param(
        params.0.get("page_size"),
        DEFAULT_PAGE_SIZE,
        Some(MAX_PAGE_SIZE),
        "page_size",
    )?;
    let offset = (page - 1).saturating_mul(page_size);

    let (items, total) = run_db(Arc::clone(&state.db), move |conn| {
        let rows = albums::list_with_song_count(conn, page_size, offset)?;
        let total = albums::count(conn)?;
        Ok((
            rows.into_iter().map(|s| album_summary_json(&s)).collect::<Vec<_>>(),
            total,
        ))
    })
    .await?;
    Ok(Json(paginated_json(items, page, page_size, total)))
}

/// GET /api/artists —— 歌手列表（分页，按曲目数降序）。
///
/// 「歌手」在后端就是 `songs.artists` 整串，与详情接口的精确匹配口径一致
/// （原因见 [`crate::db::repos::songs::ArtistSummary`] 的注释）。
pub async fn artists_list(
    _auth: AuthUser,
    State(state): State<AppState>,
    params: QueryParams,
) -> ApiResult<Json<Value>> {
    let page = parse_page_param(params.0.get("page"), 1, None, "page")?;
    let page_size = parse_page_param(
        params.0.get("page_size"),
        DEFAULT_PAGE_SIZE,
        Some(MAX_PAGE_SIZE),
        "page_size",
    )?;
    let offset = (page - 1).saturating_mul(page_size);

    let (items, total) = run_db(Arc::clone(&state.db), move |conn| {
        let rows = songs::list_artists(conn, page_size, offset)?;
        let total = songs::count_artists(conn)?;
        let items = rows
            .into_iter()
            .map(|a| {
                json!({
                    "name": a.name,
                    "song_count": a.song_count,
                    "album_count": a.album_count,
                })
            })
            .collect::<Vec<_>>();
        Ok((items, total))
    })
    .await?;
    Ok(Json(paginated_json(items, page, page_size, total)))
}

/// GET /api/songs/:id —— 单曲详情（**唯一**带 lyrics 的接口）。
pub async fn song(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    let found = run_db(Arc::clone(&state.db), move |conn| {
        Ok(songs::get(conn, id, false)?)
    })
    .await?;

    match found {
        // 详情同样补专辑名、同样带 lyrics（这是唯一带歌词的接口）
        Some(song) => {
            let (value, _) = run_db(Arc::clone(&state.db), move |conn| {
                let mut v = songs_json(conn, std::slice::from_ref(&song), true);
                Ok((v.pop().unwrap_or(Value::Null), ()))
            })
            .await?;
            Ok(Json(json!({ "song": value })))
        }
        None => Err(ApiError::not_found("请求的歌曲不存在")),
    }
}

/// GET /api/albums/:id —— 专辑详情 + 曲目列表。
///
/// 曲目按 (碟号, 音轨号, id) 排：专辑页要的是唱片刻号顺序，不是入库顺序。
/// /api/library 的排序白名单里没有 track（那是给列表接口的字段集），所以这里
/// 取回后在 Rust 侧排序，不往 repo 里塞第二套排序白名单。
pub async fn album(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    let loaded = run_db(Arc::clone(&state.db), move |conn| {
        let Some(album) = albums::get(conn, id)? else {
            return Ok(None);
        };
        let page = songs::list_page(
            conn,
            SongFilter {
                album_id: Some(id),
                ..Default::default()
            },
            SongSort::AddedAt,
            false,
            i64::MAX,
            0,
            false,
        )?;
        Ok(Some((album, page.songs)))
    })
    .await?;

    let Some((album, mut tracks)) = loaded else {
        return Err(ApiError::not_found("请求的专辑不存在"));
    };
    tracks.sort_by_key(|track| {
        (
            track.disc.unwrap_or(i64::MAX),
            track.track.unwrap_or(i64::MAX),
            track.id,
        )
    });
    // 出口统一走 songs_json：专辑名一并带上，口径与其它列表接口一致
    // （否则这里的「专辑」列会显示成 —，看着像坏了）
    let songs = run_db(Arc::clone(&state.db), move |conn| {
        Ok(songs_json(conn, &tracks, false))
    })
    .await?;
    Ok(Json(json!({
        "album": album_json(&album),
        "songs": songs,
    })))
}

/// GET /api/artists/:name —— 某歌手名下的歌曲 / 专辑。
///
/// 画布明确不建 artists 表：歌手名存在 songs.artists 文本列，按名字查是现算。
///
/// 匹配口径：
///   * 歌曲 —— songs.artists 的**子串**匹配（大小写：走 SQLite LIKE，ASCII
///     不敏感；% 与 _ 按字面量转义）。取舍见 songs::list_page 的文档：多个歌手
///     在库里是文本拼接（扫描时用 " / " 连接），但刮削插件可写任意整串，所以
///     不做分隔符精确切分；代价是包含关系会误命中。
///   * 专辑 —— albums.album_artist 与路径参数**精确相等**（album_artist 是
///     单值列，没必要也不应该用子串）。
///
/// 该接口不分页（画布没给分页参数）：返回该歌手名下的全部歌曲与专辑。
/// 专辑列表用 albums::list_with_song_count 取回后在 Rust 侧按 album_artist
/// 过滤 —— 专辑数量远小于歌曲，且不触碰 albums.rs（本步骤不允许改它）。
pub async fn artists(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_name): Path<String>,
) -> ApiResult<Json<Value>> {
    let name = raw_name.trim().to_string();
    // 空名字（例如 /api/artists/%20）不是「所有歌手」，直接空结果，
    // 免得空白过滤值退化成 LIKE %% 把整库导出。
    if name.is_empty() {
        return Ok(Json(json!({ "artist": "", "songs": [], "albums": [] })));
    }

    let lookup = name.clone();
    let (songs, album_rows) = run_db(Arc::clone(&state.db), move |conn| {
        let page = songs::list_page(
            conn,
            SongFilter {
                artists: Some(&lookup),
                ..Default::default()
            },
            SongSort::AddedAt,
            false,
            i64::MAX,
            0,
            false,
        )?;
        let album_rows = albums::list_with_song_count(conn, i64::MAX, 0)?;
        // 出口统一走 songs_json —— 顺手补专辑名，与列表接口口径一致
        let songs = songs_json(conn, &page.songs, false);
        Ok((songs, album_rows))
    })
    .await?;

    let albums: Vec<Value> = album_rows
        .into_iter()
        .filter(|summary| summary.album_artist == name)
        .map(|summary| album_summary_json(&summary))
        .collect();
    Ok(Json(json!({
        "artist": name,
        "songs": songs,
        "albums": albums,
    })))
}

/// GET /api/search?q=&page=&page_size= —— 搜索。
///
/// 空 / 缺失 / 全空白的 q 与 repo 的 search 口径一致：**返回空结果（200）**，
/// 而不是「返回全部」（要列全部请用 /api/library）。
pub async fn search(
    _auth: AuthUser,
    State(state): State<AppState>,
    params: QueryParams,
) -> ApiResult<Json<Value>> {
    let page = parse_page_param(params.0.get("page"), 1, None, "page")?;
    let page_size = parse_page_param(
        params.0.get("page_size"),
        DEFAULT_PAGE_SIZE,
        Some(MAX_PAGE_SIZE),
        "page_size",
    )?;
    let offset = (page - 1).saturating_mul(page_size);
    // 关键词缺失 / 空白都折叠成空串；list_page 对空串返回空结果。
    let keyword = params
        .0
        .get("q")
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let echo = keyword.clone();

    let result = run_db(Arc::clone(&state.db), move |conn| {
        Ok(songs::list_page(
            conn,
            SongFilter {
                keyword: Some(&keyword),
                ..Default::default()
            },
            DEFAULT_SORT,
            DEFAULT_DESCENDING,
            page_size,
            offset,
            false,
        )?)
    })
    .await?;

    let items = run_db(Arc::clone(&state.db), move |conn| {
        Ok(songs_json(conn, &result.songs, false))
    })
    .await?;
    let mut body = paginated_json(items, page, page_size, result.total);
    if let Some(object) = body.as_object_mut() {
        let _ = object.insert("q".to_string(), Value::String(echo));
    }
    Ok(Json(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use rusqlite::params;
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::db::migrations;
    use crate::db::models::{Role, User};
    use crate::db::pool::TempDb;
    use crate::server::auth::sign_token_with_ttl;
    use crate::server::routes::build_router;

    /// 测试用 JWT 密钥。
    const SECRET: &str = "s16-secret";

    /// 建一个跑完迁移的临时文件库状态（画布指定：DbPool::open_temp）。
    /// 必须持有返回的 TempDb，它一析构就会删库文件。
    fn test_state(tag: &str) -> (AppState, TempDb) {
        let (pool, temp) = DbPool::open_temp(tag).expect("建临时文件库池");
        {
            let mut guard = pool.acquire().expect("借连接");
            migrations::apply(&mut guard).expect("应用迁移");
        }
        let mut cfg = Config::defaults();
        cfg.server.jwt_secret = SECRET.to_string();
        cfg.storage.library_roots = vec!["/tmp/server-s16-library-music".to_string()];
        (AppState::new(Arc::new(pool), Arc::new(cfg)), temp)
    }

    /// 预置一个用户并签一个可用令牌（不走注册接口，省掉 argon2 的开销）。
    fn issue_token(state: &AppState) -> String {
        let id = {
            let conn = state.db.acquire().expect("借连接");
            conn.execute(
                "INSERT INTO users (username, password_hash, role, created_at)
                 VALUES ('tester', 'hash', 'admin', ?1)",
                params![crate::db::now_unix_ms()],
            )
            .expect("预置用户");
            conn.last_insert_rowid()
        };
        let user = User {
            id,
            username: "tester".to_string(),
            password_hash: "hash".to_string(),
            role: Role::Admin,
            created_at: 0,
            last_login: None,
        };
        sign_token_with_ttl(SECRET, &user, 3600).expect("签发令牌")
    }

    /// 测试用歌曲字段；只需要什么就设什么，added_at 显式给（插入同毫秒会
    /// 让按 added_at 排序的结果不确定）。
    #[derive(Default, Clone)]
    struct SeedSong {
        path: String,
        title: String,
        artists: Option<String>,
        album_artist: Option<String>,
        album_id: Option<i64>,
        track: Option<i64>,
        disc: Option<i64>,
        year: Option<i64>,
        duration_ms: Option<i64>,
        bitrate_bps: Option<i64>,
        lyrics: Option<String>,
        added_at: i64,
    }

    /// 直接插库（不走 songs::insert：后者会盖掉 added_at，排序测试需要可控时间）。
    fn seed(conn: &Connection, song: SeedSong) -> i64 {
        let search_text = match &song.artists {
            Some(artists) => format!("{} {}", song.title, artists),
            None => song.title.clone(),
        };
        conn.execute(
            "INSERT INTO songs (
                 file_path, album_id, title, artists, album_artist, track, disc, year,
                 duration_ms, bitrate_bps, search_text, lyrics, scrape_status, added_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'pending', ?13, ?13)",
            params![
                song.path,
                song.album_id,
                song.title,
                song.artists,
                song.album_artist,
                song.track,
                song.disc,
                song.year,
                song.duration_ms,
                song.bitrate_bps,
                search_text,
                song.lyrics,
                song.added_at,
            ],
        )
        .expect("预置歌曲");
        conn.last_insert_rowid()
    }

    /// 预置一张专辑，返回 id。
    fn seed_album(conn: &Connection, name: &str, album_artist: &str) -> i64 {
        conn.execute(
            "INSERT INTO albums (name, album_artist, updated_at) VALUES (?1, ?2, ?3)",
            params![name, album_artist, crate::db::now_unix_ms()],
        )
        .expect("预置专辑");
        conn.last_insert_rowid()
    }

    /// 把非 ASCII 字符按 UTF-8 百分号编码，方便拼 URI（测试只处理中文值）。
    fn encode(value: &str) -> String {
        let mut out = String::new();
        for byte in value.as_bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(*byte as char)
                }
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out
    }

    fn get(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::empty()).expect("构造请求")
    }

    /// tower oneshot 直调 Router（不绑端口），返回状态码 + JSON body。
    async fn call(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
        let response = build_router(state.clone())
            .oneshot(request)
            .await
            .expect("oneshot");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("读响应体");
        let body = serde_json::from_slice(&bytes).expect("响应体必须是合法 JSON");
        (status, body)
    }

    /// 带令牌 GET，返回 (状态码, body)。
    async fn authed(state: &AppState, token: &str, uri: &str) -> (StatusCode, Value) {
        call(state, get(uri, Some(token))).await
    }

    /// 从 items 里取出 id 列表（顺序敏感）。
    fn ids(body: &Value) -> Vec<i64> {
        body["items"]
            .as_array()
            .expect("items 是数组")
            .iter()
            .map(|item| item["id"].as_i64().expect("id 是整数"))
            .collect()
    }

    /// 断言一个歌曲对象没有泄漏任何内部字段。
    fn assert_no_internal_fields(item: &Value, allow_lyrics: bool) {
        for field in [
            "file_path",
            "scrape_error",
            "audio_hash",
            "search_text",
            "file_mtime",
            "deleted_at",
        ] {
            assert!(
                item.get(field).is_none(),
                "响应泄漏了内部字段 {field}：{item}"
            );
        }
        if !allow_lyrics {
            assert!(item.get("lyrics").is_none(), "列表项不该带 lyrics：{item}");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 1. 分页边界
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：分页边界 —— 第一页 / 最后一页不足一页 / 越界页空数组 + 200 /
    /// total 与 total_pages 计算正确。
    #[tokio::test]
    async fn library_pages_first_last_and_out_of_range() {
        let (state, _temp) = test_state("s16-page");
        let token = issue_token(&state);
        {
            let conn = state.db.acquire().expect("借连接");
            for index in 0..5 {
                seed(
                    &conn,
                    SeedSong {
                        path: format!("/music/{index}.mp3"),
                        title: format!("歌 {index}"),
                        added_at: 100 + index,
                        ..Default::default()
                    },
                );
            }
        }

        // 默认排序是 added_at 降序：第一页取到最新两首（id 5、4）
        let (status, body) = authed(&state, &token, "/api/library?page=1&page_size=2").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), vec![5, 4]);
        assert_eq!(body["page"], 1);
        assert_eq!(body["page_size"], 2);
        assert_eq!(body["total"], 5);
        assert_eq!(body["total_pages"], 3);

        // 中间页
        let (status, body) = authed(&state, &token, "/api/library?page=2&page_size=2").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), vec![3, 2]);

        // 最后一页只有 1 条（不足 page_size）
        let (status, body) = authed(&state, &token, "/api/library?page=3&page_size=2").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), vec![1]);
        assert_eq!(body["total_pages"], 3);

        // 越界页：200 + 空数组（不是 404、不是错误）
        let (status, body) = authed(&state, &token, "/api/library?page=99&page_size=2").await;
        assert_eq!(status, StatusCode::OK, "越界页必须是 200：{body}");
        assert_eq!(ids(&body), Vec::<i64>::new());
        assert_eq!(body["total"], 5, "越界页也要如实给 total");
        assert_eq!(body["total_pages"], 3);

        // 极端 page：offset 饱和，仍必须 200 + 空数组（不能 panic / 溢出）
        let (status, body) =
            authed(&state, &token, "/api/library?page=9223372036854775807&page_size=200").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), Vec::<i64>::new());

        // 未给参数时用默认 page=1 / page_size=50
        let (status, body) = authed(&state, &token, "/api/library").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["page"], 1);
        assert_eq!(body["page_size"], 50);
        assert_eq!(ids(&body).len(), 5);

        // page_size 正好等于上限：允许
        let (status, _) = authed(&state, &token, "/api/library?page_size=200").await;
        assert_eq!(status, StatusCode::OK, "200 是允许的上限");

        // 空库：total = 0、total_pages = 0、items 空
        let (empty_state, _temp2) = test_state("s16-empty");
        let empty_token = issue_token(&empty_state);
        let (status, body) = authed(&empty_state, &empty_token, "/api/library").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], 0);
        assert_eq!(body["total_pages"], 0, "空库的 total_pages 是 0 而不是 1");
    }

    /// 画布 UT：分页参数非法一律 400 + 统一错误形状（不静默夹取）。
    #[tokio::test]
    async fn invalid_pagination_parameters_are_400() {
        let (state, _temp) = test_state("s16-page-bad");
        let token = issue_token(&state);

        let cases = [
            "/api/library?page=0",
            "/api/library?page=-1",
            "/api/library?page=abc",
            "/api/library?page=1.5",
            "/api/library?page=99999999999999999999",
            "/api/library?page_size=0",
            "/api/library?page_size=-5",
            "/api/library?page_size=abc",
            "/api/library?page_size=201",
            "/api/library?page_size=999999999",
            "/api/search?q=a&page=0",
            "/api/search?q=a&page_size=201",
            "/api/search?q=a&page_size=abc",
        ];
        for uri in cases {
            let (status, body) = authed(&state, &token, uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} 必须 400：{body}");
            assert_eq!(body["error"]["code"], "BAD_REQUEST", "{uri}");
            let message = body["error"]["message"].as_str().unwrap_or_default();
            assert!(!message.is_empty(), "{uri} 的错误必须有中文说明");
            assert!(
                body["error"].get("details").is_some(),
                "{uri} 必须保持统一错误形状"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 2. 排序字段白名单
    // ─────────────────────────────────────────────────────────────────────────

    /// 预置 3 首字段各不相同的歌，返回 ids；用于验证 7 个白名单字段的升降序。
    fn seed_sort_fixture(conn: &Connection) -> (i64, i64, i64) {
        let a = seed(
            conn,
            SeedSong {
                path: "/music/a.mp3".to_string(),
                title: "Alpha".to_string(),
                artists: Some("Beta".to_string()),
                album_artist: Some("Gamma".to_string()),
                year: Some(1999),
                duration_ms: Some(100_000),
                bitrate_bps: Some(128_000),
                added_at: 100,
                ..Default::default()
            },
        );
        let b = seed(
            conn,
            SeedSong {
                path: "/music/b.mp3".to_string(),
                title: "Bravo".to_string(),
                artists: Some("Alpha".to_string()),
                album_artist: Some("Delta".to_string()),
                year: Some(2010),
                duration_ms: Some(300_000),
                bitrate_bps: Some(320_000),
                added_at: 200,
                ..Default::default()
            },
        );
        let c = seed(
            conn,
            SeedSong {
                path: "/music/c.mp3".to_string(),
                title: "Charlie".to_string(),
                artists: Some("Gamma".to_string()),
                album_artist: Some("Beta".to_string()),
                year: Some(2005),
                duration_ms: Some(200_000),
                bitrate_bps: Some(192_000),
                added_at: 300,
                ..Default::default()
            },
        );
        (a, b, c)
    }

    /// 画布 UT：合法字段的升降序都真的生效（断言顺序，而不只是状态码）。
    #[tokio::test]
    async fn sort_whitelist_orders_both_directions() {
        let (state, _temp) = test_state("s16-sort");
        let token = issue_token(&state);
        let (a, b, c) = {
            let conn = state.db.acquire().expect("借连接");
            seed_sort_fixture(&conn)
        };
        assert_eq!((a, b, c), (1, 2, 3), "自增 id 应按插入顺序");

        // 默认 = added_at 降序
        let (status, body) = authed(&state, &token, "/api/library").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), vec![c, b, a], "默认必须是最新在前");

        let cases: [(&str, Vec<i64>); 9] = [
            ("sort=added_at", vec![a, b, c]),
            ("sort=-added_at", vec![c, b, a]),
            ("sort=title", vec![a, b, c]),
            ("sort=-title", vec![c, b, a]),
            ("sort=artists", vec![b, a, c]),
            ("sort=album_artist", vec![c, b, a]),
            ("sort=year", vec![a, c, b]),
            ("sort=duration_ms", vec![a, c, b]),
            ("sort=bitrate_bps", vec![a, c, b]),
        ];
        for (query, expected) in cases {
            let uri = format!("/api/library?{query}");
            let (status, body) = authed(&state, &token, &uri).await;
            assert_eq!(status, StatusCode::OK, "{query} 应成功：{body}");
            assert_eq!(ids(&body), expected, "{query} 的排序结果不对");
        }
    }

    /// 画布 UT + 硬要求：注入串 / 白名单外字段 → 400，且 songs 表仍然健在。
    #[tokio::test]
    async fn sort_injection_is_rejected_and_the_table_survives() {
        let (state, _temp) = test_state("s16-sort-injection");
        let token = issue_token(&state);
        {
            let conn = state.db.acquire().expect("借连接");
            seed(
                &conn,
                SeedSong {
                    path: "/music/a.mp3".to_string(),
                    title: "Alpha".to_string(),
                    added_at: 1,
                    ..Default::default()
                },
            );
        }

        let attacks = [
            "id;DROP TABLE songs--",
            "title;--",
            "1=1",
            "(SELECT 1)",
            "random()",
            "id",
            "file_path",
            "lyrics",
            "search_text",
            "deleted_at",
            "added_at ASC--",
        ];
        for attack in attacks {
            let uri = format!("/api/library?sort={}", encode(attack));
            let (status, body) = authed(&state, &token, &uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{attack} 必须 400：{body}");
            assert_eq!(body["error"]["code"], "BAD_REQUEST", "{attack}");
        }

        // 表还在：直接对 songs 跑一次聚合，注入若生效过这里会报 no such table
        {
            let conn = state.db.acquire().expect("借连接");
            let count: i64 = conn
                .query_row("SELECT COUNT(*) FROM songs", [], |row| row.get(0))
                .expect("songs 表必须还在");
            assert_eq!(count, 1, "注入不该改数据");
        }
        // 合法请求仍然可用
        let (status, body) = authed(&state, &token, "/api/library?sort=-added_at").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), vec![1]);
    }

    /// 白名单常量本身：解析只认那 7 个字段，且与文档一致。
    #[test]
    fn sort_whitelist_matches_the_documented_fields() {
        let allowed = SongSort::allowed_fields();
        assert_eq!(
            allowed,
            vec![
                "added_at",
                "title",
                "artists",
                "album_artist",
                "year",
                "duration_ms",
                "bitrate_bps",
            ]
        );
        for field in &allowed {
            assert!(SongSort::parse(field).is_some(), "{field} 应在白名单里");
        }
        for field in ["id", "file_path", "lyrics", "", "ADDED_AT", "added_at ", "title2"] {
            assert!(SongSort::parse(field).is_none(), "{field} 不该被放行");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 3. 不存在 404
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：单曲 / 专辑不存在 → 404 + 统一错误形状。
    #[tokio::test]
    async fn missing_song_and_album_are_404() {
        let (state, _temp) = test_state("s16-404");
        let token = issue_token(&state);

        for uri in ["/api/songs/999999", "/api/albums/999999"] {
            let (status, body) = authed(&state, &token, uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri} 必须 404：{body}");
            assert_eq!(body["error"]["code"], "NOT_FOUND", "{uri}");
            let message = body["error"]["message"].as_str().unwrap_or_default();
            assert!(
                message.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{uri} 的错误信息必须是中文：{message}"
            );
            assert!(body["error"].get("details").is_some(), "统一形状要带 details");
        }

        // 非整数 id 是 400（不是 404、也不是 axum 的纯文本拒绝）
        let (status, body) = authed(&state, &token, "/api/songs/abc").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "BAD_REQUEST");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 4. 单曲 / 专辑详情
    // ─────────────────────────────────────────────────────────────────────────

    /// 单曲详情：字段齐全；带 lyrics；不含任何内部字段。
    #[tokio::test]
    async fn song_detail_includes_lyrics_and_hides_internal_fields() {
        let (state, _temp) = test_state("s16-song-detail");
        let token = issue_token(&state);
        let song_id = {
            let conn = state.db.acquire().expect("借连接");
            let id = seed(
                &conn,
                SeedSong {
                    path: "/music/secret.mp3".to_string(),
                    title: "夜曲".to_string(),
                    artists: Some("周杰伦".to_string()),
                    year: Some(2005),
                    duration_ms: Some(226_000),
                    lyrics: Some("一群嗜血的蚂蚁".to_string()),
                    added_at: 7,
                    ..Default::default()
                },
            );
            conn.execute(
                "UPDATE songs SET scrape_error = ?2 WHERE id = ?1",
                params![id, "插件全部未命中（内部原因）"],
            )
            .expect("写内部字段");
            id
        };

        let (status, body) =
            authed(&state, &token, &format!("/api/songs/{song_id}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let song = &body["song"];
        assert_eq!(song["id"], song_id);
        assert_eq!(song["title"], "夜曲");
        assert_eq!(song["artists"], "周杰伦");
        assert_eq!(song["lyrics"], "一群嗜血的蚂蚁", "详情必须给歌词");
        assert_eq!(song["scrape_status"], "pending");
        assert_no_internal_fields(song, true);
    }

    /// 专辑详情：AlbumSummary 现算的曲目列表按 (disc, track) 排，且只含本专辑的歌。
    #[tokio::test]
    async fn album_detail_lists_only_its_tracks() {
        let (state, _temp) = test_state("s16-album-detail");
        let token = issue_token(&state);
        let (album_id, other_album_id) = {
            let conn = state.db.acquire().expect("借连接");
            let album = seed_album(&conn, "十一月的萧邦", "周杰伦");
            let other = seed_album(&conn, "第二张", "别人");
            // 故意乱序插入 + 跨碟
            seed(
                &conn,
                SeedSong {
                    path: "/music/2.mp3".to_string(),
                    title: "第二轨".to_string(),
                    album_id: Some(album),
                    disc: Some(1),
                    track: Some(2),
                    added_at: 20,
                    ..Default::default()
                },
            );
            seed(
                &conn,
                SeedSong {
                    path: "/music/1.mp3".to_string(),
                    title: "第一轨".to_string(),
                    album_id: Some(album),
                    disc: Some(1),
                    track: Some(1),
                    added_at: 30,
                    ..Default::default()
                },
            );
            seed(
                &conn,
                SeedSong {
                    path: "/music/other.mp3".to_string(),
                    title: "别人的歌".to_string(),
                    album_id: Some(other),
                    added_at: 40,
                    ..Default::default()
                },
            );
            (album, other)
        };

        let (status, body) = authed(&state, &token, &format!("/api/albums/{album_id}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["album"]["id"], album_id);
        assert_eq!(body["album"]["name"], "十一月的萧邦");
        assert_eq!(body["album"]["album_artist"], "周杰伦");
        assert_eq!(body["album"]["has_cover"], false);
        assert!(
            body["album"].get("cover_data").is_none(),
            "绝不能在 JSON 里内联封面字节"
        );

        let tracks = body["songs"].as_array().expect("songs 是数组");
        assert_eq!(tracks.len(), 2, "只该有本专辑的曲目：{body}");
        assert_eq!(tracks[0]["title"], "第一轨", "专辑曲目要按 (disc, track) 排");
        assert_eq!(tracks[1]["title"], "第二轨");
        for track in tracks {
            assert_eq!(track["album_id"], album_id);
            assert_no_internal_fields(track, false);
        }

        // 另一张专辑的曲目不在里面
        let (status, body) =
            authed(&state, &token, &format!("/api/albums/{other_album_id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["songs"].as_array().expect("数组").len(), 1);
        assert_eq!(body["songs"][0]["title"], "别人的歌");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 5. 搜索
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：搜索命中 / 搜不到空数组 / 空 q 返回空 / 大小写不敏感 / 转义。
    #[tokio::test]
    async fn search_hits_misses_and_blank_keywords() {
        let (state, _temp) = test_state("s16-search");
        let token = issue_token(&state);
        {
            let conn = state.db.acquire().expect("借连接");
            seed(
                &conn,
                SeedSong {
                    path: "/music/1.mp3".to_string(),
                    title: "Hey Jude".to_string(),
                    artists: Some("The Beatles".to_string()),
                    added_at: 1,
                    ..Default::default()
                },
            );
            seed(
                &conn,
                SeedSong {
                    path: "/music/2.mp3".to_string(),
                    title: "Bohemian Rhapsody".to_string(),
                    artists: Some("Queen".to_string()),
                    added_at: 2,
                    ..Default::default()
                },
            );
        }

        let (status, body) = authed(&state, &token, "/api/search?q=Jude").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["items"].as_array().expect("数组").len(), 1);
        assert_eq!(body["items"][0]["title"], "Hey Jude");
        assert_eq!(body["total"], 1);
        assert_eq!(body["total_pages"], 1);
        assert_eq!(body["q"], "Jude");

        // ASCII 大小写不敏感（沿用 repo 的 search 口径）
        let (status, body) = authed(&state, &token, "/api/search?q=beatles").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], 1);
        assert_no_internal_fields(&body["items"][0], false);

        // 搜不到：200 + 空数组（不是错误）
        let uri = format!("/api/search?q={}", encode("根本没有"));
        let (status, body) = authed(&state, &token, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(ids(&body), Vec::<i64>::new());
        assert_eq!(body["total"], 0);

        // 空 q / 缺 q / 全空白 q：返回空（与 repo 一致，不是「返回全部」）
        for uri in ["/api/search?q=", "/api/search", "/api/search?q=%20%20"] {
            let (status, body) = authed(&state, &token, uri).await;
            assert_eq!(status, StatusCode::OK, "{uri} 必须 200：{body}");
            assert_eq!(ids(&body), Vec::<i64>::new(), "{uri} 必须空结果");
            assert_eq!(body["total"], 0, "{uri}");
        }

        // % 与 _ 是字面量，不是通配符：搜 % 不能把整库捞出来
        let (status, body) = authed(&state, &token, "/api/search?q=%25").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), Vec::<i64>::new());

        // 中文关键词（百分号编码）也能命中
        {
            let conn = state.db.acquire().expect("借连接");
            seed(
                &conn,
                SeedSong {
                    path: "/music/3.mp3".to_string(),
                    title: "中文标题".to_string(),
                    artists: Some("某歌手".to_string()),
                    added_at: 3,
                    ..Default::default()
                },
            );
        }
        let uri = format!("/api/search?q={}", encode("某歌手"));
        let (status, body) = authed(&state, &token, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], 1);
        assert_eq!(body["items"][0]["title"], "中文标题");

        // 搜索也支持分页
        let (status, body) = authed(&state, &token, "/api/search?q=e&page=1&page_size=1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["items"].as_array().expect("数组").len(), 1);
        assert_eq!(body["page"], 1);
        assert_eq!(body["page_size"], 1);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 6. 歌手（子串口径）
    // ─────────────────────────────────────────────────────────────────────────

    /// 歌手接口：歌曲按 songs.artists 子串匹配；专辑按 album_artist 精确匹配。
    #[tokio::test]
    async fn artist_uses_documented_substring_and_exact_album_matching() {
        let (state, _temp) = test_state("s16-artist");
        let token = issue_token(&state);
        let (jay_album, other_album, single, duet, similar, other_song) = {
            let conn = state.db.acquire().expect("借连接");
            let jay_album = seed_album(&conn, "十一月的萧邦", "周杰伦");
            let other_album = seed_album(&conn, "第二天堂", "林俊杰");
            let single = seed(
                &conn,
                SeedSong {
                    path: "/music/1.mp3".to_string(),
                    title: "夜曲".to_string(),
                    artists: Some("周杰伦".to_string()),
                    album_id: Some(jay_album),
                    added_at: 1,
                    ..Default::default()
                },
            );
            // 多歌手拼接：扫描入库用 " / " 连接
            let duet = seed(
                &conn,
                SeedSong {
                    path: "/music/2.mp3".to_string(),
                    title: "千里之外".to_string(),
                    artists: Some("周杰伦 / 费玉清".to_string()),
                    album_id: Some(jay_album),
                    added_at: 2,
                    ..Default::default()
                },
            );
            // 包含关系的名字：子串口径下会被命中（已知取舍，测试把它钉死）
            let similar = seed(
                &conn,
                SeedSong {
                    path: "/music/3.mp3".to_string(),
                    title: "模仿者".to_string(),
                    artists: Some("小周杰伦".to_string()),
                    added_at: 3,
                    ..Default::default()
                },
            );
            let other_song = seed(
                &conn,
                SeedSong {
                    path: "/music/4.mp3".to_string(),
                    title: "江南".to_string(),
                    artists: Some("林俊杰".to_string()),
                    album_id: Some(other_album),
                    added_at: 4,
                    ..Default::default()
                },
            );
            (
                jay_album,
                other_album,
                single,
                duet,
                similar,
                other_song,
            )
        };

        let uri = format!("/api/artists/{}", encode("周杰伦"));
        let (status, body) = authed(&state, &token, &uri).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["artist"], "周杰伦");

        let mut song_ids: Vec<i64> = body["songs"]
            .as_array()
            .expect("songs 是数组")
            .iter()
            .map(|item| item["id"].as_i64().expect("id 是整数"))
            .collect();
        song_ids.sort_unstable();
        assert_eq!(
            song_ids,
            vec![single, duet, similar],
            "子串口径：多歌手拼接与包含关系的名字都会命中"
        );
        for item in body["songs"].as_array().expect("数组") {
            assert_no_internal_fields(item, false);
        }

        let album_ids: Vec<i64> = body["albums"]
            .as_array()
            .expect("albums 是数组")
            .iter()
            .map(|item| item["id"].as_i64().expect("id 是整数"))
            .collect();
        assert_eq!(
            album_ids,
            vec![jay_album],
            "专辑按 album_artist 精确匹配，不该带上别人的"
        );
        assert_eq!(body["albums"][0]["song_count"], 2, "曲目数要现算");
        assert!(
            body["albums"][0].get("cover_data").is_none(),
            "专辑摘要不该内联封面"
        );

        // 另一个歌手
        let uri = format!("/api/artists/{}", encode("林俊杰"));
        let (status, body) = authed(&state, &token, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["songs"]
                .as_array()
                .expect("数组")
                .iter()
                .map(|item| item["id"].as_i64().expect("id"))
                .collect::<Vec<_>>(),
            vec![other_song]
        );
        assert_eq!(
            body["albums"]
                .as_array()
                .expect("数组")
                .iter()
                .map(|item| item["id"].as_i64().expect("id"))
                .collect::<Vec<_>>(),
            vec![other_album]
        );

        // 查无此人：200 + 两个空数组（不是 404）
        let uri = format!("/api/artists/{}", encode("查无此人"));
        let (status, body) = authed(&state, &token, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["songs"].as_array().expect("数组").is_empty());
        assert!(body["albums"].as_array().expect("数组").is_empty());

        // 空白歌手名不是「所有歌手」
        let (status, body) = authed(&state, &token, "/api/artists/%20").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["songs"].as_array().expect("数组").is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 7. 鉴权（5 个接口逐个断言）
    // ─────────────────────────────────────────────────────────────────────────

    /// 我加的 UT：未登录访问这 5 个接口一律 401 + 统一错误形状。
    #[tokio::test]
    async fn every_library_route_requires_authentication() {
        let (state, _temp) = test_state("s16-auth");
        let routes = [
            "/api/library",
            "/api/songs/1",
            "/api/albums/1",
            "/api/artists/%E5%91%A8",
            "/api/search?q=x",
        ];
        for uri in routes {
            let (status, body) = call(&state, get(uri, None)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} 必须 401：{body}");
            assert_eq!(body["error"]["code"], "UNAUTHORIZED", "{uri}");
            assert!(body["error"].get("details").is_some(), "{uri} 统一形状");

            // 换个乱写的令牌也必须是 401，而不是掉进 handler
            let (status, _) = call(&state, get(uri, Some("not-a-jwt"))).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 8. 不泄漏内部字段
    // ─────────────────────────────────────────────────────────────────────────

    /// 我加的 UT：列表类接口不带 file_path / scrape_error / lyrics 等内部字段。
    #[tokio::test]
    async fn list_endpoints_never_leak_internal_fields() {
        let (state, _temp) = test_state("s16-leak");
        let token = issue_token(&state);
        let album_id = {
            let conn = state.db.acquire().expect("借连接");
            let album = seed_album(&conn, "专辑", "歌手");
            let id = seed(
                &conn,
                SeedSong {
                    path: "/srv/music/very/secret/path.mp3".to_string(),
                    title: "可搜到的标题".to_string(),
                    artists: Some("某歌手".to_string()),
                    album_id: Some(album),
                    lyrics: Some("很长很长的歌词".to_string()),
                    added_at: 1,
                    ..Default::default()
                },
            );
            conn.execute(
                "UPDATE songs SET scrape_error = '内部失败原因' WHERE id = ?1",
                params![id],
            )
            .expect("写内部字段");
            album
        };

        let (status, library) = authed(&state, &token, "/api/library").await;
        assert_eq!(status, StatusCode::OK);
        assert_no_internal_fields(&library["items"][0], false);
        assert!(
            !library.to_string().contains("secret/path"),
            "列表响应绝不能出现服务器绝对路径"
        );

        let uri = format!("/api/search?q={}", encode("可搜到"));
        let (_, search_body) = authed(&state, &token, &uri).await;
        assert_no_internal_fields(&search_body["items"][0], false);

        let uri = format!("/api/artists/{}", encode("某歌手"));
        let (_, artist_body) = authed(&state, &token, &uri).await;
        assert_no_internal_fields(&artist_body["songs"][0], false);

        let (_, album_body) =
            authed(&state, &token, &format!("/api/albums/{album_id}")).await;
        assert_no_internal_fields(&album_body["songs"][0], false);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 9. 软删不可见
    // ─────────────────────────────────────────────────────────────────────────

    /// 我加的 UT：软删的歌在 library / search / artist / album / 详情里都不可见。
    #[tokio::test]
    async fn soft_deleted_songs_are_invisible_everywhere() {
        let (state, _temp) = test_state("s16-soft-delete");
        let token = issue_token(&state);
        let (album_id, gone, keeps) = {
            let conn = state.db.acquire().expect("借连接");
            let album = seed_album(&conn, "专辑", "某歌手");
            let gone = seed(
                &conn,
                SeedSong {
                    path: "/music/gone.mp3".to_string(),
                    title: "消失的歌".to_string(),
                    artists: Some("某歌手".to_string()),
                    album_id: Some(album),
                    added_at: 1,
                    ..Default::default()
                },
            );
            let keeps = seed(
                &conn,
                SeedSong {
                    path: "/music/keeps.mp3".to_string(),
                    title: "还在的歌".to_string(),
                    artists: Some("某歌手".to_string()),
                    album_id: Some(album),
                    added_at: 2,
                    ..Default::default()
                },
            );
            songs::mark_deleted(&conn, gone).expect("标记软删");
            (album, gone, keeps)
        };

        let (status, body) = authed(&state, &token, "/api/library").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), vec![keeps], "软删的歌不该出现在列表里");
        assert_eq!(body["total"], 1, "total 也要排除软删");

        let uri = format!("/api/search?q={}", encode("消失的歌"));
        let (status, body) = authed(&state, &token, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ids(&body), Vec::<i64>::new(), "软删的歌搜不到");

        let uri = format!("/api/artists/{}", encode("某歌手"));
        let (status, body) = authed(&state, &token, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["songs"]
                .as_array()
                .expect("数组")
                .iter()
                .map(|item| item["id"].as_i64().expect("id"))
                .collect::<Vec<_>>(),
            vec![keeps],
            "歌手页不该出现软删的歌"
        );

        let (status, body) = authed(&state, &token, &format!("/api/albums/{album_id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["songs"].as_array().expect("数组").len(), 1);
        assert_eq!(body["songs"][0]["id"], keeps);

        let (status, body) = authed(&state, &token, &format!("/api/songs/{gone}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "软删的歌按 id 取必须是 404：{body}");
        assert_eq!(body["error"]["code"], "NOT_FOUND");
    }
}
