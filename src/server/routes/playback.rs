//! S21 · 播放周边 API：history / favorites / settings。
//!
//! 路径（取自画布 API 清单）：
//!
//! * POST   /api/history                        记录一次播放
//! * GET    /api/history?limit=&offset=         最近播放（分页）
//! * GET    /api/favorites                      我的收藏
//! * POST   /api/favorites/{song_id}            收藏（幂等：首次 201 / 重复 200）
//! * DELETE /api/favorites/{song_id}            取消收藏（幂等：重复 200 + removed:false）
//! * GET    /api/settings                       我的设置（键值）
//! * PUT    /api/settings                       写设置（批量；null 值 = 删除该键）
//!
//! 7 条路由全部挂在 routes::build_router 的**受保护子 Router** 上（require_auth
//! 中间件），此外每个 handler 都显式提取 AuthUser —— 双重保险：将来有人把路由
//! 顺手挪进公开 Router 时仍会 401，而不是悄悄匿名可用。
//!
//! 响应体一律手写 serde_json::Value（项目规范：不引 serde derive）。
//!
//! # 用户隔离（本步骤的第一要求）
//!
//! 这三组数据**只属于调用者**，没有任何「看别人」的口子：
//!
//! * user_id 一律取自 AuthUser（令牌验签后按 sub 查库得到），**绝不从请求体 /
//!   查询串里读 user_id** —— 那是越权漏洞：请求体里塞别人的 id 就能读写别人的
//!   历史 / 收藏 / 设置。
//! * 每条 SQL 都由 repo 层带上 `WHERE user_id = ?`（history::recent /
//!   count_by_user、favorites::list_by_user / is_favorited、settings::list /
//!   count / get 都是）。本模块**不写裸 SQL**，避免哪天漏掉这个条件。
//! * 同一首歌，A 收藏不影响 B；A 的历史不出现在 B 的分页里；同名设置键各存各的。
//!   跨用户隔离有专门的 UT（A 写一份、B 全看不到），见文件末尾。
//!
//! # 断点续播的键约定（前端照这个来，否则对不上）
//!
//! 画布：position_ms 存 user_settings，不另开路由。本 API 把它固化成一条约定：
//!
//! * **键 = `resume:{song_id}`**（例如 `resume:42`），前缀见 [RESUME_KEY_PREFIX]；
//! * **值 = 毫秒的十进制字符串**（例如 `"12345"`，没有单位后缀、没有小数点）。
//!
//! 读写都走通用设置接口：
//!
//! ```text
//! PUT /api/settings  {"resume:42": "12345"}   ← 记录播放到 12.345 秒
//! GET /api/settings  → { "settings": { "resume:42": "12345" } }
//! ```
//!
//! 一首歌播完 / 用户从头播放时把键删掉即可（PUT `{"resume:42": null}`）。
//! 为什么不做成强类型路由：画布明确「不需要为它单开路由」，而 settings 本来就是
//! 通用键值表；把键名与值格式写死成约定，前后端就不会各理解一套。
//!
//! # 收藏幂等（画布 UT 明确要求）
//!
//! * 首次收藏 → **201** + `{ favorited: true, created: true }`；
//! * 重复收藏 → **200** + `{ favorited: true, created: false }`，**不产生第二行**
//!   （favorites 有 UNIQUE(user_id, song_id)，repo 的 add 重复时返回 Conflict；
//!   这里先查 is_favorited，并把并发下的 Conflict 也当「已收藏」）。理由与 S22
//!   加歌一致：这是**幂等成功**不是冲突，弱网重试 / 双击重发不该被当成错误。
//! * 重复取消 → **200** + `removed: false`（DELETE 语义天然幂等）。
//! * song_id 不存在 / 已软删 → **404**，绝不交给外键去报错（那会变成 500）。
//!   POST 与 DELETE 同一口径：路径指向的歌不存在时，这个资源本来就不存在。
//!
//! # 设置的滥用防护（唯一「用户能塞任意数据」的入口）
//!
//! 不设限的键值写入等于给每个登录用户一块无上限的存储，因此定死三条：
//!
//! * 单键 ≤ [MAX_SETTING_KEY_CHARS] 字符，且不含空白 / 控制字符（键是标识符，不是
//!   正文；含空白的键几乎一定是前端 bug）；
//! * 单值 ≤ [MAX_SETTING_VALUE_CHARS] 字符；
//! * 每用户键数 ≤ [MAX_SETTINGS_PER_USER]，单次批量 ≤ [MAX_SETTINGS_BATCH_KEYS] 个键。
//!
//! 任一条超限 → **400**（中文说明），**不静默截断**：悄悄截断会让前端以为写进去了。
//! 值只接受字符串或 null：**null 表示删除该键**（不是写一个空值），删除一个不存在的
//! 键也是 200（幂等）。数字 / 布尔 / 对象一律 400 —— 值是 TEXT 列，若在这里做类型
//! 推断（30 是 "30" 还是 30？），前后端迟早对不上。
//!
//! # 软删的歌
//!
//! 歌曲是软删（deleted_at），历史 / 收藏行被外键保留，本模块的口径：
//!
//! * **历史**：条目照旧返回（历史是既成事实，不该因为文件没了就消失），内联的
//!   `song` 置 null，前端跳过即可；`total` 与分页口径仍然精确 —— 这一点很重要，
//!   若在这里过滤行，页大小与 total 都会和 repo 的 count 对不上。
//! * **收藏列表**：不可播放的歌直接**跳过**（与 S22 歌单详情同口径），列表里只留
//!   能播的；`total` = 实际返回条数。
//! * **写入**：给软删的歌记历史 / 收藏一律 404（songs::get 默认不含软删行）。
//!
//! # 阻塞调用
//!
//! rusqlite 全是同步阻塞调用，统一用 library::run_db 包进 spawn_blocking
//! （S14 铁律），绝不在 async 上下文里直接碰连接池。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

use crate::db::models::{PlayHistory, UserSetting};
use crate::db::repos::RepoError;
use crate::db::repos::{favorites, history, settings, songs};
use crate::server::auth::AuthUser;
use crate::server::error::{ApiError, ApiResult};
use crate::server::state::AppState;

use super::library::{parse_id, run_db, song_json, QueryParams};

// ─────────────────────────────────────────────────────────────────────────────
// 常量与约定
// ─────────────────────────────────────────────────────────────────────────────

/// 断点续播在 user_settings 里的键前缀，完整键是 `resume:{song_id}`。
///
/// 值是毫秒的十进制字符串（见模块头注释）。这里导出成常量，是为了让键名只有
/// 一处事实来源 —— 前端文档、测试、以后的服务端逻辑都引用它。
pub const RESUME_KEY_PREFIX: &str = "resume:";

/// 最近播放默认返回条数。
const DEFAULT_HISTORY_LIMIT: i64 = 50;

/// 最近播放单页条数上限，超过一律 400（不静默夹取）。
///
/// 与 S16 的 MAX_PAGE_SIZE 同口径：没有上限时 `?limit=999999999` 就是一次
/// 内存放大攻击；静默夹取又会让前端以为拿到了想要的大小。
const MAX_HISTORY_LIMIT: i64 = 200;

/// 播放统计按天聚合的默认 / 最大天数。
///
/// 上限 366（一年）而不是随意大：一天一行，366 行在界面上已经画不下，
/// 再大只是白算。
const DEFAULT_STATS_DAYS: i64 = 30;
const MAX_STATS_DAYS: i64 = 366;

/// 播放统计里歌 / 歌手榜的默认与最大条数。
const DEFAULT_STATS_TOP: i64 = 10;
const MAX_STATS_TOP: i64 = 50;

/// 时区偏移的允许范围（分钟）：现实世界是 UTC-12:00 ~ UTC+14:00。
///
/// 不设这个范围的话，`?tz_offset_minutes=99999999` 会让 SQLite 的 date()
/// 修饰符把人带到一个毫无意义的日期上（响应里那个 `day` 就成了假数据）。
const MAX_TZ_OFFSET_MINUTES: i64 = 14 * 60;
const MIN_TZ_OFFSET_MINUTES: i64 = -12 * 60;

