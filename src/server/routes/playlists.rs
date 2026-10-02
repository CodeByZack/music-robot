//! S22 · 播放列表 API：CRUD + 权限 + 排序。
//!
//! 路径（取自画布 API 清单）：
//!
//! * GET    /api/playlists                        列出可见歌单（公开的 + 自己的）
//! * POST   /api/playlists                        新建（owner = 当前用户）
//! * GET    /api/playlists/{id}                   详情（含曲目列表）
//! * PUT    /api/playlists/{id}                   改名 / 改描述 / 改 is_public
//! * DELETE /api/playlists/{id}                   删除（曲目随外键级联消失）
//! * POST   /api/playlists/{id}/items             加歌（幂等）
//! * DELETE /api/playlists/{id}/items/{song_id}   移出
//! * PUT    /api/playlists/{id}/items             整表重排（position）
//!
//! 8 条路由全部挂在 routes::build_router 的**受保护子 Router** 上（require_auth
//! 中间件），此外每个 handler 都显式提取 AuthUser —— 双重保险：将来有人把路由顺手
//! 挪进公开 Router 时仍会 401，而不是悄悄匿名可用。
//!
//! 响应体一律手写 serde_json::Value（项目规范：不引 serde derive）。
//!
//! # 权限：404 还是 403，判断依据在这里（别统一成 403）
//!
//! 画布规则原文：is_public=0 → 只有 owner 可读可改；is_public=1 → 所有人可读，
//! 仍只有 owner 可改。落到 HTTP 状态码上，**「读不到」和「改不动」不是一回事**：
//!
//! * **看不见的歌单 → 一律 404**。包括「不存在」和「is_public=0 且 viewer 不是 owner」
//!   两种情况，也**包括 owner 专属的写接口**：外人连它存在都不知道，谈不上「有权限改它」。
//!   这里若回 403，等于承认「这个歌单存在，只是你没权限」—— 攻击者用 403/404 的差异
//!   就能枚举出别人有多少私有歌单（存在性泄漏）。所以对调用方而言，别人的私有歌单与
//!   不存在的 id 必须**逐字无法区分**：同一个 hidden_playlist() 助手、同一句文案。
//! * **看得见的歌单（is_public=1）但不是 owner → 写接口 403**。它本来就公开，存在性不是
//!   秘密；此时缺的确实只是写权限，403 语义准确，前端也能据此提示「这是别人的歌单」，
//!   而不是把它当成链接失效。
//! * **owner 自己**：读写都 200。
//!
//! 判断落点只有两个助手：load_visible（读）与 load_owned（写）。要改口径就改它俩，
//! 不要在各自的 handler 里另判一套 —— 那正是「后人统一成 403」的入口。
//!
//! # 加歌幂等（画布 UT 明确要求）
//!
//! 同一首歌重复加进同一个歌单**不得产生第二条**（playlist_items 主键是
//! (playlist_id, song_id)）。第二次返回 **200**（不是 201，也不是 409），响应里用
//! added: false 体现「已存在」。理由：这是**幂等成功**，不是冲突 —— 弱网重试 / 双击
//! 重发同一个请求不该被当成错误；回 409 会逼前端把「重试」写成一个错误分支。首次成功
//! 仍用 201，与创建歌单口径一致。
//!
//! # 重排
//!
//! PUT .../items 收的是**完整的新顺序**（song_id 数组），必须与现有曲目集合**完全一致**
//! （不多不少、不重复），否则 400。校验通过后复用 playlists::reorder_item 逐个落位，
//! position 由 repo 保证是连续的 1..n（见该模块头注释的不变量）。
//!
//! # 不泄漏内部字段（与 S16 同口径）
//!
//! * 歌单只给 id / name / description / is_public / created_at / updated_at；
//!   **不给 user_id**：属主身份由 /api/auth/me 判断，对外只需要「是不是公开、叫什么」。
//! * 曲目用与 /api/songs/{id} **同一个** library::song_json 产出（传 false），
//!   因此不会暴露 file_path / audio_hash / search_text / scrape_error 等内部列。
//!   **不带 lyrics**：歌单可能有几十首，歌词动辄几十 KB，这里按列表口径处理
//!   （要歌词请按 id 走 /api/songs/{id}）。
//! * 详情里的 songs 数组**顺序即 position 顺序**，不再单独回传 position 列
//!   （前端拿数组顺序即可；重排接口收的也是同一串数组）。
//! * 软删的歌虽然还留在 playlist_items 里（歌曲是软删，不真删），但已经不可见，
//!   详情里直接跳过，不返回 404 —— 一首歌被删掉不该让整个歌单打不开。
//!
//! # 阻塞调用
//!
//! rusqlite 全是同步阻塞调用，统一用 library::run_db 包进 spawn_blocking
//! （S14 铁律），绝不在 async 上下文里直接碰连接池。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::db::models::{Playlist, PlaylistItem};
use crate::db::repos::playlists;
use crate::db::repos::songs;
use crate::db::repos::RepoError;
use crate::server::auth::AuthUser;
use crate::server::error::{ApiError, ApiResult};
use crate::server::state::AppState;

use super::library::{parse_id, run_db, song_json};

/// 歌单名字符数上限。
///
/// 画布没规定，这里只是防一手「把整本书塞进 name」的输入：NOT NULL 挡不住超长文本，
/// 而不设上限的 TEXT 列会原样进库、进响应体。200 个字符对歌单名足够宽裕。
const MAX_NAME_CHARS: usize = 200;

// ─────────────────────────────────────────────────────────────────────────────
// 权限判定（404 / 403 的唯一落点，见模块头注释）
// ─────────────────────────────────────────────────────────────────────────────

/// 「看不见」的统一答复。
///
/// 单独抽成函数而不是各处手写 ApiError::not_found(...)：文案一旦漂移，别人私有歌单
/// 与不存在 id 的两条路径就能被区分出来，存在性照样泄漏。
fn hidden_playlist() -> ApiError {
    ApiError::not_found("请求的歌单不存在")
}

/// 读路径：歌单不存在、或 viewer 看不见它 → 404（两种情况对外完全一样）。
fn load_visible(conn: &Connection, id: i64, viewer_id: i64) -> Result<Playlist, ApiError> {
    match playlists::get(conn, id)? {
        // 可见 = 公开的，或自己的（含自己的私有）。
        Some(playlist) if playlist.is_public || playlist.user_id == viewer_id => Ok(playlist),
        _ => Err(hidden_playlist()),
    }
}