/// 单个设置键的最大字符数。
///
/// 键是标识符（`volume` / `resume:42`），128 个字符已经绰绰有余；
/// NOT NULL 挡不住超长文本，必须在这里挡，否则一个 key 就能塞进整本书。
const MAX_SETTING_KEY_CHARS: usize = 128;

/// 单个设置值的最大字符数。
///
/// 值都是小 JSON / 数字串（续播位置、音量、播放模式），4 KiB 足够宽裕；再大就该
/// 走正经的接口，而不是拿设置表当文件柜。
const MAX_SETTING_VALUE_CHARS: usize = 4096;

/// 每个用户的设置键数量上限。
///
/// 200 个键覆盖「续播位置（每首歌一个）+ 偏好项」的常见规模；没有上限的话，
/// 一个循环就能把设置表撑成用户专属的无限存储。
const MAX_SETTINGS_PER_USER: i64 = 200;

/// 单次 PUT /api/settings 允许的键数量上限。
///
/// 一次批量写入不该超过一个用户的总配额：既挡住「一个请求塞十万个键」的放大攻击，
/// 也不让合法客户端写出必然超配额的请求（提前失败，语义更清楚）。
const MAX_SETTINGS_BATCH_KEYS: usize = 200;

// ─────────────────────────────────────────────────────────────────────────────
// 通用校验
// ─────────────────────────────────────────────────────────────────────────────

/// 请求体必须是 JSON 对象（否则字段全取不到，报错会含糊）。
fn require_object(body: &Value) -> Result<(), ApiError> {
    if body.is_object() {
        Ok(())
    } else {
        Err(ApiError::bad_request("请求体必须是 JSON 对象"))
    }
}

/// 解析最近播放的 limit：缺省 [DEFAULT_HISTORY_LIMIT]，必须 >= 1 且 <= 上限。
///
/// 空串视同缺省（与 S16 的分页参数一致）；其余非法输入一律 400。
fn parse_history_limit(raw: Option<&str>) -> Result<i64, ApiError> {
    let Some(text) = raw else {
        return Ok(DEFAULT_HISTORY_LIMIT);
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_HISTORY_LIMIT);
    }
    let value: i64 = trimmed
        .parse()
        .map_err(|_| ApiError::bad_request("分页参数 limit 必须是整数"))?;
    if value < 1 {
        return Err(ApiError::bad_request("分页参数 limit 必须大于等于 1"));
    }
    if value > MAX_HISTORY_LIMIT {
        return Err(ApiError::bad_request(format!(
            "分页参数 limit 不能超过 {MAX_HISTORY_LIMIT}"
        )));
    }
    Ok(value)
}

/// 解析最近播放的 offset：缺省 0，必须 >= 0；没有上限（越界页返回空数组）。
fn parse_history_offset(raw: Option<&str>) -> Result<i64, ApiError> {
    let Some(text) = raw else {
        return Ok(0);
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    let value: i64 = trimmed
        .parse()
        .map_err(|_| ApiError::bad_request("分页参数 offset 必须是整数"))?;
    if value < 0 {
        return Err(ApiError::bad_request("分页参数 offset 不能为负数"));
    }
    Ok(value)
}

/// 解析 `days`：缺省 [DEFAULT_STATS_DAYS]，允许 **0 = 全部时间**，上限见常量。
fn parse_stats_days(raw: Option<&str>) -> Result<i64, ApiError> {
    parse_bounded(raw, DEFAULT_STATS_DAYS, 0, MAX_STATS_DAYS, "days")
}

/// 解析 `top`：缺省 [DEFAULT_STATS_TOP]，必须 >= 1 且 <= 上限。
fn parse_stats_top(raw: Option<&str>) -> Result<i64, ApiError> {
    parse_bounded(raw, DEFAULT_STATS_TOP, 1, MAX_STATS_TOP, "top")
}

/// 解析 `tz_offset_minutes`：缺省 0，允许负值（西时区），范围见常量。
///
/// 这里是**唯一允许负数**的参数 —— 别的分页参数为负一律 400，
/// 所以没有复用 [parse_bounded]（它从 1 起）。符号在 SQLite 那边由 repo 拼。
fn parse_tz_offset(raw: Option<&str>) -> Result<i64, ApiError> {
    let Some(text) = raw else {
        return Ok(0);
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    let value: i64 = trimmed
        .parse()
        .map_err(|_| ApiError::bad_request("tz_offset_minutes 必须是整数（与 UTC 的分钟差）"))?;
    if !(MIN_TZ_OFFSET_MINUTES..=MAX_TZ_OFFSET_MINUTES).contains(&value) {
        return Err(ApiError::bad_request(format!(
            "tz_offset_minutes 必须在 {MIN_TZ_OFFSET_MINUTES} 到 {MAX_TZ_OFFSET_MINUTES} 之间（即 UTC-12:00 到 UTC+14:00）"
        )));
    }
    Ok(value)
}

/// 「整数 + 闭区间」的通用解析（空串视同缺省，非法 / 越界一律 400，**不静默夹取**）。
fn parse_bounded(
    raw: Option<&str>,
    default: i64,
    min: i64,
    max: i64,
    name: &str,
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
        .map_err(|_| ApiError::bad_request(format!("参数 {name} 必须是整数")))?;
    if value < min || value > max {
        return Err(ApiError::bad_request(format!(
            "参数 {name} 必须在 {min} 到 {max} 之间"
        )));
    }
    Ok(value)
}

/// 校验设置键：非空、不超长、不含空白或控制字符。
fn checked_key(key: &str) -> Result<(), ApiError> {
    if key.is_empty() {
        return Err(ApiError::bad_request("设置键不能为空"));
    }
    let chars = key.chars().count();
    if chars > MAX_SETTING_KEY_CHARS {
        return Err(ApiError::bad_request(format!(
            "设置键不能超过 {MAX_SETTING_KEY_CHARS} 个字符（当前 {chars} 个）"
        )));
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::bad_request(
            "设置键不能包含空白或控制字符（键是标识符，例如 volume、resume:42）",
        ));
    }
    Ok(())
}

/// 把请求体解析成「键 -> 新值」的写入计划：Some = 写入该值，None = 删除该键。
///
/// 先整批校验再落库（调用方保证）：任何一项不合法都 400，不会出现写了一半才报错。
fn parse_settings_changes(body: &Value) -> Result<BTreeMap<String, Option<String>>, ApiError> {
    let object = body
        .as_object()
        .ok_or_else(|| ApiError::bad_request("请求体必须是 JSON 对象"))?;
    if object.len() > MAX_SETTINGS_BATCH_KEYS {
        return Err(ApiError::bad_request(format!(
            "单次最多写入 {MAX_SETTINGS_BATCH_KEYS} 个设置项（本次 {} 个）",
            object.len()
        )));
    }

    let mut changes = BTreeMap::new();
    for (key, value) in object {
        checked_key(key)?;
        let parsed = match value {
            // null = 删除该键（与「写一个空值」不同，见模块头注释）。
            Value::Null => None,
            Value::String(text) => {
                let chars = text.chars().count();
                if chars > MAX_SETTING_VALUE_CHARS {
                    return Err(ApiError::bad_request(format!(
                        "设置项 {key} 的值不能超过 {MAX_SETTING_VALUE_CHARS} 个字符（当前 {chars} 个）"
                    )));
                }
                Some(text.clone())
            }
            _ => {
                return Err(ApiError::bad_request(format!(
                    "设置项 {key} 的值必须是字符串或 null（null 表示删除该键）"
                )))
            }
        };
        let _ = changes.insert(key.clone(), parsed);
    }
    Ok(changes)
}

/// 把写入计划落到库上，并在写入前检查「每用户键数量上限」。
///
/// 计数口径：现有键数 + 本批次会新建的键数 - 本批次会删掉的现有键数。
/// 这样「删掉一个再写一个」不会被误判成超限，而「已经到顶还要新建」会被拦下。
///
/// 说明：检查与写入在同一个阻塞闭包里、同一条连接上执行，但连接池可能让两个并发
/// PUT 交错，理论上可以超出上限一个批次。这是防滥用的软上限（不是安全边界），
/// 为此上事务不值得 —— 真正的威胁是无限增长，而它已经被挡住了。
fn apply_settings(
    conn: &Connection,
    user_id: i64,
    changes: &BTreeMap<String, Option<String>>,
) -> Result<(), ApiError> {
    if changes.is_empty() {
        return Ok(());
    }

    let current = settings::count(conn, user_id)?;
    let existing: HashSet<String> = settings::list(conn, user_id)?
        .into_iter()
        .map(|row| row.key)
        .collect();

    let mut final_count = current;
    for (key, value) in changes {
        match value {
            Some(_) if !existing.contains(key) => final_count += 1,
            None if existing.contains(key) => final_count -= 1,
            _ => {}
        }
    }
    if final_count > MAX_SETTINGS_PER_USER {
        return Err(ApiError::bad_request(format!(
            "每个用户最多保存 {MAX_SETTINGS_PER_USER} 个设置项（本次写入后会变成 {final_count} 个），请先删除一些再写"
        )));
    }

    for (key, value) in changes {
        match value {
            Some(text) => settings::set(conn, user_id, key, Some(text.as_str()))?,
            None => {
                settings::delete(conn, user_id, key)?;
            }
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// 对外 JSON
// ─────────────────────────────────────────────────────────────────────────────

/// 播放历史一行的对外 JSON（**不含 user_id**：那由令牌决定，不必回传）。
fn history_json(row: &PlayHistory) -> Value {
    json!({
        "id": row.id,
        "song_id": row.song_id,
        "played_at": row.played_at,
        "duration_listened_ms": row.duration_listened_ms,
    })
}

/// 设置行的对外形状：JSON 对象（键 -> 值）。
///
/// 值为 NULL 的行只有绕过本接口（PUT 的 null 是删除）才可能出现，这里如实回 null，
/// 不假装它不存在 —— 客户端能看见真实状态，也好自己决定要不要清理。
fn settings_map(rows: &[UserSetting]) -> Map<String, Value> {
    let mut map = Map::new();
    for row in rows {
        let value = match &row.value {
            Some(text) => Value::String(text.clone()),
            None => Value::Null,
        };
        let _ = map.insert(row.key.clone(), value);
    }
    map
}

// ─────────────────────────────────────────────────────────────────────────────
// history handlers
// ─────────────────────────────────────────────────────────────────────────────

/// POST /api/history —— 记录一次播放。
///
/// 请求体：`{ "song_id": 3, "duration_listened_ms": 30000 }`（时长可缺席 / null）。
/// 成功 **201** + `{ history: { id, song_id, played_at, duration_listened_ms } }`。
///
/// * song_id 不存在 / 已软删 → **404**（先查歌再写，不让外键报错变成 500）；
/// * duration_listened_ms 为负 → **400**（负数时长没有意义，多半是前端算错了）；
/// * played_at 由 repo 盖当前时间，请求体里给不给都无效。
pub async fn record_history(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    require_object(&body)?;
    let song_id = body
        .get("song_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::bad_request("缺少整数 song_id"))?;
    let duration_ms = match body.get("duration_listened_ms") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let ms = value
                .as_i64()
                .ok_or_else(|| ApiError::bad_request("duration_listened_ms 必须是整数"))?;
            if ms < 0 {
                return Err(ApiError::bad_request("duration_listened_ms 不能为负数"));
            }
            Some(ms)
        }
    };
    // 用户身份只来自令牌，绝不看请求体里的任何 user_id 字段。
    let user_id = auth.0.id;

    let recorded = run_db(Arc::clone(&state.db), move |conn| {
        if songs::get(conn, song_id, false)?.is_none() {
            return Err(ApiError::not_found("请求的歌曲不存在"));
        }
        let id = history::record(conn, user_id, song_id, duration_ms)?;
        history::get(conn, id)?
            .ok_or_else(|| ApiError::internal("播放历史刚写入就查不到了，怀疑有并发删除"))
    })
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "history": history_json(&recorded) })),
    ))
}

/// GET /api/history?limit=&offset= —— 最近播放分页，最近的在前。
///
/// 响应：`{ items, limit, offset, total }`；每个 item 是历史行 + 内联的 `song`
/// （歌被软删时 song 为 null，见模块头注释）。total 是该用户的**全量**条数，
/// 不随分页变化。
///
/// 参数口径与 S16 完全一致：非法 / 超上限一律 400，越界 offset 是 200 + 空数组。
pub async fn recent_history(
    auth: AuthUser,
    State(state): State<AppState>,
    params: QueryParams,
) -> ApiResult<Json<Value>> {
    let limit = parse_history_limit(params.get("limit"))?;
    let offset = parse_history_offset(params.get("offset"))?;
    let user_id = auth.0.id;

    let (items, total) = run_db(Arc::clone(&state.db), move |conn| {
        let rows = history::recent(conn, user_id, limit, offset)?;
        let total = history::count_by_user(conn, user_id)?;

        // 一次把这批历史涉及的曲目全取回来（原来是逐行 songs::get —— N+1），
        // 并且走 library::songs_json，这样**专辑名也一起补上**，与列表接口口径一致。
        let ids: Vec<i64> = rows.iter().map(|row| row.song_id).collect();
        let found = songs::get_many(conn, &ids)?;
        let values = super::library::songs_json(conn, &found, false);
        let mut by_id: HashMap<i64, Value> = HashMap::with_capacity(found.len());
        for (song, value) in found.iter().zip(values) {
            by_id.insert(song.id, value);
        }

        let items: Vec<Value> = rows
            .iter()
            .map(|row| {
                let mut item = history_json(row);
                if let Some(object) = item.as_object_mut() {
                    // 曲目可能已被软删：那时给 null，而不是把这条历史抹掉
                    let song = by_id.get(&row.song_id).cloned().unwrap_or(Value::Null);
                    let _ = object.insert("song".to_string(), song);
                }
                item
            })
            .collect();
        Ok((items, total))
    })
    .await?;

    Ok(Json(json!({
        "items": items,
        "limit": limit,
        "offset": offset,
        "total": total,
    })))
}