/// 写路径：看不见 → 404（别泄漏私有歌单的存在性）；看得见但不是 owner → 403。
fn load_owned(conn: &Connection, id: i64, viewer_id: i64) -> Result<Playlist, ApiError> {
    let playlist = load_visible(conn, id, viewer_id)?;
    if playlist.user_id == viewer_id {
        Ok(playlist)
    } else {
        Err(ApiError::forbidden(
            "这是别人的公开歌单，只有属主可以修改或删除",
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 请求体解析与校验
// ─────────────────────────────────────────────────────────────────────────────

/// 请求体必须是 JSON 对象（否则字段全取不到，报错会含糊）。
fn require_object(body: &Value) -> Result<(), ApiError> {
    if body.is_object() {
        Ok(())
    } else {
        Err(ApiError::bad_request("请求体必须是 JSON 对象"))
    }
}

/// 取一个字符串字段，返回「是否出现」与「值」的两层结构：
///   * None         = 字段缺席（PUT 里表示保持原值）；
///   * Some(None)   = 显式 null（只有 description 用得上：清空描述）；
///   * Some(Some(s)) = 字符串，已 trim。
fn optional_string(body: &Value, key: &str) -> Result<Option<Option<String>>, ApiError> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(Value::String(text)) => Ok(Some(Some(text.trim().to_string()))),
        Some(_) => Err(ApiError::bad_request(format!(
            "字段 {key} 必须是字符串或 null"
        ))),
    }
}

/// 取一个布尔字段；字段缺席返回 None。
fn optional_bool(body: &Value, key: &str) -> Result<Option<bool>, ApiError> {
    match body.get(key) {
        None => Ok(None),
        Some(Value::Bool(flag)) => Ok(Some(*flag)),
        Some(_) => Err(ApiError::bad_request(format!("字段 {key} 必须是布尔值"))),
    }
}

/// 歌单名：trim 后非空、且不超长。返回规范化后的名字。
fn checked_name(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("歌单名不能为空"));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::bad_request(format!(
            "歌单名不能超过 {MAX_NAME_CHARS} 个字符"
        )));
    }
    Ok(name.to_string())
}

/// 解析重排请求里的 song_ids 数组；缺失 / 非数组 / 元素不是整数都 400。
fn parse_song_ids(body: &Value) -> Result<Vec<i64>, ApiError> {
    let raw = body
        .get("song_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("缺少 song_ids 数组"))?;
    let mut ids = Vec::with_capacity(raw.len());
    for value in raw {
        let id = value
            .as_i64()
            .ok_or_else(|| ApiError::bad_request("song_ids 的每一项都必须是整数"))?;
        ids.push(id);
    }
    Ok(ids)
}

/// 两个 song_id 列表是否「完全一致」：长度相同、排序后逐项相等。
///
/// 排序比较同时排除了请求里的重复项：现有曲目是主键去重的，请求里若有重复，排序后
/// 必然对不上（例如现有 [1,2,3] vs 请求 [1,2,2]）。
fn same_song_set(current: &[i64], requested: &[i64]) -> bool {
    if current.len() != requested.len() {
        return false;
    }
    let mut left = current.to_vec();
    let mut right = requested.to_vec();
    left.sort_unstable();
    right.sort_unstable();
    left == right
}

// ─────────────────────────────────────────────────────────────────────────────
// 对外 JSON
// ─────────────────────────────────────────────────────────────────────────────

/// 歌单的对外 JSON（不含 user_id 等内部信息，见模块头）。
///
/// `is_owner` 是**必需的**，不是可有可无的装饰：模块头写着「403 语义准确，前端也能
/// 据此提示『这是别人的歌单』」—— 但**光靠 403 做不到**：那个提示要在**打开歌单之前**
/// 就出现在列表页上，而列表页不会为了每条去试一次写请求。前端需要的是「我能不能改这个
/// 歌单」这一个布尔量。
///
/// 为什么给布尔量而不是 `user_id`：`user_id` 是内部标识（也是别人的账号 id），
/// 而前端要回答的问题只有「是不是我的」。两者信息量不同 —— 前者会把「谁建的」
/// 也漏出去。所以这里传 `viewer_id` 算一次比较，而不是把原始字段递出去。
fn playlist_json(playlist: &Playlist, viewer_id: i64) -> Value {
    json!({
        "id": playlist.id,
        "name": playlist.name,
        "description": playlist.description,
        "is_public": playlist.is_public,
        "is_owner": playlist.user_id == viewer_id,
        "created_at": playlist.created_at,
        "updated_at": playlist.updated_at,
    })
}

/// 在歌单里找一首歌的条目；找不到返回 None。
///
/// 复用 repo 的 list_items 而不是自己写 SQL：歌单条目数量很小（线性扫足够），
/// 而且 position 不变量只在 repo 里维护，这里不要另起一套读取口径。
fn find_item(
    conn: &Connection,
    playlist_id: i64,
    song_id: i64,
) -> Result<Option<PlaylistItem>, ApiError> {
    Ok(playlists::list_items(conn, playlist_id)?
        .into_iter()
        .find(|item| item.song_id == song_id))
}

/// 加歌的结果：新加进去 / 本来就在（幂等分支）。
enum AddOutcome {
    /// 新插入，附带最终 position
    Added(i64),
    /// 已经存在，附带它当前的 position
    AlreadyThere(i64),
}

// ─────────────────────────────────────────────────────────────────────────────
// handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/playlists —— 列出 viewer 可见的歌单：自己的全部 + 别人的公开歌单。
///
/// 不分页：歌单是用户自己攒的，量级小；脏页语义在画布上也没定义。响应给
/// { items, total }，与曲库列表的 items 形状一致。
pub async fn list(auth: AuthUser, State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let viewer = auth.0.id;
    let rows = run_db(Arc::clone(&state.db), move |conn| {
        Ok(playlists::list_visible_to(conn, Some(viewer))?)
    })
    .await?;

    let items: Vec<Value> = rows.iter().map(|p| playlist_json(p, viewer)).collect();
    let total = items.len();
    Ok(Json(json!({ "items": items, "total": total })))
}

/// POST /api/playlists —— 新建歌单，owner 恒为当前登录用户（绝不从请求体取 user_id）。
///
/// 成功 201 + { playlist }（与 S15 注册接口的 201 口径一致）。
pub async fn create(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    require_object(&body)?;
    let name = match optional_string(&body, "name")? {
        Some(Some(raw)) => checked_name(&raw)?,
        // 缺席或显式 null 都是「没给名字」
        _ => return Err(ApiError::bad_request("新建歌单必须提供非空的 name")),
    };
    // description 缺席与显式 null 等价（都是没有描述）；
    // is_public 缺席默认私有（画布默认值 0）。
    let description = optional_string(&body, "description")?.flatten();
    let is_public = optional_bool(&body, "is_public")?.unwrap_or(false);
    let user_id = auth.0.id;

    let created = run_db(Arc::clone(&state.db), move |conn| {
        let new = Playlist {
            id: 0,
            user_id,
            name,
            description,
            is_public,
            created_at: 0,
            updated_at: 0,
        };
        let id = playlists::insert(conn, &new)?;
        // 写后重读：created_at / updated_at 是 repo 盖的章，直接回给前端做展示基准。
        playlists::get(conn, id)?
            .ok_or_else(|| ApiError::internal("歌单刚写入就查不到了，怀疑有并发写入者"))
    })
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "playlist": playlist_json(&created, user_id) })),
    ))
}