/// GET /api/history/stats —— 播放统计（**只统计调用者自己的行**）。
///
/// 查询参数：
/// * `days`——统计范围：最近多少天（默认 [DEFAULT_STATS_DAYS]，上限 [MAX_STATS_DAYS]，
///   **0 = 全部时间**）。**三个数字一起受它影响**（总量 / 榜单 / 按天），
///   否则界面上「总量」与柱状图之和会对不上。
/// * `tz_offset_minutes`——与 UTC 的偏移分钟数（北京 = 480），**决定「一天」从哪儿切**。
///   前端传 `-new Date().getTimezoneOffset()` 即可（那个 API 的符号与这里相反）。
///   省略按 0（UTC）算 —— 但那样凌晨听的东西会算到前一天，所以前端**应该传**。
/// * `top`——歌 / 歌手榜各取前几条（默认 [DEFAULT_STATS_TOP]，上限 [MAX_STATS_TOP]）。
///
/// 响应：
/// ```json
/// {
///   "totals": { "plays": 12, "listened_ms": 345000, "songs": 5 },
///   "top_songs": [ { "song": {...}, "plays": 3, "listened_ms": 90000 } ],
///   "top_artists": [ { "name": "周杰伦", "plays": 8, "listened_ms": 200000 } ],
///   "daily": [ { "day": "2026-10-02", "plays": 4, "listened_ms": 120000 } ],
///   "days": 30,
///   "tz_offset_minutes": 480
/// }
/// ```
///
/// ⚠️ **`totals.listened_ms` 可能偏小**：`duration_listened_ms` 是 2026-10-02 起
/// 前端才上报的，更早的历史行是 NULL（SUM 当 0 算，不丢行）。所以「播放次数」准、
/// 「累计时长」是「已知的那部分」。别把它当精确值展示成「共听了 X 小时」而不留余地。
///
/// ⚠️ **`totals.plays` ≥ 各榜单之和**：榜单跳过已软删的歌（放不进榜也没法播），
/// 而次数照算 —— 两者本来就不相等。
pub async fn history_stats(
    auth: AuthUser,
    State(state): State<AppState>,
    params: QueryParams,
) -> ApiResult<Json<Value>> {
    let days = parse_stats_days(params.get("days"))?;
    let tz_offset_minutes = parse_tz_offset(params.get("tz_offset_minutes"))?;
    let top = parse_stats_top(params.get("top"))?;
    let user_id = auth.0.id;
    // 范围起点。`days = 0` = **全部时间**（不设下限）—— 界面上「全部」那一档就是它。
    // 非零时往前多算一天，是为了让「今天」那一格必然是完整的：客户端按自己的
    // 时区切天，这里给宽一点比给窄了好（给窄了会少一格）。
    let since_ms = if days == 0 {
        0
    } else {
        (crate::db::now_unix_ms() - (days + 1) * 86_400_000).max(0)
    };

    let (totals, top_songs, top_artists, daily) = run_db(Arc::clone(&state.db), move |conn| {
        let totals = history::totals(conn, user_id, since_ms)?;
        let top_songs = history::top_songs(conn, user_id, since_ms, top)?;
        let top_artists = history::top_artists(conn, user_id, since_ms, top)?;
        let daily = history::daily(conn, user_id, since_ms, tz_offset_minutes)?;
        Ok((totals, top_songs, top_artists, daily))
    })
    .await?;

    // 榜单里的歌一次性取回来（与 recent_history 同一套：批量 + songs_json 补专辑名）
    let ids: Vec<i64> = top_songs.iter().map(|row| row.song_id).collect();
    let songs_with_album = run_db(Arc::clone(&state.db), move |conn| {
        let found = songs::get_many(conn, &ids)?;
        let values = super::library::songs_json(conn, &found, false);
        Ok(found
            .iter()
            .map(|song| song.id)
            .zip(values)
            .collect::<HashMap<i64, Value>>())
    })
    .await?;

    let song_rows: Vec<Value> = top_songs
        .iter()
        .map(|row| {
            json!({
                // 同上：榜单里的歌可能刚被软删（聚合与取曲目之间），那时给 null
                "song": songs_with_album.get(&row.song_id).cloned().unwrap_or(Value::Null),
                "plays": row.plays,
                "listened_ms": row.listened_ms,
            })
        })
        .collect();

    let artist_rows: Vec<Value> = top_artists
        .iter()
        .map(|row| {
            json!({
                "name": row.name,
                "plays": row.plays,
                "listened_ms": row.listened_ms,
            })
        })
        .collect();

    let daily_rows: Vec<Value> = daily
        .iter()
        .map(|row| {
            json!({
                "day": row.day,
                "plays": row.plays,
                "listened_ms": row.listened_ms,
            })
        })
        .collect();

    Ok(Json(json!({
        "totals": {
            "plays": totals.plays,
            "listened_ms": totals.listened_ms,
            "songs": totals.songs,
        },
        "top_songs": song_rows,
        "top_artists": artist_rows,
        "daily": daily_rows,
        "days": days,
        "tz_offset_minutes": tz_offset_minutes,
    })))
}

// ─────────────────────────────────────────────────────────────────────────────
// favorites handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/favorites —— 我的收藏（最近收藏的在前）。
///
/// 不分页：画布没给分页参数，收藏是用户自己攒的、量级小（与 S22 的歌单列表同口径）。
/// item 用与 /api/songs/{id} **同一个** song_json 产出（传 false），因此不会漏出
/// file_path 等内部列。软删的歌跳过（见模块头注释）。
pub async fn list_favorites(
    auth: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Value>> {
    let user_id = auth.0.id;

    let items = run_db(Arc::clone(&state.db), move |conn| {
        let rows = favorites::list_by_user(conn, user_id, i64::MAX, 0)?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            // 已不可播放的歌不再返回：收藏行还在（软删不级联），但列表里只留能播的。
            if let Some(song) = songs::get(conn, row.song_id, false)? {
                items.push(song_json(&song, false));
            }
        }
        Ok(items)
    })
    .await?;

    let total = items.len();
    Ok(Json(json!({ "items": items, "total": total })))
}

/// POST /api/favorites/{song_id} —— 收藏（幂等）。
///
/// * 首次 → **201** + `{ song_id, favorited: true, created: true }`；
/// * 已收藏 → **200** + `{ song_id, favorited: true, created: false, message }`；
/// * 歌不存在 / 已软删 → **404**（不让外键失败变成 500）。
pub async fn add_favorite(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_song_id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let song_id = parse_id(&raw_song_id)?;
    let user_id = auth.0.id;

    let created = run_db(Arc::clone(&state.db), move |conn| {
        if songs::get(conn, song_id, false)?.is_none() {
            return Err(ApiError::not_found("请求的歌曲不存在"));
        }
        if favorites::is_favorited(conn, user_id, song_id)? {
            return Ok(false);
        }
        match favorites::add(conn, user_id, song_id) {
            Ok(_) => Ok(true),
            // 并发下两个请求都通过了 is_favorited 检查，后到的撞 UNIQUE —— 结果等价于「已收藏」。
            Err(RepoError::Conflict { .. }) => Ok(false),
            Err(other) => Err(other.into()),
        }
    })
    .await?;

    if created {
        Ok((
            StatusCode::CREATED,
            Json(json!({ "song_id": song_id, "favorited": true, "created": true })),
        ))
    } else {
        Ok((
            StatusCode::OK,
            Json(json!({
                "song_id": song_id,
                "favorited": true,
                "created": false,
                "message": "这首歌已经在收藏里了",
            })),
        ))
    }
}

/// DELETE /api/favorites/{song_id} —— 取消收藏（幂等）。
///
/// * 取消成功 → **200** + `{ song_id, favorited: false, removed: true }`；
/// * 本来就没收藏 / 重复取消 → **200** + `removed: false`；
/// * 歌不存在 / 已软删 → **404**（与 POST 同口径，见模块头注释）。
pub async fn remove_favorite(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_song_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let song_id = parse_id(&raw_song_id)?;
    let user_id = auth.0.id;

    let removed = run_db(Arc::clone(&state.db), move |conn| {
        if songs::get(conn, song_id, false)?.is_none() {
            return Err(ApiError::not_found("请求的歌曲不存在"));
        }
        Ok(favorites::remove(conn, user_id, song_id)?)
    })
    .await?;

    Ok(Json(json!({
        "song_id": song_id,
        "favorited": false,
        "removed": removed > 0,
    })))
}

// ─────────────────────────────────────────────────────────────────────────────
// settings handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/settings —— 我的设置（键值对象）。
///
/// 响应：`{ settings: { 键: 值, ... }, total }`。按 key 升序（repo 的 ORDER BY key）。
/// 断点续播就存在这里，键约定见模块头注释。
pub async fn get_settings(
    auth: AuthUser,
    State(state): State<AppState>,
) -> ApiResult<Json<Value>> {
    let user_id = auth.0.id;
    let rows = run_db(Arc::clone(&state.db), move |conn| {
        Ok(settings::list(conn, user_id)?)
    })
    .await?;

    let map = settings_map(&rows);
    let total = map.len();
    Ok(Json(json!({ "settings": Value::Object(map), "total": total })))
}

/// PUT /api/settings —— 批量写设置。
///
/// 请求体：`{ "volume": "30", "resume:42": "12345", "play_mode": null }`。
/// **null 表示删除该键**；字符串是写入（同键覆盖，不新增行）。空对象 ``{}`` 是合法的
/// 空操作。成功 **200** + `{ settings（写后全量）, total, written, deleted }`。
///
/// 任一项超限 / 类型不对 → **400**，且**整批不落库**（先全部校验，再统一写）。
pub async fn put_settings(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    require_object(&body)?;
    let changes = parse_settings_changes(&body)?;
    let written: Vec<String> = changes
        .iter()
        .filter(|(_, value)| value.is_some())
        .map(|(key, _)| key.clone())
        .collect();
    let deleted: Vec<String> = changes
        .iter()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.clone())
        .collect();
    let user_id = auth.0.id;

    let rows = run_db(Arc::clone(&state.db), move |conn| {
        apply_settings(conn, user_id, &changes)?;
        Ok(settings::list(conn, user_id)?)
    })
    .await?;

    let map = settings_map(&rows);
    let total = map.len();
    Ok(Json(json!({
        "settings": Value::Object(map),
        "total": total,
        "written": written,
        "deleted": deleted,
    })))
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
    const SECRET: &str = "s21-secret";

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
        config.storage.library_roots = vec!["/tmp/server-s21-playback-music".to_string()];
        (AppState::new(Arc::new(pool), Arc::new(config)), temp)
    }

    /// 预置一个用户并签一个可用令牌（不走注册接口，省掉 argon2 的开销）。
    /// 返回 (user_id, token)：跨用户隔离的 UT 要拿 id 直接查库核对归属。
    fn issue_token(state: &AppState, username: &str) -> (i64, String) {
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
        (
            id,
            sign_token_with_ttl(SECRET, &user, 3600).expect("签发令牌"),
        )
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

    /// 把一首歌标记成软删（磁盘上文件不见了）。
    fn soft_delete(state: &AppState, song_id: i64) {
        let conn = state.db.acquire().expect("借连接");
        songs::mark_deleted(&conn, song_id).expect("标记删除");
    }

    /// 设一首歌的「歌手」——统计里的歌手榜按 `songs.artists` 整串分组，
    /// 而 [seed_song] 不写这个列，所以单独设一下。
    fn set_artists(state: &AppState, song_id: i64, artists: &str) {
        let conn = state.db.acquire().expect("借连接");
        conn.execute(
            "UPDATE songs SET artists = ?2 WHERE id = ?1",
            params![song_id, artists],
        )
        .expect("设歌手");
    }

    /// 直接查库：某用户的播放历史条数。
    fn count_history(state: &AppState, user_id: i64) -> i64 {
        let conn = state.db.acquire().expect("借连接");
        history::count_by_user(&conn, user_id).expect("数历史")
    }

    /// 直接查库：某用户的收藏条数。
    fn count_favorites(state: &AppState, user_id: i64) -> i64 {
        let conn = state.db.acquire().expect("借连接");
        favorites::count_by_user(&conn, user_id).expect("数收藏")
    }

    /// 直接查库：某用户的设置条数。
    fn count_settings(state: &AppState, user_id: i64) -> i64 {
        let conn = state.db.acquire().expect("借连接");
        settings::count(&conn, user_id).expect("数设置")
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

    /// 构造 PUT /api/settings 的请求体：Some(值) 写入，None 写 null（删除该键）。
    fn settings_body(entries: Vec<(&str, Option<&str>)>) -> Value {
        let mut map = Map::new();
        for (key, value) in entries {
            let value = match value {
                Some(text) => Value::String(text.to_string()),
                None => Value::Null,
            };
            let _ = map.insert(key.to_string(), value);
        }
        Value::Object(map)
    }

    /// 历史分页响应里的 song_id（顺序敏感）。
    fn history_song_ids(body: &Value) -> Vec<i64> {
        body["items"]
            .as_array()
            .expect("items 是数组")
            .iter()
            .map(|item| item["song_id"].as_i64().expect("song_id 是整数"))
            .collect()
    }

    /// 收藏响应里的歌曲 id（顺序敏感；items 用的是歌曲 JSON，id 就是歌曲 id）。
    fn favorite_song_ids(body: &Value) -> Vec<i64> {
        body["items"]
            .as_array()
            .expect("items 是数组")
            .iter()
            .map(|item| item["id"].as_i64().expect("收藏项应有歌曲 id"))
            .collect()
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 1. 记录播放
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「记录播放」：写进去的行字段正确、played_at 被盖章；
    /// 不存在的歌 / 软删的歌 → 404；负时长与缺 song_id → 400。
    #[tokio::test]
    async fn record_play_stamps_the_row_and_rejects_bad_requests() {
        let (state, _temp) = test_state("s21-record");
        let (user_id, token) = issue_token(&state, "alice");
        let song_id = seed_song(&state, "/music/a.mp3");

        let (status, body) = call(
            &state,
            api(
                "POST",
                "/api/history",
                Some(&token),
                Some(json!({ "song_id": song_id, "duration_listened_ms": 30_000 })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "记录播放应 201：{body}");
        assert_eq!(body["history"]["song_id"], song_id);
        assert_eq!(body["history"]["duration_listened_ms"], 30_000);
        let history_id = body["history"]["id"].as_i64().expect("history.id 是整数");
        // 响应绝不回传 user_id（内部字段）
        assert!(body["history"].get("user_id").is_none());

        // 查库断言：那一行确实落库，且 played_at 由 repo 盖了时间戳
        let row = {
            let conn = state.db.acquire().expect("借连接");
            history::get(&conn, history_id)
                .expect("查历史")
                .expect("行应在库里")
        };
        assert_eq!(row.user_id, user_id, "历史必须挂在当前用户上");
        assert_eq!(row.song_id, song_id);
        assert_eq!(row.duration_listened_ms, Some(30_000));
        assert!(row.played_at > 0, "played_at 应由 repo 盖章");

        // 时长可缺席
        let (status, body) = call(
            &state,
            api(
                "POST",
                "/api/history",
                Some(&token),
                Some(json!({ "song_id": song_id })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert!(body["history"]["duration_listened_ms"].is_null());

        // 不存在的歌 → 404，且不写历史
        let (status, body) = call(
            &state,
            api(
                "POST",
                "/api/history",
                Some(&token),
                Some(json!({ "song_id": 999_999 })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "不存在的歌必须 404：{body}");
        assert_eq!(body["error"]["code"], "NOT_FOUND");
        assert_eq!(count_history(&state, user_id), 2, "失败的请求不该写进历史");

        // 负时长 → 400
        let (status, body) = call(
            &state,
            api(
                "POST",
                "/api/history",
                Some(&token),
                Some(json!({ "song_id": song_id, "duration_listened_ms": -1 })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "负时长必须 400：{body}");
        assert_eq!(body["error"]["code"], "BAD_REQUEST");

        // 缺 song_id → 400
        let (status, _) = call(
            &state,
            api("POST", "/api/history", Some(&token), Some(json!({}))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 软删的歌 → 404
        soft_delete(&state, song_id);
        let (status, _) = call(
            &state,
            api(
                "POST",
                "/api/history",
                Some(&token),
                Some(json!({ "song_id": song_id })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "软删的歌必须 404");

        // 软删之后历史条目仍然在（历史是既成事实），但内联的 song 变成 null
        let (status, body) = call(&state, api("GET", "/api/history", Some(&token), None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], 2, "软删不该清掉历史");
        assert!(
            body["items"][0]["song"].is_null(),
            "不可播放的歌不再内联：{body}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 2. 最近播放分页
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「最近播放分页」：5 条历史，limit=2 拿最近的 2 条、offset 生效、
    /// total 正确；非法 / 超上限参数一律 400（不静默夹取）。
    #[tokio::test]
    async fn recent_history_paginates_newest_first_and_validates_params() {
        let (state, _temp) = test_state("s21-history-page");
        let (_user_id, token) = issue_token(&state, "alice");
        let songs: Vec<i64> = (0..5)
            .map(|index| seed_song(&state, &format!("/music/{index}.mp3")))
            .collect();
        for song_id in &songs {
            let (status, body) = call(
                &state,
                api(
                    "POST",
                    "/api/history",
                    Some(&token),
                    Some(json!({ "song_id": song_id, "duration_listened_ms": 1000 })),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "记播放失败：{body}");
        }

        // 第一页：最近的在最前。played_at 精度是毫秒、5 条可能同毫秒，
        // repo 用 ORDER BY played_at DESC, id DESC 兜底，顺序依旧稳定。
        let (status, body) = call(
            &state,
            api("GET", "/api/history?limit=2", Some(&token), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 5);
        assert_eq!(body["limit"], 2);
        assert_eq!(body["offset"], 0);
        assert_eq!(
            history_song_ids(&body),
            vec![songs[4], songs[3]],
            "最近播放的排最前"
        );

        // offset 生效：第二页是第 3、4 近的
        let (status, body) = call(
            &state,
            api("GET", "/api/history?limit=2&offset=2", Some(&token), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(history_song_ids(&body), vec![songs[2], songs[1]]);
        assert_eq!(body["total"], 5, "total 是全量条数，不随分页变化");

        // 越界 offset 是 200 + 空数组（不是 404，也不是错误）
        let (status, body) = call(
            &state,
            api("GET", "/api/history?limit=2&offset=99", Some(&token), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(history_song_ids(&body).is_empty());
        assert_eq!(body["total"], 5);

        // 缺省参数：不带 limit / offset 也能拿到全部 5 条
        let (status, body) = call(&state, api("GET", "/api/history", Some(&token), None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(history_song_ids(&body).len(), 5);
        assert_eq!(body["limit"], DEFAULT_HISTORY_LIMIT);

        // 非法参数 → 400（含超过上限）
        for uri in [
            "/api/history?limit=0",
            "/api/history?limit=-1",
            "/api/history?limit=201",
            "/api/history?limit=abc",
            "/api/history?offset=-1",
            "/api/history?offset=abc",
        ] {
            let (status, body) = call(&state, api("GET", uri, Some(&token), None)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} 必须 400：{body}");
            assert_eq!(body["error"]["code"], "BAD_REQUEST");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 2.5 播放统计
    // ─────────────────────────────────────────────────────────────────────────

    /// 播放统计：总量 / 歌榜 / 歌手榜 / 按天，并且**只看自己的行**。
    #[tokio::test]
    async fn history_stats_aggregates_totals_rankings_and_days() {
        let (state, _temp) = test_state("s21-stats");
        let (_alice_id, alice) = issue_token(&state, "alice");
        let (_bob_id, bob) = issue_token(&state, "bob");
        let a1 = seed_song(&state, "/music/a1.mp3");
        let a2 = seed_song(&state, "/music/a2.mp3");
        for song in [a1, a2] {
            set_artists(&state, song, "某歌手");
        }

        // alice：a1 三次（1s / 2s / 3s）、a2 一次（不带时长）
        for (song, dur) in [(a1, 1000), (a1, 2000), (a1, 3000), (a2, 0)] {
            let body = if dur > 0 {
                json!({ "song_id": song, "duration_listened_ms": dur })
            } else {
                json!({ "song_id": song })
            };
            let (status, resp) = call(
                &state,
                api("POST", "/api/history", Some(&alice), Some(body)),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{resp}");
        }
        // bob 的播放不该出现在 alice 的统计里
        let (status, _) = call(
            &state,
            api(
                "POST",
                "/api/history",
                Some(&bob),
                Some(json!({ "song_id": a2, "duration_listened_ms": 999_999 })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = call(
            &state,
            api("GET", "/api/history/stats", Some(&alice), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");

        assert_eq!(body["totals"]["plays"], 4, "alice 播了四次：{body}");
        assert_eq!(
            body["totals"]["listened_ms"], 6000,
            "缺时长的行当 0 累加，不是丢掉"
        );
        assert_eq!(body["totals"]["songs"], 2);
        assert_eq!(body["days"], DEFAULT_STATS_DAYS);
        assert_eq!(body["tz_offset_minutes"], 0, "不传就是 UTC 口径");

        let top = body["top_songs"].as_array().expect("top_songs 是数组");
        assert_eq!(top.len(), 2);
        assert_eq!(top[0]["song"]["id"], a1);
        assert_eq!(top[0]["plays"], 3);
        assert_eq!(top[0]["listened_ms"], 6000);
        assert_eq!(top[1]["song"]["id"], a2);
        assert_eq!(top[1]["plays"], 1);
        // 榜上的曲目形状必须与别处一致（不泄漏内部字段）
        assert!(
            top[0]["song"].get("file_path").is_none(),
            "泄漏了 file_path：{}",
            top[0]
        );

        let artists = body["top_artists"].as_array().expect("top_artists 是数组");
        assert_eq!(artists.len(), 1, "两首同歌手 → 合并：{body}");
        assert_eq!(artists[0]["name"], "某歌手");
        assert_eq!(artists[0]["plays"], 4);

        // 按天：今天（服务器时间）必然有 4 次
        let daily = body["daily"].as_array().expect("daily 是数组");
        assert_eq!(daily.len(), 1, "全部发生在今天：{body}");
        assert_eq!(daily[0]["plays"], 4);
        assert_eq!(daily[0]["listened_ms"], 6000);
        assert!(
            daily[0]["day"].as_str().is_some_and(|d| d.len() == 10),
            "day 必须是 YYYY-MM-DD：{}",
            daily[0]["day"]
        );
        // ⚠️ **不变量：按天之和 == 总量**（同一个 since 窗口、同一组行）。
        // 界面上「总量」与柱状图并排放着，两者对不上就是明显的 bug；
        // 这条把它钉住 —— 以后谁只给其中一边加过滤，这里立刻变红。
        let daily_sum: i64 = daily.iter().filter_map(|d| d["plays"].as_i64()).sum();
        assert_eq!(
            daily_sum, body["totals"]["plays"].as_i64().unwrap_or(-1),
            "按天之和必须等于总播放次数：{body}"
        );

        // bob 那边只有他自己的一次
        let (_, bob_body) = call(&state, api("GET", "/api/history/stats", Some(&bob), None)).await;
        assert_eq!(bob_body["totals"]["plays"], 1, "{bob_body}");
        assert_eq!(bob_body["totals"]["listened_ms"], 999_999);

        // 空历史：四个字段都在，数值为 0 / 空数组（不能缺键，前端少一处判空）
        let (_carol_id, carol) = issue_token(&state, "carol");
        let (status, empty) = call(
            &state,
            api("GET", "/api/history/stats", Some(&carol), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{empty}");
        assert_eq!(empty["totals"]["plays"], 0);
        assert_eq!(empty["totals"]["listened_ms"], 0);
        assert_eq!(empty["totals"]["songs"], 0);
        assert_eq!(empty["top_songs"].as_array().map(Vec::len), Some(0));
        assert_eq!(empty["top_artists"].as_array().map(Vec::len), Some(0));
        assert_eq!(empty["daily"].as_array().map(Vec::len), Some(0));
    }

    /// 参数校验：`days` / `top` / `tz_offset_minutes` 的边界，一律 400 不静默夹取。
    #[tokio::test]
    async fn history_stats_validates_its_params() {
        let (state, _temp) = test_state("s21-stats-params");
        let (_id, token) = issue_token(&state, "alice");

        // 合法：边界内的值都被接受，并**回显**在响应里（前端据此确认生效）
        for (uri, days, tz) in [
            ("/api/history/stats?days=1&tz_offset_minutes=480", 1, 480),
            ("/api/history/stats?days=7&tz_offset_minutes=-300", 7, -300),
            (
                "/api/history/stats?days=366&tz_offset_minutes=840",
                366,
                840,
            ),
            // days=0 = **全部时间**（界面上「全部」那一档），是允许的
            ("/api/history/stats?days=0", 0, 0),
        ] {
            let (status, body) = call(&state, api("GET", uri, Some(&token), None)).await;
            assert_eq!(status, StatusCode::OK, "{uri} 应通过：{body}");
            assert_eq!(body["days"], days, "{uri}");
            assert_eq!(body["tz_offset_minutes"], tz, "{uri}");
        }

        for uri in [
            "/api/history/stats?days=-1",
            "/api/history/stats?days=367",
            "/api/history/stats?days=abc",
            "/api/history/stats?top=0",
            "/api/history/stats?top=51",
            "/api/history/stats?top=abc",
            "/api/history/stats?tz_offset_minutes=841",
            "/api/history/stats?tz_offset_minutes=-721",
            "/api/history/stats?tz_offset_minutes=abc",
        ] {
            let (status, body) = call(&state, api("GET", uri, Some(&token), None)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} 必须 400：{body}");
            assert_eq!(body["error"]["code"], "BAD_REQUEST", "{uri}");
        }

        // 未登录 → 401（路由挂在受保护子 Router 上）
        let (status, _) = call(&state, api("GET", "/api/history/stats", None, None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // top 真的在限量
        let song = seed_song(&state, "/music/limited.mp3");
        for _ in 0..3 {
            let _ = call(
                &state,
                api(
                    "POST",
                    "/api/history",
                    Some(&token),
                    Some(json!({ "song_id": song })),
                ),
            )
            .await;
        }
        let (_, limited) = call(
            &state,
            api("GET", "/api/history/stats?top=1", Some(&token), None),
        )
        .await;
        assert_eq!(
            limited["top_songs"].as_array().map(Vec::len),
            Some(1),
            "{limited}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 3. 收藏幂等
    // ─────────────────────────────────────────────────────────────────────────
    /// 画布 UT「收藏幂等」：同一首连收两次只有一行、第二次 200；取消两次第二次
    /// removed:false；收藏列表里出现且只出现一次；不存在的歌 / 软删的歌 → 404。
    #[tokio::test]
    async fn favorite_add_and_remove_are_idempotent() {
        let (state, _temp) = test_state("s21-favorite-idem");
        let (user_id, token) = issue_token(&state, "alice");
        let song_id = seed_song(&state, "/music/a.mp3");
        let uri = format!("/api/favorites/{song_id}");

        // 第一次收藏 → 201
        let (status, body) = call(&state, api("POST", &uri, Some(&token), None)).await;
        assert_eq!(status, StatusCode::CREATED, "首次收藏应 201：{body}");
        assert_eq!(body["favorited"], true);
        assert_eq!(body["created"], true);

        // 第二次收藏 → 200，且库里只有一行
        let (status, body) = call(&state, api("POST", &uri, Some(&token), None)).await;
        assert_eq!(status, StatusCode::OK, "重复收藏应 200：{body}");
        assert_eq!(body["favorited"], true, "响应要体现「已收藏」");
        assert_eq!(body["created"], false);
        assert_eq!(count_favorites(&state, user_id), 1, "重复收藏不能产生第二行");

        // 收藏列表里出现且只出现一次
        let (status, body) = call(&state, api("GET", "/api/favorites", Some(&token), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 1);
        assert_eq!(favorite_song_ids(&body), vec![song_id]);
        let item = &body["items"][0];
        assert!(item.get("user_id").is_none(), "不暴露内部字段 user_id");
        assert!(item.get("file_path").is_none(), "不暴露服务器路径");

        // 取消：第一次 removed:true，第二次 200 + removed:false（幂等）
        let (status, body) = call(&state, api("DELETE", &uri, Some(&token), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["removed"], true);
        let (status, body) = call(&state, api("DELETE", &uri, Some(&token), None)).await;
        assert_eq!(status, StatusCode::OK, "重复取消应 200：{body}");
        assert_eq!(body["removed"], false);
        assert_eq!(count_favorites(&state, user_id), 0);
        let (_, body) = call(&state, api("GET", "/api/favorites", Some(&token), None)).await;
        assert_eq!(body["total"], 0);
        assert!(favorite_song_ids(&body).is_empty());

        // 不存在的歌 → 404（不能靠外键报错变成 500）
        let (status, body) = call(
            &state,
            api("POST", "/api/favorites/999999", Some(&token), None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "NOT_FOUND");

        // 软删的歌 → 404（收藏与取消同一口径）
        soft_delete(&state, song_id);
        let (status, _) = call(&state, api("POST", &uri, Some(&token), None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "软删的歌不能收藏");
        let (status, _) = call(&state, api("DELETE", &uri, Some(&token), None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "软删的歌不能取消收藏");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 4. 断点续播读写
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「断点续播读写」：PUT 写 resume:{song_id} = 毫秒 → GET 原样读回；
    /// 同键覆盖不新增行。
    #[tokio::test]
    async fn resume_position_round_trips_through_generic_settings() {
        let (state, _temp) = test_state("s21-resume");
        let (user_id, token) = issue_token(&state, "alice");
        let song_id = seed_song(&state, "/music/a.mp3");
        let key = format!("{RESUME_KEY_PREFIX}{song_id}");
        // 键约定写死在文档里，这里再钉一次，防止哪天前缀被改掉
        assert_eq!(key, format!("resume:{song_id}"));

        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![(key.as_str(), Some("12345"))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "写设置应 200：{body}");
        assert_eq!(body["settings"][key.as_str()], "12345");

        // GET 原样读回
        let (status, body) = call(&state, api("GET", "/api/settings", Some(&token), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["settings"][key.as_str()], "12345",
            "续播位置必须原样读回"
        );
        assert_eq!(body["total"], 1);

        // 直接查库核对（不是只信响应体）
        {
            let conn = state.db.acquire().expect("借连接");
            assert_eq!(
                settings::value(&conn, user_id, &key)
                    .expect("读设置")
                    .as_deref(),
                Some("12345")
            );
        }

        // 覆盖写：同键再写就是更新
        let (status, _) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![(key.as_str(), Some("67890"))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, body) = call(&state, api("GET", "/api/settings", Some(&token), None)).await;
        assert_eq!(body["settings"][key.as_str()], "67890");
        assert_eq!(body["total"], 1, "覆盖写不该多出一行");
        assert_eq!(count_settings(&state, user_id), 1);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 5. 跨用户隔离
    // ─────────────────────────────────────────────────────────────────────────

    /// 跨用户隔离：A 的历史 / 收藏 / 设置，B 全都看不到；B 的列表里只有 B 自己的。
    #[tokio::test]
    async fn playback_data_is_isolated_between_users() {
        let (state, _temp) = test_state("s21-isolation");
        let (alice_id, alice) = issue_token(&state, "alice");
        let (bob_id, bob) = issue_token(&state, "bob");
        let song_a = seed_song(&state, "/music/a.mp3");
        let song_b = seed_song(&state, "/music/b.mp3");
        let alice_key = format!("{RESUME_KEY_PREFIX}{song_a}");
        let bob_key = format!("{RESUME_KEY_PREFIX}{song_b}");

        // alice 写自己的一份
        let (status, body) = call(
            &state,
            api(
                "POST",
                "/api/history",
                Some(&alice),
                Some(json!({ "song_id": song_a })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let (status, _) = call(
            &state,
            api("POST", &format!("/api/favorites/{song_a}"), Some(&alice), None),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&alice),
                Some(settings_body(vec![(alice_key.as_str(), Some("111"))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // bob 写自己的一份
        let (status, _) = call(
            &state,
            api(
                "POST",
                "/api/history",
                Some(&bob),
                Some(json!({ "song_id": song_b })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = call(
            &state,
            api("POST", &format!("/api/favorites/{song_b}"), Some(&bob), None),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&bob),
                Some(settings_body(vec![(bob_key.as_str(), Some("222"))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // 历史：各看各的
        let (status, body) = call(&state, api("GET", "/api/history", Some(&alice), None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"], 1, "alice 只看得到自己的历史：{body}");
        assert_eq!(history_song_ids(&body), vec![song_a]);
        let (_, body) = call(&state, api("GET", "/api/history", Some(&bob), None)).await;
        assert_eq!(body["total"], 1, "bob 只看得到自己的历史：{body}");
        assert_eq!(history_song_ids(&body), vec![song_b]);

        // 收藏：各看各的
        let (_, body) = call(&state, api("GET", "/api/favorites", Some(&alice), None)).await;
        assert_eq!(favorite_song_ids(&body), vec![song_a], "{body}");
        assert_eq!(body["total"], 1);
        let (_, body) = call(&state, api("GET", "/api/favorites", Some(&bob), None)).await;
        assert_eq!(favorite_song_ids(&body), vec![song_b], "{body}");

        // 设置：各看各的，键都不串门
        let (_, body) = call(&state, api("GET", "/api/settings", Some(&alice), None)).await;
        assert_eq!(body["settings"][alice_key.as_str()], "111");
        assert!(
            body["settings"].get(bob_key.as_str()).is_none(),
            "alice 看不到 bob 的设置：{body}"
        );
        assert_eq!(body["total"], 1);
        let (_, body) = call(&state, api("GET", "/api/settings", Some(&bob), None)).await;
        assert_eq!(body["settings"][bob_key.as_str()], "222");
        assert!(body["settings"].get(alice_key.as_str()).is_none());

        // 直接查库核对归属：行数按 user 分开，绝不串到对方头上
        assert_eq!(count_history(&state, alice_id), 1);
        assert_eq!(count_history(&state, bob_id), 1);
        assert_eq!(count_favorites(&state, alice_id), 1);
        assert_eq!(count_favorites(&state, bob_id), 1);
        assert_eq!(count_settings(&state, alice_id), 1);
        assert_eq!(count_settings(&state, bob_id), 1);

        // 同一首歌：A 取消收藏不能动 B 的收藏
        let (status, _) = call(
            &state,
            api("POST", &format!("/api/favorites/{song_a}"), Some(&bob), None),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "bob 也能收藏同一首歌");
        let (status, body) = call(
            &state,
            api("DELETE", &format!("/api/favorites/{song_a}"), Some(&alice), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["removed"], true);
        let (_, body) = call(&state, api("GET", "/api/favorites", Some(&bob), None)).await;
        assert_eq!(
            favorite_song_ids(&body),
            vec![song_a, song_b],
            "alice 取消收藏不该动 bob 的：{body}"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 6. 设置的上限与 null 删除
    // ─────────────────────────────────────────────────────────────────────────

    /// settings 上限：超长键 / 空白键 / 超长值 / 非字符串值 → 400；
    /// null 值删除该键（删除不存在的键也幂等），整批校验失败不落库。
    #[tokio::test]
    async fn settings_validation_and_null_delete_are_enforced() {
        let (state, _temp) = test_state("s21-settings-limits");
        let (user_id, token) = issue_token(&state, "alice");

        // 正常批量写
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![
                    ("volume", Some("30")),
                    ("play_mode", Some("shuffle")),
                ])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["settings"]["volume"], "30");
        assert_eq!(body["settings"]["play_mode"], "shuffle");
        assert_eq!(body["total"], 2);
        assert_eq!(body["written"].as_array().map(Vec::len), Some(2));
        assert_eq!(count_settings(&state, user_id), 2);

        // null = 删除该键
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![("volume", None)])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["deleted"][0], "volume");
        assert!(body["settings"].get("volume").is_none(), "键应被删除：{body}");
        assert_eq!(body["total"], 1);
        assert_eq!(count_settings(&state, user_id), 1);

        // 删除不存在的键也幂等
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![("never", None)])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 1);

        // 空对象是合法的空操作
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 1);

        // 超长键 → 400
        let long_key = "k".repeat(MAX_SETTING_KEY_CHARS + 1);
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![(long_key.as_str(), Some("1"))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "超长键必须 400：{body}");
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("设置键"), "错误要中文说明：{message}");

        // 空键 / 含空白的键 → 400
        let (status, _) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![("", Some("1"))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "空键必须 400");
        let (status, _) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![("bad key", Some("1"))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "含空白的键必须 400");

        // 超长值 → 400
        let long_value = "v".repeat(MAX_SETTING_VALUE_CHARS + 1);
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![("ok", Some(long_value.as_str()))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "超长值必须 400：{body}");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("的值不能超过"));

        // 非字符串 / null 的值 → 400（数字不做类型推断）
        for value in [json!(30), json!(true), json!(["a"])] {
            let (status, body) = call(
                &state,
                api(
                    "PUT",
                    "/api/settings",
                    Some(&token),
                    Some(json!({ "volume": value })),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "非字符串值必须 400：{body}");
        }

        // 整批校验：一项不合法则整批不落库
        let (status, _) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![("good", Some("1")), ("bad key", Some("2"))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(count_settings(&state, user_id), 1, "整批校验失败不该写一半");
    }

    /// 每用户键数量上限：写满 MAX_SETTINGS_PER_USER 个之后，再新建键 → 400；
    /// 但「删一个再写一个」不受影响（上限看的是写入后的最终数量）；
    /// 单次批量超上限也在碰库之前就被挡下。
    #[tokio::test]
    async fn settings_per_user_key_quota_is_enforced() {
        let (state, _temp) = test_state("s21-settings-quota");
        let (user_id, token) = issue_token(&state, "alice");

        // 一次批量写满配额（单次批量上限 = 每用户配额 = 200，正好放得下）
        let keys: Vec<String> = (0..MAX_SETTINGS_PER_USER)
            .map(|index| format!("k{index}"))
            .collect();
        let entries: Vec<(&str, Option<&str>)> =
            keys.iter().map(|key| (key.as_str(), Some("1"))).collect();
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(entries)),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "写满配额应成功：{body}");
        assert_eq!(body["total"], MAX_SETTINGS_PER_USER);
        assert_eq!(count_settings(&state, user_id), MAX_SETTINGS_PER_USER);

        // 再新建一个键 → 400（不静默丢弃）
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![("one-more", Some("1"))])),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "超过键数量上限必须 400：{body}"
        );
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(&MAX_SETTINGS_PER_USER.to_string()),
            "错误要说清上限：{message}"
        );
        assert_eq!(count_settings(&state, user_id), MAX_SETTINGS_PER_USER);

        // 删一个再写一个：最终数量不变，应当放行
        let first = keys[0].as_str();
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![(first, None)])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], MAX_SETTINGS_PER_USER - 1);
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(vec![("one-more", Some("1"))])),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "删一个再写一个应放行：{body}");
        assert_eq!(body["total"], MAX_SETTINGS_PER_USER);

        // 单次批量超过上限 → 400
        let unique: Vec<String> = (0..=MAX_SETTINGS_BATCH_KEYS)
            .map(|index| format!("batch{index}"))
            .collect();
        let batch_entries: Vec<(&str, Option<&str>)> =
            unique.iter().map(|key| (key.as_str(), Some("1"))).collect();
        let (status, body) = call(
            &state,
            api(
                "PUT",
                "/api/settings",
                Some(&token),
                Some(settings_body(batch_entries)),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "超批量上限必须 400：{body}");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 7. 未登录一律 401
    // ─────────────────────────────────────────────────────────────────────────

    /// 7 条路由未登录（或令牌无效）一律 401 —— 受保护子 Router 的统一拦截。
    #[tokio::test]
    async fn playback_routes_require_authentication() {
        let (state, _temp) = test_state("s21-auth");
        let song_id = seed_song(&state, "/music/a.mp3");
        let cases: Vec<(&str, String, bool)> = vec![
            ("POST", "/api/history".to_string(), true),
            ("GET", "/api/history".to_string(), false),
            ("GET", "/api/favorites".to_string(), false),
            ("POST", format!("/api/favorites/{song_id}"), false),
            ("DELETE", format!("/api/favorites/{song_id}"), false),
            ("GET", "/api/settings".to_string(), false),
            ("PUT", "/api/settings".to_string(), true),
        ];
        for (method, uri, with_body) in cases {
            let body = if with_body { Some(json!({})) } else { None };
            let (status, value) = call(&state, api(method, &uri, None, body)).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {uri} 未登录必须 401：{value}"
            );
            assert_eq!(value["error"]["code"], "UNAUTHORIZED");
        }

        // 无效令牌同样是 401
        let (status, _) = call(
            &state,
            api("GET", "/api/history", Some("not-a-token"), None),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