/// GET /api/playlists/{id} —— 详情（含曲目列表）。
///
/// 看不见的歌单（不存在 / 别人的私有）→ 404，两者不可区分（见模块头）。
pub async fn detail(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    let viewer = auth.0.id;

    let (playlist, tracks) = run_db(Arc::clone(&state.db), move |conn| {
        let playlist = load_visible(conn, id, viewer)?;
        let items = playlists::list_items(conn, id)?;
        let mut tracks = Vec::with_capacity(items.len());
        for item in items {
            // 软删的歌仍留在条目表里（歌是软删不是真删），但已不可见 —— 跳过而不是 404。
            if let Some(song) = songs::get(conn, item.song_id, false)? {
                tracks.push(song_json(&song, false));
            }
        }
        Ok((playlist, tracks))
    })
    .await?;

    Ok(Json(json!({
        "playlist": playlist_json(&playlist, viewer),
        "songs": tracks,
    })))
}

/// PUT /api/playlists/{id} —— 改名 / 改描述 / 改 is_public（**部分更新**：字段缺席即不动）。
///
/// 权限：看不见 → 404；看得见但不是 owner → 403。
pub async fn update(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    require_object(&body)?;

    // 先解析再做任何写操作：参数不合法时不该先动数据库。
    let new_name = match optional_string(&body, "name")? {
        None => None,
        Some(Some(raw)) => Some(checked_name(&raw)?),
        Some(None) => return Err(ApiError::bad_request("歌单名不能为 null")),
    };
    // 外层 Some = 字段出现了；内层 None = 显式 null（清空描述）。
    let new_description = optional_string(&body, "description")?;
    let new_public = optional_bool(&body, "is_public")?;
    let viewer = auth.0.id;

    let updated = run_db(Arc::clone(&state.db), move |conn| {
        let mut playlist = load_owned(conn, id, viewer)?;
        if let Some(name) = new_name {
            playlist.name = name;
        }
        if let Some(description) = new_description {
            playlist.description = description;
        }
        if let Some(is_public) = new_public {
            playlist.is_public = is_public;
        }
        playlists::update(conn, &playlist)?;
        playlists::get(conn, id)?
            .ok_or_else(|| ApiError::internal("歌单刚更新就查不到了，怀疑有并发删除"))
    })
    .await?;

    Ok(Json(json!({ "playlist": playlist_json(&updated, viewer) })))
}

/// DELETE /api/playlists/{id} —— 删除歌单。
///
/// playlist_items 是 ON DELETE CASCADE，条目随歌单一起消失；这里不手动清表
/// （手动清反而会在「删一半失败」时留下不一致）。测试直接查库证明级联生效。
pub async fn delete(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    let viewer = auth.0.id;

    let removed = run_db(Arc::clone(&state.db), move |conn| {
        // 先按写权限判定（看不见 404 / 非 owner 403），再删。
        load_owned(conn, id, viewer)?;
        Ok(playlists::delete(conn, id)?)
    })
    .await?;

    Ok(Json(json!({ "id": id, "deleted": removed > 0 })))
}

/// POST /api/playlists/{id}/items —— 加歌（幂等）。
///
/// * 首次加入 → **201** + added: true；
/// * 已在歌单里 → **200** + added: false（理由见模块头：幂等成功不是冲突）；
/// * song_id 不存在 / 已软删 → **404**（不能让它变成外键报错的 500）。
pub async fn add_item(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let playlist_id = parse_id(&raw_id)?;
    require_object(&body)?;
    let song_id = body
        .get("song_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::bad_request("缺少整数 song_id"))?;
    // position 可选：给了就插到那里（repo 会把越界值夹进合法区间），
    // 缺席 / null 都表示追加到末尾。
    let position = match body.get("position") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .ok_or_else(|| ApiError::bad_request("position 必须是整数"))?,
        ),
    };
    let viewer = auth.0.id;

    let outcome = run_db(Arc::clone(&state.db), move |conn| {
        load_owned(conn, playlist_id, viewer)?;
        // 先确认歌存在（且没软删）：不查直接插，外键失败会被收敛成 500，
        // 但「给不存在的歌加歌单」是客户端错误，必须是 404。
        if songs::get(conn, song_id, false)?.is_none() {
            return Err(ApiError::not_found("请求的歌曲不存在"));
        }
        // 幂等：已在歌单里直接回「已存在」，不再插第二行。
        if let Some(existing) = find_item(conn, playlist_id, song_id)? {
            return Ok(AddOutcome::AlreadyThere(existing.position));
        }
        match playlists::add_item(conn, playlist_id, song_id, position) {
            Ok(target) => Ok(AddOutcome::Added(target)),
            // 并发下两个请求都通过了存在性检查，后到的那个撞上主键 —— 结果等价于「已存在」。
            Err(RepoError::Conflict { .. }) => {
                let current = find_item(conn, playlist_id, song_id)?
                    .map(|item| item.position)
                    // 理论上冲突一定伴随已存在行；真取不到就退回 0，
                    // 而不是 unwrap 让生产路径 panic。
                    .unwrap_or(0);
                Ok(AddOutcome::AlreadyThere(current))
            }
            Err(other) => Err(other.into()),
        }
    })
    .await?;

    match outcome {
        AddOutcome::Added(position) => Ok((
            StatusCode::CREATED,
            Json(json!({
                "added": true,
                "song_id": song_id,
                "position": position,
            })),
        )),
        AddOutcome::AlreadyThere(position) => Ok((
            StatusCode::OK,
            Json(json!({
                "added": false,
                "song_id": song_id,
                "position": position,
                "message": "这首歌已经在歌单里了",
            })),
        )),
    }
}

/// DELETE /api/playlists/{id}/items/{song_id} —— 把一首歌移出歌单。
///
/// 删一首不在歌单里的歌返回 200 + removed: false（DELETE 语义上幂等，重复调用不该报错）。
/// repo 会在删除后把剩下的条目重排成连续的 1..n。
pub async fn remove_item(
    auth: AuthUser,
    State(state): State<AppState>,
    Path((raw_id, raw_song_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let playlist_id = parse_id(&raw_id)?;
    let song_id = parse_id(&raw_song_id)?;
    let viewer = auth.0.id;

    let removed = run_db(Arc::clone(&state.db), move |conn| {
        load_owned(conn, playlist_id, viewer)?;
        Ok(playlists::remove_item(conn, playlist_id, song_id)?)
    })
    .await?;

    Ok(Json(json!({ "song_id": song_id, "removed": removed > 0 })))
}

/// PUT /api/playlists/{id}/items —— 整表重排。
///
/// 请求体：{ "song_ids": [3, 1, 2] }（完整的新顺序）。必须与现有曲目集合完全一致，
/// 否则 400 —— 缺一首 / 多一首都说明前端拿的是过期数据，静默忽略差异会让用户以为
/// 顺序保存了，其实没有。
pub async fn reorder_items(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let playlist_id = parse_id(&raw_id)?;
    require_object(&body)?;
    let requested = parse_song_ids(&body)?;
    let viewer = auth.0.id;

    let order = run_db(Arc::clone(&state.db), move |conn| {
        load_owned(conn, playlist_id, viewer)?;
        let current: Vec<i64> = playlists::list_items(conn, playlist_id)?
            .into_iter()
            .map(|item| item.song_id)
            .collect();
        if !same_song_set(&current, &requested) {
            return Err(ApiError::bad_request(format!(
                "重排数组必须与歌单现有曲目完全一致：现有 {} 首，请求 {} 首",
                current.len(),
                requested.len()
            )));
        }
        // 逐首挪到目标位次。repo 的 reorder_item 每次都会把 position 重写成连续的 1..n，
        // 依次处理后最终顺序就是请求的顺序（歌单很小，O(n²) 的挪动可以接受）。
        for (index, song_id) in requested.iter().enumerate() {
            let target = index as i64 + 1;
            playlists::reorder_item(conn, playlist_id, *song_id, target)?;
        }
        Ok(requested)
    })
    .await?;

    let count = order.len();
    Ok(Json(json!({ "song_ids": order, "count": count })))
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
    use crate::db::pool::{DbPool, TempDb};
    use crate::server::auth::sign_token_with_ttl;
    use crate::server::routes::build_router;

    /// 测试用 JWT 密钥。
    const SECRET: &str = "s22-secret";

    /// 跑完迁移的临时文件库状态（画布指定：DbPool::open_temp）。
    /// 必须持有返回的 TempDb，它一析构就会删库文件。
    fn test_state(tag: &str) -> (AppState, TempDb) {
        let (pool, temp) = DbPool::open_temp(tag).expect("建临时文件库池");
        {
            let mut guard = pool.acquire().expect("借连接");
            migrations::apply(&mut guard).expect("应用迁移");
        }
        let mut config = Config::defaults();
        config.server.jwt_secret = SECRET.to_string();
        config.storage.library_roots = vec!["/tmp/server-s22-playlists-music".to_string()];
        (AppState::new(Arc::new(pool), Arc::new(config)), temp)
    }

    /// 预置一个用户并签一个可用令牌（不走注册接口，省掉 argon2 的开销）。
    /// owner 与外人都走这里 —— 「两个用户」是这组权限 UT 的前提。
    fn issue_token(state: &AppState, username: &str) -> String {
        let id = {
            let conn = state.db.acquire().expect("借连接");
            conn.execute(
                "INSERT INTO users (username, password_hash, role, created_at)
                 VALUES (?1, 'hash', 'user', ?2)",
                params![username, crate::db::now_unix_ms()],
            )
            .expect("预置用户");
            conn.last_insert_rowid()
        };
        let user = User {
            id,
            username: username.to_string(),
            password_hash: "hash".to_string(),
            role: Role::User,
            created_at: 0,
            last_login: None,
        };
        sign_token_with_ttl(SECRET, &user, 3600).expect("签发令牌")
    }

    /// 预置一首歌，返回 id。
    fn seed_song(state: &AppState, file_path: &str) -> i64 {
        let conn = state.db.acquire().expect("借连接");
        conn.execute(
            "INSERT INTO songs (file_path, title, search_text, scrape_status, added_at, updated_at)
             VALUES (?1, '测试曲目', '测试曲目', 'pending', 1, 1)",
            params![file_path],
        )
        .expect("预置歌曲");
        conn.last_insert_rowid()
    }

    /// 构造请求：body 为 None 时不带请求体，Some 时带 JSON body + Content-Type。
    fn api(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        match body {
            Some(value) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(value.to_string()))
                .expect("构造带 JSON body 的请求"),
            None => builder.body(Body::empty()).expect("构造空 body 请求"),
        }
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
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("响应体必须是合法 JSON")
        };
        (status, body)
    }

    /// 建一个歌单，返回 id。
    async fn create_playlist(state: &AppState, token: &str, name: &str, is_public: bool) -> i64 {
        let (status, body) = call(
            state,
            api(
                "POST",
                "/api/playlists",
                Some(token),
                Some(json!({ "name": name, "is_public": is_public })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "建歌单失败：{body}");
        body["playlist"]["id"].as_i64().expect("歌单 id 是整数")
    }

    /// 直接查库：歌单里的 (song_id, position) 列表，按 position 升序。
    fn stored_items(state: &AppState, playlist_id: i64) -> Vec<(i64, i64)> {
        let conn = state.db.acquire().expect("借连接");
        playlists::list_items(&conn, playlist_id)
            .expect("读条目")
            .into_iter()
            .map(|item| (item.song_id, item.position))
            .collect()
    }

    /// 直接查库：playlist_items 里属于该歌单的行数（级联删除用）。
    fn stored_item_rows(state: &AppState, playlist_id: i64) -> i64 {
        let conn = state.db.acquire().expect("借连接");
        conn.query_row(
            "SELECT COUNT(*) FROM playlist_items WHERE playlist_id = ?1",
            params![playlist_id],
            |row| row.get(0),
        )
        .expect("数条目行")
    }

    /// 列表响应里的歌单 id（顺序敏感）。
    fn listed_ids(body: &Value) -> Vec<i64> {
        body["items"]
            .as_array()
            .expect("items 是数组")
            .iter()
            .map(|item| item["id"].as_i64().expect("id 是整数"))
            .collect()
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 1. owner 校验
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「owner 校验」：owner 能改能删；外人改公开歌单是 403、删也是 403。
    #[tokio::test]
    async fn owner_can_update_and_delete_while_outsiders_get_403_on_public_playlists() {
        let (state, _temp) = test_state("s22-owner");
        let alice = issue_token(&state, "alice");
        let bob = issue_token(&state, "bob");

        // 公开歌单：外人看得见，所以缺的只是写权限 → 403（不是 404）。
        let public_id = create_playlist(&state, &alice, "alice 的公开歌单", true).await;

        let (status, body) = call(
            &state,
            api(
                "PUT",
                &format!("/api/playlists/{public_id}"),
                Some(&bob),
                Some(json!({ "name": "改别人的" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "外人改公开歌单必须 403：{body}");
        assert_eq!(body["error"]["code"], "FORBIDDEN");

        let (status, body) = call(
            &state,
            api(
                "DELETE",
                &format!("/api/playlists/{public_id}"),
                Some(&bob),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "外人删公开歌单必须 403：{body}");
        assert_eq!(body["error"]["code"], "FORBIDDEN");

        // owner 自己读 / 改都 200
        let (status, body) = call(
            &state,
            api(
                "GET",
                &format!("/api/playlists/{public_id}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "owner 读自己的歌单必须 200：{body}");
        // 属主视角：is_owner = true（界面据此显示「编辑 / 删除」）
        assert_eq!(body["playlist"]["is_owner"], true, "owner 看自己应 is_owner=true：{body}");

        // 同一个公开歌单，换外人来看：看得见，但 is_owner = false。
        // 这条是**界面正确性的前提** —— 列表页要在这条歌单上少画两个按钮，
        // 而它只发了这一个 GET，不会为了每条去试写请求（靠 403 反推在列表上做不到）。
        let (status, body) = call(
            &state,
            api(
                "GET",
                &format!("/api/playlists/{public_id}"),
                Some(&bob),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "公开歌单外人读得到：{body}");
        assert_eq!(body["playlist"]["is_owner"], false, "外人看应 is_owner=false：{body}");
        // 仍然不泄漏 user_id（is_owner 是布尔量，不是原始标识）
        assert!(
            body["playlist"].get("user_id").is_none(),
            "响应里不能出现 user_id：{body}"
        );

        // 列表接口同样带上 is_owner（列表页就是靠它决定画不画编辑入口）。
        // 两个视角看**同一条歌单**，拿到的布尔量必须相反 —— 这是这个字段的全部意义。
        let (status, body) = call(&state, api("GET", "/api/playlists", Some(&bob), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let seen_by_bob = body["items"]
            .as_array()
            .expect("items 是数组")
            .iter()
            .find(|p| p["id"] == public_id)
            .expect("bob 应能在列表里看到 alice 的公开歌单")
            .clone();
        assert_eq!(
            seen_by_bob["is_owner"], false,
            "外人视角的列表里应 is_owner=false：{seen_by_bob}"
        );

        let (status, body) = call(&state, api("GET", "/api/playlists", Some(&alice), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let seen_by_alice = body["items"]
            .as_array()
            .expect("items 是数组")
            .iter()
            .find(|p| p["id"] == public_id)
            .expect("alice 应能在列表里看到自己的歌单")
            .clone();
        assert_eq!(
            seen_by_alice["is_owner"], true,
            "属主视角的列表里应 is_owner=true：{seen_by_alice}"
        );
        assert!(
            body.to_string().find("user_id").is_none(),
            "列表里也不能出现 user_id：{body}"
        );

        let (status, body) = call(
            &state,
            api(
                "PUT",
                &format!("/api/playlists/{public_id}"),
                Some(&alice),
                Some(json!({
                    "name": "alice 改名",
                    "description": "改过了",
                    "is_public": false,
                })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "owner 改自己的歌单必须 200：{body}");
        assert_eq!(body["playlist"]["name"], "alice 改名");
        assert_eq!(body["playlist"]["description"], "改过了");
        assert_eq!(body["playlist"]["is_public"], false);

        // owner 删 → 200，且此后确实查不到
        let (status, body) = call(
            &state,
            api(
                "DELETE",
                &format!("/api/playlists/{public_id}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "owner 删自己的歌单必须 200：{body}");
        assert_eq!(body["deleted"], true);
        let (status, _) = call(
            &state,
            api(
                "GET",
                &format!("/api/playlists/{public_id}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "删掉之后按 id 取应是 404");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 2. is_public=0 他人不可读；改成公开后能读但不能改
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「is_public=0 他人不可读」+ 画布权限原文的后半句。
    #[tokio::test]
    async fn private_playlist_is_invisible_then_public_is_readable_but_not_writable() {
        let (state, _temp) = test_state("s22-private");
        let alice = issue_token(&state, "alice");
        let bob = issue_token(&state, "bob");

        let private_id = create_playlist(&state, &alice, "alice 的私有歌单", false).await;

        // 外人读 → 404（不是 403：403 会承认它存在）
        let (status, body) = call(
            &state,
            api(
                "GET",
                &format!("/api/playlists/{private_id}"),
                Some(&bob),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "别人的私有歌单必须 404：{body}");
        assert_eq!(body["error"]["code"], "NOT_FOUND");

        // 与「真的不存在」逐字一致 —— 否则等于泄漏私有歌单的存在性
        let (missing_status, missing_body) = call(
            &state,
            api("GET", "/api/playlists/999999", Some(&bob), None),
        )
        .await;
        assert_eq!(missing_status, StatusCode::NOT_FOUND);
        assert_eq!(
            body, missing_body,
            "别人的私有歌单与不存在的歌单必须无法区分"
        );

        // 外人列表里看不到它
        let (status, list) = call(&state, api("GET", "/api/playlists", Some(&bob), None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            !listed_ids(&list).contains(&private_id),
            "别人的私有歌单不该出现在列表里：{list}"
        );
        assert_eq!(list["total"], 0);

        // owner 自己读得到
        let (status, body) = call(
            &state,
            api(
                "GET",
                &format!("/api/playlists/{private_id}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "owner 读自己的私有歌单必须 200：{body}");
        assert_eq!(body["playlist"]["id"], private_id);
        assert_eq!(body["playlist"]["is_public"], false);

        // 改成公开：外人能读（200），但仍然不能改（403）
        let (status, body) = call(
            &state,
            api(
                "PUT",
                &format!("/api/playlists/{private_id}"),
                Some(&alice),
                Some(json!({ "is_public": true })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["playlist"]["is_public"], true);

        let (status, body) = call(
            &state,
            api(
                "GET",
                &format!("/api/playlists/{private_id}"),
                Some(&bob),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "公开后外人必须能读：{body}");
        assert_eq!(body["playlist"]["id"], private_id);

        let (status, body) = call(
            &state,
            api(
                "PUT",
                &format!("/api/playlists/{private_id}"),
                Some(&bob),
                Some(json!({ "name": "bob 改名" })),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "公开歌单仍只有 owner 能改：{body}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 3. 级联删除
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「级联删除」：删歌单后**直接查库**证明 playlist_items 没有残留行。
    #[tokio::test]
    async fn deleting_a_playlist_cascades_to_its_items() {
        let (state, _temp) = test_state("s22-cascade");
        let alice = issue_token(&state, "alice");
        let playlist_id = create_playlist(&state, &alice, "带歌的歌单", false).await;
        let song_a = seed_song(&state, "/music/a.mp3");
        let song_b = seed_song(&state, "/music/b.mp3");

        for song_id in [song_a, song_b] {
            let (status, body) = call(
                &state,
                api(
                    "POST",
                    &format!("/api/playlists/{playlist_id}/items"),
                    Some(&alice),
                    Some(json!({ "song_id": song_id })),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }
        assert_eq!(stored_item_rows(&state, playlist_id), 2);

        let (status, body) = call(
            &state,
            api(
                "DELETE",
                &format!("/api/playlists/{playlist_id}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        assert_eq!(
            stored_item_rows(&state, playlist_id),
            0,
            "歌单一删，playlist_items 必须被级联清空"
        );
        assert!(stored_items(&state, playlist_id).is_empty());

        // 级联只删条目，歌本身还在
        let conn = state.db.acquire().expect("借连接");
        let songs_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM songs WHERE id IN (?1, ?2)",
                params![song_a, song_b],
                |row| row.get(0),
            )
            .expect("数歌");
        assert_eq!(songs_left, 2, "删歌单不该把歌曲本身删掉");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 4. position 重排
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「position 重排」：整表重排后顺序与请求一致、position 连续 1..n；
    /// 与现有曲目不一致的数组 → 400（且一行都不改）。
    #[tokio::test]
    async fn reorder_rewrites_positions_to_a_contiguous_run() {
        let (state, _temp) = test_state("s22-reorder");
        let alice = issue_token(&state, "alice");
        let playlist_id = create_playlist(&state, &alice, "顺序", false).await;
        let songs: Vec<i64> = (0..3)
            .map(|index| seed_song(&state, &format!("/music/{index}.mp3")))
            .collect();

        for song_id in &songs {
            let (status, body) = call(
                &state,
                api(
                    "POST",
                    &format!("/api/playlists/{playlist_id}/items"),
                    Some(&alice),
                    Some(json!({ "song_id": song_id })),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }

        // 倒序重排
        let reversed: Vec<i64> = songs.iter().rev().copied().collect();
        let (status, body) = call(
            &state,
            api(
                "PUT",
                &format!("/api/playlists/{playlist_id}/items"),
                Some(&alice),
                Some(json!({ "song_ids": reversed })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["count"], 3);

        let items = stored_items(&state, playlist_id);
        let order: Vec<i64> = items.iter().map(|(song_id, _)| *song_id).collect();
        assert_eq!(order, reversed, "顺序必须与请求一致");
        assert_eq!(
            items.iter().map(|(_, position)| *position).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "position 必须是连续的 1..3"
        );

        // 数组与现有曲目不一致 → 400，且顺序不动
        for bad in [
            json!({ "song_ids": [songs[0]] }),
            json!({ "song_ids": [songs[0], songs[1], songs[2], 999_999] }),
            json!({ "song_ids": [songs[0], songs[0], songs[1]] }),
            json!({ "song_ids": [] }),
            json!({}),
            json!({ "song_ids": "not-an-array" }),
        ] {
            let (status, body) = call(
                &state,
                api(
                    "PUT",
                    &format!("/api/playlists/{playlist_id}/items"),
                    Some(&alice),
                    Some(bad.clone()),
                ),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "数组 {bad} 必须 400：{body}"
            );
            assert_eq!(body["error"]["code"], "BAD_REQUEST");
        }
        assert_eq!(stored_items(&state, playlist_id), items, "400 时不该改动任何一行");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 5. 重复加歌幂等
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「重复加歌幂等」：第二次 200 + added:false，且库里只有一行。
    #[tokio::test]
    async fn adding_the_same_song_twice_is_idempotent() {
        let (state, _temp) = test_state("s22-idempotent");
        let alice = issue_token(&state, "alice");
        let playlist_id = create_playlist(&state, &alice, "幂等", false).await;
        let song_id = seed_song(&state, "/music/once.mp3");

        let (status, body) = call(
            &state,
            api(
                "POST",
                &format!("/api/playlists/{playlist_id}/items"),
                Some(&alice),
                Some(json!({ "song_id": song_id })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "首次加歌必须 201：{body}");
        assert_eq!(body["added"], true);
        assert_eq!(body["position"], 1);

        let (status, body) = call(
            &state,
            api(
                "POST",
                &format!("/api/playlists/{playlist_id}/items"),
                Some(&alice),
                Some(json!({ "song_id": song_id })),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "重复加歌必须是 200（幂等成功，不是 409）：{body}"
        );
        assert_eq!(body["added"], false, "响应要体现「已存在」");
        assert_eq!(body["song_id"], song_id);

        assert_eq!(
            stored_items(&state, playlist_id),
            vec![(song_id, 1)],
            "同一首歌在同一个歌单里只能有一行"
        );
        assert_eq!(stored_item_rows(&state, playlist_id), 1);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 6. 我加的：未登录 401
    // ─────────────────────────────────────────────────────────────────────────

    /// 我加的 UT：8 条路由未登录一律 401 + 统一错误形状。
    #[tokio::test]
    async fn every_playlist_route_requires_login() {
        let (state, _temp) = test_state("s22-401");
        let routes: [(&str, &str, Option<Value>); 8] = [
            ("GET", "/api/playlists", None),
            ("POST", "/api/playlists", Some(json!({ "name": "x" }))),
            ("GET", "/api/playlists/1", None),
            ("PUT", "/api/playlists/1", Some(json!({ "name": "x" }))),
            ("DELETE", "/api/playlists/1", None),
            ("POST", "/api/playlists/1/items", Some(json!({ "song_id": 1 }))),
            ("DELETE", "/api/playlists/1/items/1", None),
            ("PUT", "/api/playlists/1/items", Some(json!({ "song_ids": [] }))),
        ];

        for (method, uri, body) in routes {
            let (status, response) = call(&state, api(method, uri, None, body.clone())).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {uri} 未登录必须 401：{response}"
            );
            assert_eq!(response["error"]["code"], "UNAUTHORIZED", "{method} {uri}");

            // 伪造令牌同样 401，且绝不能掉进 handler
            let (status, _) = call(&state, api(method, uri, Some("not-a-jwt"), body)).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {uri} 伪造令牌必须 401"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 7. 我加的：加不存在的歌 → 404
    // ─────────────────────────────────────────────────────────────────────────

    /// 我加的 UT：给不存在的歌加歌单 → 404（不是外键报错收敛出来的 500）。
    #[tokio::test]
    async fn adding_a_missing_song_is_404_not_500() {
        let (state, _temp) = test_state("s22-missing-song");
        let alice = issue_token(&state, "alice");
        let playlist_id = create_playlist(&state, &alice, "空歌单", false).await;

        let (status, body) = call(
            &state,
            api(
                "POST",
                &format!("/api/playlists/{playlist_id}/items"),
                Some(&alice),
                Some(json!({ "song_id": 999_999 })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "不存在的歌必须 404：{body}");
        assert_eq!(body["error"]["code"], "NOT_FOUND");
        assert_eq!(stored_item_rows(&state, playlist_id), 0);

        // 软删的歌同样不可加（对用户已经不存在）
        let gone = seed_song(&state, "/music/gone.mp3");
        {
            let conn = state.db.acquire().expect("借连接");
            songs::mark_deleted(&conn, gone).expect("标记软删");
        }
        let (status, body) = call(
            &state,
            api(
                "POST",
                &format!("/api/playlists/{playlist_id}/items"),
                Some(&alice),
                Some(json!({ "song_id": gone })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "软删的歌也必须 404：{body}");

        // song_id 缺失 / 类型不对是 400，不是 500
        for bad in [json!({}), json!({ "song_id": "abc" })] {
            let (status, _) = call(
                &state,
                api(
                    "POST",
                    &format!("/api/playlists/{playlist_id}/items"),
                    Some(&alice),
                    Some(bad),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 8. 我加的：列表只含可见的
    // ─────────────────────────────────────────────────────────────────────────

    /// 我加的 UT：列表恰好只含可见的歌单（自己的私有 + 自己的公开），别人的私有看不到。
    #[tokio::test]
    async fn list_contains_exactly_the_visible_playlists() {
        let (state, _temp) = test_state("s22-list");
        let alice = issue_token(&state, "alice");
        let bob = issue_token(&state, "bob");

        let alice_private = create_playlist(&state, &alice, "alice 私有", false).await;
        let alice_public = create_playlist(&state, &alice, "alice 公开", true).await;
        let bob_private = create_playlist(&state, &bob, "bob 私有", false).await;

        // alice 视角：恰好是前两个
        let (status, body) = call(&state, api("GET", "/api/playlists", Some(&alice), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            listed_ids(&body),
            vec![alice_private, alice_public],
            "alice 的列表必须恰好是「自己的私有 + 自己的公开」"
        );
        assert_eq!(body["total"], 2);

        // bob 视角：自己的私有 + alice 的公开，看不到 alice 的私有
        let (status, body) = call(&state, api("GET", "/api/playlists", Some(&bob), None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed_ids(&body), vec![alice_public, bob_private]);
        assert!(!listed_ids(&body).contains(&alice_private));
        assert_eq!(body["total"], 2);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 9. 我加的：跨用户隔离
    // ─────────────────────────────────────────────────────────────────────────

    /// 我加的 UT：A 的歌单，B 的 5 种写操作全被拒 ——
    /// 私有 → 404（连存在都不知道），公开 → 403（存在不是秘密，缺的只是写权限）。
    #[tokio::test]
    async fn cross_user_writes_are_rejected_with_404_or_403_by_visibility() {
        let (state, _temp) = test_state("s22-cross-user");
        let alice = issue_token(&state, "alice");
        let bob = issue_token(&state, "bob");

        let private_id = create_playlist(&state, &alice, "alice 私有", false).await;
        let public_id = create_playlist(&state, &alice, "alice 公开", true).await;
        let song_id = seed_song(&state, "/music/some.mp3");
        // owner 先在两个歌单里各放一首，这样移出 / 重排有真实数据可操作
        for playlist_id in [private_id, public_id] {
            let (status, body) = call(
                &state,
                api(
                    "POST",
                    &format!("/api/playlists/{playlist_id}/items"),
                    Some(&alice),
                    Some(json!({ "song_id": song_id })),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }

        // 私有：读不到 → 所有写操作 404
        let private_writes: [(&str, String, Option<Value>); 5] = [
            (
                "PUT",
                format!("/api/playlists/{private_id}"),
                Some(json!({ "name": "bob 改" })),
            ),
            ("DELETE", format!("/api/playlists/{private_id}"), None),
            (
                "POST",
                format!("/api/playlists/{private_id}/items"),
                Some(json!({ "song_id": song_id })),
            ),
            (
                "DELETE",
                format!("/api/playlists/{private_id}/items/{song_id}"),
                None,
            ),
            (
                "PUT",
                format!("/api/playlists/{private_id}/items"),
                Some(json!({ "song_ids": [song_id] })),
            ),
        ];
        for (method, uri, body) in private_writes {
            let (status, response) = call(&state, api(method, &uri, Some(&bob), body)).await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{method} {uri} 对别人的私有歌单必须 404：{response}"
            );
        }

        // 公开：看得见但改不了 → 所有写操作 403
        let public_writes: [(&str, String, Option<Value>); 5] = [
            (
                "PUT",
                format!("/api/playlists/{public_id}"),
                Some(json!({ "name": "bob 改" })),
            ),
            ("DELETE", format!("/api/playlists/{public_id}"), None),
            (
                "POST",
                format!("/api/playlists/{public_id}/items"),
                Some(json!({ "song_id": song_id })),
            ),
            (
                "DELETE",
                format!("/api/playlists/{public_id}/items/{song_id}"),
                None,
            ),
            (
                "PUT",
                format!("/api/playlists/{public_id}/items"),
                Some(json!({ "song_ids": [song_id] })),
            ),
        ];
        for (method, uri, body) in public_writes {
            let (status, response) = call(&state, api(method, &uri, Some(&bob), body)).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{method} {uri} 对别人的公开歌单必须 403：{response}"
            );
        }

        // 被拒之后数据没被动过
        let (status, _) = call(
            &state,
            api(
                "GET",
                &format!("/api/playlists/{private_id}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "别人的写操作不该删掉歌单");
        let (status, body) = call(
            &state,
            api(
                "GET",
                &format!("/api/playlists/{public_id}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["songs"].as_array().expect("songs 数组").len(),
            1,
            "别人的写操作不该移出 / 破坏曲目"
        );
        assert_eq!(stored_items(&state, private_id), vec![(song_id, 1)]);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 10. 我加的：详情形状 / 部分更新 / 参数校验
    // ─────────────────────────────────────────────────────────────────────────

    /// 我加的 UT：详情曲目用与 /api/songs/{id} 一致的精简形状（无内部字段），
    /// 顺序即 position 顺序；部分更新只动提到的字段；非法参数 400。
    #[tokio::test]
    async fn detail_returns_tracks_in_position_order_without_internal_fields() {
        let (state, _temp) = test_state("s22-detail");
        let alice = issue_token(&state, "alice");
        let playlist_id = create_playlist(&state, &alice, "详情", false).await;
        let first = seed_song(&state, "/srv/music/secret-a.mp3");
        let second = seed_song(&state, "/srv/music/secret-b.mp3");
        for song_id in [first, second] {
            let (status, body) = call(
                &state,
                api(
                    "POST",
                    &format!("/api/playlists/{playlist_id}/items"),
                    Some(&alice),
                    Some(json!({ "song_id": song_id })),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }

        let (status, body) = call(
            &state,
            api(
                "GET",
                &format!("/api/playlists/{playlist_id}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["playlist"]["name"], "详情");
        let tracks = body["songs"].as_array().expect("songs 是数组");
        assert_eq!(tracks.len(), 2);
        assert_eq!(
            tracks
                .iter()
                .map(|track| track["id"].as_i64().expect("id"))
                .collect::<Vec<_>>(),
            vec![first, second],
            "曲目顺序就是 position 顺序"
        );
        for track in tracks {
            for leaked in [
                "file_path",
                "scrape_error",
                "audio_hash",
                "search_text",
                "file_mtime",
                "deleted_at",
                "lyrics",
            ] {
                assert!(
                    track.get(leaked).is_none(),
                    "详情曲目泄漏了内部字段 {leaked}：{track}"
                );
            }
        }
        assert!(
            !body.to_string().contains("secret-a"),
            "响应体不能出现服务器绝对路径"
        );

        // 改名（要 trim）/ 改描述
        let (status, body) = call(
            &state,
            api(
                "PUT",
                &format!("/api/playlists/{playlist_id}"),
                Some(&alice),
                Some(json!({ "name": "  新名字  ", "description": "一段描述" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["playlist"]["name"], "新名字", "名字要 trim");
        assert_eq!(body["playlist"]["description"], "一段描述");

        // 显式 null 清空描述，且不碰没提到的 name
        let (status, body) = call(
            &state,
            api(
                "PUT",
                &format!("/api/playlists/{playlist_id}"),
                Some(&alice),
                Some(json!({ "description": null })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["playlist"]["description"].is_null(), "显式 null 应清空描述");
        assert_eq!(body["playlist"]["name"], "新名字", "没提到的字段不该被清掉");

        // 空名字 / 非法类型 / 非对象 body → 400
        for bad in [
            json!({ "name": "   " }),
            json!({ "name": 42 }),
            json!({ "is_public": "yes" }),
            json!([]),
        ] {
            let (status, response) = call(
                &state,
                api(
                    "PUT",
                    &format!("/api/playlists/{playlist_id}"),
                    Some(&alice),
                    Some(bad.clone()),
                ),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "body {bad} 必须 400：{response}"
            );
        }

        // 新建歌单缺 name / 超长 name → 400
        for bad in [json!({}), json!({ "name": "x".repeat(MAX_NAME_CHARS + 1) })] {
            let (status, _) = call(
                &state,
                api("POST", "/api/playlists", Some(&alice), Some(bad)),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }

        // 移出一首歌：剩下的 position 重新连续，重复移出是 200 + removed:false
        let (status, body) = call(
            &state,
            api(
                "DELETE",
                &format!("/api/playlists/{playlist_id}/items/{first}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["removed"], true);
        assert_eq!(stored_items(&state, playlist_id), vec![(second, 1)]);

        let (status, body) = call(
            &state,
            api(
                "DELETE",
                &format!("/api/playlists/{playlist_id}/items/{first}"),
                Some(&alice),
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "重复移出也应幂等 200：{body}");
        assert_eq!(body["removed"], false);
    }
}
