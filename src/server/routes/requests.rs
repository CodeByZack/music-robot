//! S23 · 歌曲请求（点歌）API：提交 + 去重合并 + 状态机 + 管理端。
//!
//! 路径（取自画布 API 清单）：
//!
//! * POST  /api/requests                 提交点歌（去重合并 + 自动投票）
//! * GET   /api/requests?mine=1          我提交的（普通用户不带参也只看得到自己的）
//! * GET   /api/requests?status=pending  按状态列（**仅 admin**）
//! * PATCH /api/requests/{id}            改状态 / 填拒绝理由（**仅 admin**）
//! * POST  /api/requests/{id}/fetch      触发获取（**仅 admin**，当前恒 503）
//! * POST  /api/requests/{id}/link       关联到已有歌曲（**仅 admin**）
//!
//! 全部挂在 routes::build_router 的**受保护子 Router** 上（require_auth 中间件）：
//! 未登录一律 401。需要 admin 的接口（PATCH / fetch / link / ?status=）在 handler 里
//! 用 AdminUser 提取器兜底 → 普通用户 403，而不是 401（401 = 没证明身份，
//! 403 = 身份有效但没权限，与 S15/S17 的口径一致）。GET 列表则**不能**要求 admin
//! （普通用户要看自己的），所以在 handler 里按 auth.0.role 分流。
//!
//! 响应体一律手写 serde_json::Value（项目规范：不引 serde derive），且**不暴露内部字段**：
//! 不回传 dedup_key（内部去重键）、user_id（谁提交的由令牌代表，不必回传），
//! 请求对外形状固定为 [request_json] 那一份。
//!
//! # 归一化（本步骤的核心，画布 UT 第一项）
//!
//! 关键问题：**同一首歌的不同写法必须落到同一个 dedup_key 上**，否则同一首歌会分裂成
//! 多条请求、票数被稀释，「按需求人数排序」也就失去意义。归一化 [normalize] 是纯函数
//! （只依赖入参，无 I/O、无全局状态），因此可以穷举单测。规则与理由：
//!
//! 1. **全半角折叠**：U+FF01–U+FF5E（全角 ASCII）按 -0xFEE0 折回半角，
//!    U+3000（全角空格）折成半角空格。理由：中文输入法下 ＡＢＣ１２３（） 与
//!    ABC123() 是同一串字符的两种宽度写法，不折叠就会被当成两首歌；
//!    全角斜杠 ／(U+FF0F) 也因此折成 /，「七里香／周杰伦」与「七里香/周杰伦」归一。
//! 2. **大小写折叠**：逐字符 to_lowercase()（不是 to_ascii_lowercase）。
//!    先用第 1 条折宽度再转小写，所以全角大写 ＪＡＹ 也会变成 jay。
//!    理由：曲名/歌手的英文大小写是纯书写风格差异（Jay Chou / JAY CHOU）。
//! 3. **空白折叠**：任意 char::is_whitespace()（半角空格、全角空格、Tab、换行）或
//!    控制字符都当作**一次词间分隔**，连续多个折叠成**单个半角空格**，并 trim 掉
//!    首尾的分隔。理由：" 七里香 "、"七里　香"、"七里\t香" 都该归到同一个键。
//!
//! **刻意不做的事**（写清楚免得后人「顺手加强」）：不删标点、不做繁简转换、不做同义词。
//! 因为把标点当噪声删掉会让 "A+B" 与 "A B"、"不-爱" 与 "不爱" 撞键 ——
//! 那是**假合并**（把不同的歌并成一条），比漏合并更难发现、更难修。画布点名的三类做完即可。
//!
//! # dedup_key 的构造与分隔符选择
//!
//! 键 = normalize(title) + 分隔符 + normalize(artist)，分隔符是
//! [KEY_SEPARATOR] = U+001F（ASCII 的 Unit Separator，不可打印控制符）。
//!
//! **为什么它安全**：上面的规则 3 把**所有**控制字符（含 U+001F 自己）都折叠成空格或
//! trim 掉，因此 normalize 的输出里**永远不可能出现 U+001F**。于是键里那个 U+001F
//! 必然是我们插入的分隔符本身，("A", "B|C") 与 ("A|B", "C") 得到
//! "a␟b|c" 与 "a|b␟c"，**不同键**（有专门用例钉死这个坑）。
//! 用 |、/、- 这类可打印字符当分隔符就不行 —— 它们会出现在真实曲名里。
//! 反过来说：如果哪天放宽规则 3 不再吃掉控制字符，这个分隔符就不再安全，必须一起改。
//!
//! # 合并与幂等（画布 UT）
//!
//! * 提交时先算 dedup_key，交给 repo 的 requests::find_or_create：命中已有请求就
//!   **复用、不新建**，否则插一条新的。响应用 created: true/false 明确区分
//!   「新建了」还是「合并到已有」。
//! * 无论新建还是合并，都调一次 requests::add_vote(请求, 当前用户) —— 首个提交人
//!   同时也是第一个「想要」的人，所以新请求的票数是 1 而不是 0。
//! * add_vote 返回 false 就表示「这个人已经投过了」（request_votes 是
//!   (request_id, user_id) 复合主键）。同一个用户重复提交同一首歌 → 不新建、不重复投票，
//!   票数保持 1。响应里的 voted 字段如实反映这次到底有没有新增一票。
//! * 并发点歌由 dedup_key 上的唯一索引兜底，repo 已经把冲突接住并转成「复用」。
//!
//! # 列表与排序
//!
//! GET /api/requests 每条都带 vote_count，列表按 **票数降序**，票数相同按 **id 升序**
//! 兜底（保证顺序确定，不会两次请求给出不同结果）。画布没给分页，这里列出全量
//! （与 S21 的收藏列表同口径：不分页、总量小）。
//!
//! # 权限口径
//!
//! * 普通用户：**无论带什么 query，都只看得到自己提交的**（user_id 只来自 AuthUser，
//!   绝不从 query / body 读）。带 ?status= 是「全量按状态筛」，属于管理端能力 → **403**。
//! * admin：不带 query 看全部，?status= 按状态看全部，?mine=1 看自己提交的。
//! * PATCH / fetch / link 三条写接口一律 admin；非 admin → 403。
//!
//! # 状态机
//!
//! pending → processing → done | rejected，done / rejected 是**终态**。
//! 其余任何流转（含自环、终态出发、跳步）都是 **400** + 中文说明。两个前置条件：
//!
//! * 目标是 rejected 必须带非空的 reject_reason，否则 400；目标不是 rejected 时
//!   顺手把 reject_reason 清掉（不留脏数据）。
//! * 目标是 done 必须已经关联 song_id，否则 400。关联的正规入口是
//!   POST /{id}/link（它把 song_id 写上并把状态置 done），所以 PATCH 直达 done
//!   只对「已经有关联歌但还没置 done」的行有意义。
//! * status 字段取值非法（不在四个枚举里）→ 400，不静默降级。
//!
//! # /fetch 为什么返回 503
//!
//! 先分清两条**互相独立**的插件线（画布上也是分开画的，别混）：
//!
//! * **scraper**（刮削）：给*已经在库里*的歌补 title / artist / album / year / 封面。
//!   S23 起 `AppState::new` 会扫 `plugins.dir` 组装插件表，**这条已经接通**
//!   （见 `plugins/musicbrainz.js`）。
//! * **provider**（获取）：画布原文是「入参 {song:{title,artist,album}, target_dir} →
//!   出参 {ok, file_path}；系统接管：扫描该文件 → 入库 → 关联 song_id → done」——
//!   即把**音频文件本身**下下来。这个 kind **尚未实现**，注册表目前只加载
//!   `kind = "scraper"`。
//!
//! 本接口要的是**后者**，所以仍然**明确返回 503 + 中文说明**
//! （[FETCH_UNAVAILABLE_MESSAGE]），**绝不假装成功** —— 回 200 / 202 会让前端以为
//! 任务已经排上队，用户永远等不到结果，还查不出原因。
//! 【provider 接通后这里应改为触发实际流程】：置 processing → 调 provider 插件下载 →
//! 扫描产物入库 → 关联 song_id 置 done（失败保留 pending + 记录错误，可重试）。
//!
//! # 阻塞调用
//!
//! rusqlite 全是同步阻塞调用，统一用 library::run_db 包进 spawn_blocking（S14 铁律），
//! 绝不在 async 上下文里直接碰连接池。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::db::models::{RequestStatus, Role, SongRequest};
use crate::db::repos::{requests, songs};
use crate::server::auth::{AdminUser, AuthUser};
use crate::server::error::{ApiError, ApiResult};
use crate::server::state::AppState;

use super::library::{parse_id, run_db, QueryParams};

// ─────────────────────────────────────────────────────────────────────────────
// 常量
// ─────────────────────────────────────────────────────────────────────────────

/// dedup_key 里 title 与 artist 之间的分隔符（ASCII Unit Separator，U+001F）。
///
/// 安全性见模块头注释：归一化会把所有控制字符折叠成空格，所以它不可能出现在
/// normalize 的输出里，("A","B|C") 与 ("A|B","C") 不会撞键。
const KEY_SEPARATOR: char = '\u{1f}';

/// title 最大字符数。画布只点名了 note/album 限长，这里顺手也挡住 title/artist ——
/// NOT NULL 挡不住超长文本，不设上限的 TEXT 列会原样进库、进响应体。
const MAX_TITLE_CHARS: usize = 200;

/// artist 最大字符数。
const MAX_ARTIST_CHARS: usize = 200;

/// album 最大字符数（画布点名限长）。
const MAX_ALBUM_CHARS: usize = 200;

/// note 最大字符数（画布点名限长）。备注是自由文本，给得宽一些。
const MAX_NOTE_CHARS: usize = 500;

/// reject_reason 最大字符数。
const MAX_REJECT_REASON_CHARS: usize = 500;

/// 未配置 provider（下载）插件时 /fetch 的固定文案。
///
/// 措辞必须是 **provider** 而不是「刮削插件」：刮削那条线早就接通了，文案说成
/// 「未配置刮削插件」会把排查方向直接带偏（见模块头「/fetch 为什么返回 503」）。
const FETCH_UNAVAILABLE_MESSAGE: &str = "未配置下载（provider）插件，无法自动获取";

// ─────────────────────────────────────────────────────────────────────────────
// 归一化（纯函数）
// ─────────────────────────────────────────────────────────────────────────────

/// 全半角折叠：全角 ASCII（U+FF01–U+FF5E）按 -0xFEE0 折回半角，全角空格折成半角空格。
///
/// 该区间到半角是一一对应的固定偏移，from_u32 必然成功；unwrap_or(ch) 只是
/// 防御性的兜底（保持原字符），不使用 unwrap / expect。
fn fold_width(ch: char) -> char {
    match ch {
        '\u{3000}' => ' ',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(ch as u32 - 0xFEE0).unwrap_or(ch),
        _ => ch,
    }
}

/// 归一化单个字段：折全半角 → 折大小写 → 空白/控制字符折叠成单个半角空格并 trim。
///
/// 纯函数（无 I/O、无全局状态），测试可以直接穷举输入。
/// 保证：返回值里不含任何控制字符，尤其不含 [KEY_SEPARATOR]。
fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    // 已经见到过正文、且中间出现过分隔 → 下一个正文前补一个空格（连续空白只补一次）。
    let mut pending_space = false;
    let mut seen_text = false;

    for ch in raw.chars() {
        let folded = fold_width(ch);
        // 空白与控制字符同处理：都当一次词间分隔，而不是正文。
        // 控制字符（含 KEY_SEPARATOR）因此在输出里不可能出现。
        if folded.is_whitespace() || folded.is_control() {
            if seen_text {
                pending_space = true;
            }
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        seen_text = true;
        for lower in folded.to_lowercase() {
            out.push(lower);
        }
    }
    out
}

/// 由 title / artist 构造去重键：normalize(title) + KEY_SEPARATOR + normalize(artist)。
///
/// artist 为 None 时按空串处理，因此 artist: None 与 artist: Some("") 同键。
fn dedup_key(title: &str, artist: Option<&str>) -> String {
    let mut key = normalize(title);
    key.push(KEY_SEPARATOR);
    key.push_str(&normalize(artist.unwrap_or("")));
    key
}

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

/// 读一个可选文本字段：缺席 / null 视作 None，空串（trim 后）也归成 None，其余 trim 后限长。
///
/// 类型不对（数字 / 布尔 / 对象）一律 400，不做隐式字符串化 —— 那会让前端以为写进去了。
fn parse_optional_text(
    body: &Value,
    field: &'static str,
    max_chars: usize,
) -> Result<Option<String>, ApiError> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            let chars = trimmed.chars().count();
            if chars > max_chars {
                return Err(ApiError::bad_request(format!(
                    "{field} 不能超过 {max_chars} 个字符（当前 {chars} 个）"
                )));
            }
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Some(_) => Err(ApiError::bad_request(format!(
            "{field} 必须是字符串或 null"
        ))),
    }
}

/// 解析状态字符串；非法取值 → 400（不静默降级成 pending）。
fn parse_status(raw: &str) -> Result<RequestStatus, ApiError> {
    RequestStatus::parse(raw).map_err(|_| {
        ApiError::bad_request(format!(
            "状态取值非法: {raw}；只能是 pending / processing / done / rejected"
        ))
    })
}

/// 解析 ?mine=：缺省 / 空串 = false；1/true = true；0/false = false；其余 400。
fn parse_mine(raw: Option<&str>) -> Result<bool, ApiError> {
    let Some(text) = raw else {
        return Ok(false);
    };
    match text.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" => Ok(false),
        "1" | "true" => Ok(true),
        other => Err(ApiError::bad_request(format!(
            "mine 参数只能是 1 或 0（收到 {other}）"
        ))),
    }
}

/// 提交点歌的校验结果（校验过、可直接落库）。
struct Submission {
    /// 已 trim 的标题
    title: String,
    /// 已 trim 的歌手（空串归 None）
    artist: Option<String>,
    /// 已 trim 的专辑
    album: Option<String>,
    /// 已 trim 的备注
    note: Option<String>,
    /// 由 title / artist 算出的去重键
    dedup_key: String,
}

/// 校验提交体并算出 dedup_key。
///
/// * title 必须是字符串且**非空**（画布：title 非空）；
/// * artist 可为空；
/// * 但归一化后两者**不能同时为空** —— title 全角空格 / 制表符这类「看着非空、
///   归一化后是空」的输入会在这里被拦下（否则会造出一个键为「␟」的空请求）。
fn parse_submission(body: &Value) -> Result<Submission, ApiError> {
    let raw_title = body
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("缺少字符串字段 title"))?;
    if raw_title.is_empty() {
        return Err(ApiError::bad_request("title 不能为空"));
    }
    let title = raw_title.trim().to_string();
    let chars = title.chars().count();
    if chars > MAX_TITLE_CHARS {
        return Err(ApiError::bad_request(format!(
            "title 不能超过 {MAX_TITLE_CHARS} 个字符（当前 {chars} 个）"
        )));
    }

    let artist = parse_optional_text(body, "artist", MAX_ARTIST_CHARS)?;
    let album = parse_optional_text(body, "album", MAX_ALBUM_CHARS)?;
    let note = parse_optional_text(body, "note", MAX_NOTE_CHARS)?;

    if normalize(&title).is_empty() && normalize(artist.as_deref().unwrap_or("")).is_empty() {
        return Err(ApiError::bad_request(
            "歌曲标题或歌手至少要有一个（归一化后不能都为空）",
        ));
    }

    let key = dedup_key(&title, artist.as_deref());
    Ok(Submission {
        title,
        artist,
        album,
        note,
        dedup_key: key,
    })
}

/// 状态机：只允许画布写死的三条流转，其余（自环 / 终态出发 / 跳步）一律非法。
fn is_allowed_transition(from: RequestStatus, to: RequestStatus) -> bool {
    matches!(
        (from, to),
        (RequestStatus::Pending, RequestStatus::Processing)
            | (RequestStatus::Processing, RequestStatus::Done)
            | (RequestStatus::Processing, RequestStatus::Rejected)
    )
}

/// 请求是否处于终态（done / rejected 之后不能再流转、不能再 link）。
fn is_terminal(status: RequestStatus) -> bool {
    matches!(status, RequestStatus::Done | RequestStatus::Rejected)
}

// ─────────────────────────────────────────────────────────────────────────────
// 对外 JSON
// ─────────────────────────────────────────────────────────────────────────────

/// 点歌请求的对外 JSON（**不含 dedup_key / user_id**：一个是内部去重键，一个是内部归属）。
fn request_json(row: &SongRequest, vote_count: i64) -> Value {
    json!({
        "id": row.id,
        "title": row.title,
        "artist": row.artist,
        "album": row.album,
        "note": row.note,
        "status": row.status.as_str(),
        "reject_reason": row.reject_reason,
        "song_id": row.song_id,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
        "vote_count": vote_count,
    })
}

/// 「请求不存在」的统一答复（PATCH / fetch / link 三处同一句，避免文案漂移）。
fn request_not_found() -> ApiError {
    ApiError::not_found("请求的点歌不存在")
}

// ─────────────────────────────────────────────────────────────────────────────
// handlers
// ─────────────────────────────────────────────────────────────────────────────

/// POST /api/requests —— 提交点歌（去重合并 + 自动投票）。
///
/// 请求体：{ "title": "七里香", "artist": "周杰伦", "album": null, "note": "麻烦快点" }
/// （只有 title 必填；artist / album / note 可缺席）。
///
/// * **新建** → **201** + { request, created: true, voted: true }；
/// * **命中已有**（归一化后同键）→ **200** + { request, created: false, ... }，
///   并把当前用户的一票记上（已投过则 voted: false，不重复投票）。
///
/// 响应里的 created 让前端能区分「新建」与「合并到已有」，voted 说明这次提交
/// 到底有没有新增一票；request.vote_count 是合并后的总需求人数。
pub async fn submit(
    auth: AuthUser,
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    require_object(&body)?;
    let Submission {
        title,
        artist,
        album,
        note,
        dedup_key,
    } = parse_submission(&body)?;
    // 用户身份只来自令牌，绝不看请求体里的 user_id。
    let user_id = auth.0.id;

    let (row, created, voted, vote_count) = run_db(Arc::clone(&state.db), move |conn| {
        let candidate = SongRequest {
            id: 0,
            user_id,
            title,
            artist,
            album,
            note,
            dedup_key,
            status: RequestStatus::Pending,
            reject_reason: None,
            song_id: None,
            created_at: 0,
            updated_at: 0,
        };
        let (row, created) = requests::find_or_create(conn, &candidate)?;
        // 首个提交人也是第一个「想要」的人；重复提交时这里是幂等 no-op。
        let voted = requests::add_vote(conn, row.id, user_id)?;
        let vote_count = requests::vote_count(conn, row.id)?;
        Ok((row, created, voted, vote_count))
    })
    .await?;

    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        Json(json!({
            "request": request_json(&row, vote_count),
            "created": created,
            "voted": voted,
        })),
    ))
}

/// GET /api/requests —— 列出点歌请求（按票数降序，票数相同按 id 升序）。
///
/// 权限口径（模块头注释有完整说明）：
///
/// * 普通用户 → 只返回**自己提交的**，带 ?status= 一律 **403**；
/// * admin → 不带 query 看全部，?status= 按状态筛全部，?mine=1 看自己提交的。
///
/// 响应：{ items, total }，每个 item 是 request_json（带 vote_count）。
///
/// 说明：repo 的 list / list_by_status 只按创建时间排，票数不在 SQL 里；这里取回候选集
/// 后在内存里补 vote_count 再排序（画布没给分页，全量返回，量级与收藏列表同级）。
/// 每条一次 vote_count 查询是 N+1，但请求列表量小且 repo 没有批量接口 —— 不为此改 repo。
pub async fn list(
    auth: AuthUser,
    State(state): State<AppState>,
    params: QueryParams,
) -> ApiResult<Json<Value>> {
    let mine = parse_mine(params.get("mine"))?;
    let is_admin = auth.0.role == Role::Admin;

    // 空串视同没带该参数。
    let status_filter = match params.get("status") {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(parse_status(trimmed)?)
            }
        }
    };

    // 「按状态列」隐含「全量」，属于管理端能力：普通用户 → 403。
    if status_filter.is_some() && !is_admin {
        return Err(ApiError::forbidden(
            "按状态列出全部点歌请求仅管理员可用；普通用户请用 ?mine=1 查看自己提交的",
        ));
    }

    let me = auth.0.id;
    // 普通用户永远只看自己的；admin 只有显式 ?mine=1 才收窄。
    let scope_mine = mine || !is_admin;

    let (items, total) = run_db(Arc::clone(&state.db), move |conn| {
        let rows = match status_filter {
            Some(status) => requests::list_by_status(conn, status, i64::MAX, 0)?,
            None => requests::list(conn, i64::MAX, 0)?,
        };
        let mut items: Vec<(SongRequest, i64)> = Vec::with_capacity(rows.len());
        for row in rows {
            if scope_mine && row.user_id != me {
                continue;
            }
            let votes = requests::vote_count(conn, row.id)?;
            items.push((row, votes));
        }
        // 票数降序，票数相同按 id 升序兜底（确定顺序）。
        items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
        let total = items.len() as i64;
        let items = items
            .iter()
            .map(|(row, votes)| request_json(row, *votes))
            .collect::<Vec<Value>>();
        Ok((items, total))
    })
    .await?;

    Ok(Json(json!({ "items": items, "total": total })))
}

/// PATCH /api/requests/{id} —— 改状态 / 填拒绝理由（**仅 admin**）。
///
/// 请求体：{ "status": "processing" } 或 { "status": "rejected", "reject_reason": "版权原因" }。
///
/// * 目标 rejected 必须带非空 reject_reason，否则 400；
/// * 目标 done 必须先关联歌曲（走 /link），否则 400；
/// * 非法流转 / 非法 status 值 → 400；请求不存在 → 404。
///
/// 成功 **200** + { request }（改后的完整形状，含 vote_count）。
pub async fn update(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    require_object(&body)?;
    let raw_status = body
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("缺少字符串字段 status"))?;
    let target = parse_status(raw_status)?;
    let reject_reason = parse_optional_text(&body, "reject_reason", MAX_REJECT_REASON_CHARS)?;

    if target == RequestStatus::Rejected && reject_reason.is_none() {
        return Err(ApiError::bad_request(
            "拒绝请求必须填写 reject_reason（拒绝理由）",
        ));
    }

    let (row, vote_count) = run_db(Arc::clone(&state.db), move |conn| {
        let current = requests::get(conn, id)?.ok_or_else(request_not_found)?;
        if !is_allowed_transition(current.status, target) {
            return Err(ApiError::bad_request(format!(
                "不允许的状态流转：{} → {}；只允许 pending → processing → done/rejected，done 与 rejected 是终态",
                current.status.as_str(),
                target.as_str()
            )));
        }
        if target == RequestStatus::Done && current.song_id.is_none() {
            return Err(ApiError::bad_request(
                "改为 done 前必须先用 POST /api/requests/{id}/link 关联歌曲",
            ));
        }
        // 非 rejected 时把原因清掉，避免残留脏数据。
        let stored_reason = if target == RequestStatus::Rejected {
            reject_reason.as_deref()
        } else {
            None
        };
        requests::update_status(conn, id, target, stored_reason)?;
        let updated = requests::get(conn, id)?
            .ok_or_else(|| ApiError::internal("点歌请求刚更新就查不到了，怀疑有并发删除"))?;
        let vote_count = requests::vote_count(conn, id)?;
        Ok((updated, vote_count))
    })
    .await?;

    Ok(Json(json!({ "request": request_json(&row, vote_count) })))
}

/// POST /api/requests/{id}/fetch —— 触发获取（**仅 admin**）。
///
/// 请求不存在 → 404；存在 → **503 + 中文说明**（provider 插件类型尚未实现）。
/// 【provider 接通后这里应改为触发实际流程】，见模块头注释；在那之前绝不假装成功。
pub async fn fetch(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    // 先确认请求存在（否则「获取一个不存在的请求」回 503 会误导排查方向）。
    run_db(Arc::clone(&state.db), move |conn| {
        if requests::get(conn, id)?.is_none() {
            return Err(request_not_found());
        }
        Ok(())
    })
    .await?;

    // 依赖缺失如实报 503，不返回 200/202 假装任务已排队。
    Err(ApiError::service_unavailable(FETCH_UNAVAILABLE_MESSAGE))
}

/// POST /api/requests/{id}/link —— 关联到已有歌曲（**仅 admin**）。
///
/// 请求体：{ "song_id": 3 }。
///
/// * 请求不存在 → 404；请求已在终态 → 400；
/// * 歌曲不存在 / 已软删 → **404**（先查歌，不让外键失败变成 500）；
/// * 成功 → 关联 song_id 并把状态置 **done**，返回 200 + { request }。
pub async fn link(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = parse_id(&raw_id)?;
    require_object(&body)?;
    let song_id = body
        .get("song_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::bad_request("缺少整数 song_id"))?;

    let (row, vote_count) = run_db(Arc::clone(&state.db), move |conn| {
        let current = requests::get(conn, id)?.ok_or_else(request_not_found)?;
        if is_terminal(current.status) {
            return Err(ApiError::bad_request(format!(
                "点歌请求已处于终态（{}），不能再关联歌曲",
                current.status.as_str()
            )));
        }
        // songs::get 的 include_deleted = false：软删的歌也算「不存在」。
        if songs::get(conn, song_id, false)?.is_none() {
            return Err(ApiError::not_found("要关联的歌曲不存在"));
        }
        requests::attach_song(conn, id, song_id)?;
        requests::update_status(conn, id, RequestStatus::Done, None)?;
        let updated = requests::get(conn, id)?
            .ok_or_else(|| ApiError::internal("点歌请求刚关联就查不到了，怀疑有并发删除"))?;
        let vote_count = requests::vote_count(conn, id)?;
        Ok((updated, vote_count))
    })
    .await?;

    Ok(Json(json!({ "request": request_json(&row, vote_count) })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request};
    use rusqlite::params;
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::db::migrations;
    use crate::db::models::User;
    use crate::db::pool::{DbPool, TempDb};
    use crate::server::auth::sign_token_with_ttl;
    use crate::server::routes::build_router;

    /// 测试用 JWT 密钥。
    const SECRET: &str = "s23-secret";

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
        config.storage.library_roots = vec!["/tmp/server-s23-requests-music".to_string()];
        (AppState::new(Arc::new(pool), Arc::new(config)), temp)
    }

    /// 预置一个用户并签一个可用令牌（不走注册接口，省掉 argon2 开销）。
    /// 返回 (user_id, token)。
    fn issue_token(state: &AppState, username: &str, role: Role) -> (i64, String) {
        let id = {
            let conn = state.db.acquire().expect("借连接");
            conn.execute(
                "INSERT INTO users (username, password_hash, role, created_at)
                 VALUES (?1, 'hash', ?2, ?3)",
                params![username, role.as_str(), crate::db::now_unix_ms()],
            )
            .expect("预置用户");
            conn.last_insert_rowid()
        };
        let user = User {
            id,
            username: username.to_string(),
            password_hash: "hash".to_string(),
            role,
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

    /// 直接查库：song_requests 总行数（用来钉「库里只有 N 行」）。
    fn count_requests(state: &AppState) -> i64 {
        let conn = state.db.acquire().expect("借连接");
        requests::count(&conn).expect("数请求")
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

    /// 列表响应里的请求 id（顺序敏感）。
    fn item_ids(body: &Value) -> Vec<i64> {
        body["items"]
            .as_array()
            .expect("items 是数组")
            .iter()
            .map(|item| item["id"].as_i64().expect("item.id 是整数"))
            .collect()
    }

    /// 用给定身份提交一条点歌，返回 (状态码, 响应体)。
    async fn post_request(
        state: &AppState,
        token: &str,
        title: &str,
        artist: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut body = json!({ "title": title });
        if let Some(artist) = artist {
            body["artist"] = json!(artist);
        }
        call(state, api("POST", "/api/requests", Some(token), Some(body))).await
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 1. 归一化（纯函数穷举）
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT 第一项：大小写 / 空格 / 全半角三类各自的穷举用例。
    #[test]
    fn normalize_folds_case_spaces_and_fullwidth() {
        // ① 大小写（ASCII）
        assert_eq!(normalize("Jay Chou"), "jay chou");
        assert_eq!(normalize("jay chou"), "jay chou");
        assert_eq!(normalize("JAY CHOU"), "jay chou");
        // ① 大小写（全角字母也要能折：先折宽度再转小写）
        assert_eq!(normalize("ＪＡＹ　ＣＨＯＵ"), "jay chou");
        assert_eq!(normalize("Ｊａｙ Ｃｈｏｕ"), "jay chou");

        // ② 空格：首尾空白 / 连续空格 / 全角空格 / 制表符
        assert_eq!(normalize("  七里香  "), "七里香");
        assert_eq!(normalize("七里   香"), "七里 香");
        assert_eq!(normalize("七里\u{3000}香"), "七里 香");
        assert_eq!(normalize("七里\t香"), "七里 香");
        assert_eq!(normalize("七里\n香"), "七里 香");
        assert_eq!(normalize("\t\u{3000}七里香\u{3000}\t"), "七里香");
        // 全角空格 + 半角空格混排也只留一个半角空格
        assert_eq!(normalize("七里 \u{3000} 香"), "七里 香");

        // ③ 全半角：字母 / 数字 / 括号
        assert_eq!(normalize("ＡＢＣ１２３（）"), "abc123()");
        assert_eq!(normalize("ABC123()"), "abc123()");
        // 全角斜杠也折成半角斜杠
        assert_eq!(normalize("七里香／周杰伦"), normalize("七里香/周杰伦"));
        // 全角标点 / 符号（落在 U+FF01–U+FF5E 区间）
        assert_eq!(normalize("Ａ＋Ｂ＝Ｃ"), "a+b=c");
        assert_eq!(normalize("！？．，"), "!?.,");

        // 不同歌不得撞键
        assert_ne!(normalize("七里香"), normalize("七里香2"));
        assert_ne!(normalize("七里香"), normalize("东风破"));
        assert_ne!(
            dedup_key("七里香", Some("周杰伦")),
            dedup_key("七里香", Some("费玉清"))
        );
        // 只有空格差异的同名同歌手必须同键（这正是要合并的情况）
        assert_eq!(dedup_key("七里香", Some("周杰伦")), dedup_key(" 七里香 ", Some("周杰伦")));
    }

    /// dedup_key 的分隔符坑：("A","B|C") 与 ("A|B","C") 必须不同键。
    #[test]
    fn dedup_key_separator_cannot_be_forged() {
        assert_ne!(dedup_key("A", Some("B|C")), dedup_key("A|B", Some("C")));
        assert_ne!(dedup_key("A", Some("B/C")), dedup_key("A/B", Some("C")));
        assert_ne!(dedup_key("A", Some("B-C")), dedup_key("A-B", Some("C")));
        // 输入里直接塞分隔符也不行：归一化会把控制字符折成空格
        assert_ne!(
            dedup_key("A", Some("B\u{1f}C")),
            dedup_key("A\u{1f}B", Some("C"))
        );
        // 穷举所有 ASCII 控制字符：归一化结果里都不得残留分隔符
        for code in 0u32..=0x1f {
            if let Some(ch) = char::from_u32(code) {
                let text = format!("a{ch}b");
                assert!(
                    !normalize(&text).contains(KEY_SEPARATOR),
                    "归一化结果不该含分隔符（输入码点 {code:#x}）"
                );
            }
        }
        // title / artist 的边界不会互相串位：title 为空与 artist 为空是两把不同的键
        assert_ne!(dedup_key("", Some("a")), dedup_key("a", None));
        // artist 为 None 与 Some("") 同键（都按空串处理）
        assert_eq!(dedup_key("七里香", None), dedup_key("七里香", Some("")));
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 2. 合并投票
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「合并投票」：A 提交「七里香/周杰伦」、B 提交「七里香／周杰伦」
    /// （全角斜杠 + 首尾空格差异）→ 同一个请求、vote_count == 2、库里只有 1 行。
    #[tokio::test]
    async fn same_song_from_two_users_merges_into_one_request() {
        let (state, _temp) = test_state("s23-merge");
        let (_alice_id, alice) = issue_token(&state, "alice", Role::User);
        let (_bob_id, bob) = issue_token(&state, "bob", Role::User);

        let (status, body) = post_request(&state, &alice, "七里香/周杰伦", None).await;
        assert_eq!(status, StatusCode::CREATED, "首个提交应 201：{body}");
        let request_id = body["request"]["id"].as_i64().expect("request.id 是整数");
        assert_eq!(body["created"], true);
        assert_eq!(body["voted"], true);
        assert_eq!(body["request"]["vote_count"], 1, "首个提交人自成一票");
        assert!(body["request"].get("dedup_key").is_none(), "不暴露 dedup_key");
        assert!(body["request"].get("user_id").is_none(), "不暴露 user_id");

        // B 用全角斜杠 + 首尾全角空格提交，应命中同一条
        let (status, body) = post_request(&state, &bob, "　七里香／周杰伦　", None).await;
        assert_eq!(status, StatusCode::OK, "命中已有应 200：{body}");
        assert_eq!(body["created"], false, "命中已有不该新建");
        assert_eq!(body["voted"], true, "B 是第一次投这一票");
        assert_eq!(body["request"]["id"], request_id, "必须复用同一条请求");
        assert_eq!(body["request"]["vote_count"], 2, "合并后票数是 2");
        assert_eq!(count_requests(&state), 1, "库里只该有 1 行");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 3. 重复提交幂等
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「重复提交幂等」：同一用户提交两次 → 只有 1 行、vote_count == 1、
    /// 第二次 created:false 且 voted:false（不重复投票）。
    #[tokio::test]
    async fn duplicate_submission_by_same_user_is_idempotent() {
        let (state, _temp) = test_state("s23-idempotent");
        let (_user_id, alice) = issue_token(&state, "alice", Role::User);

        let (status, body) = post_request(&state, &alice, "夜曲", Some("周杰伦")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["voted"], true);
        let request_id = body["request"]["id"].as_i64().expect("request.id");

        // 第二次：只差首尾空格（归一化后同键）→ 命中已有，且不重复投票
        let (status, body) = post_request(&state, &alice, "  夜曲 ", Some(" 周杰伦 ")).await;
        assert_eq!(status, StatusCode::OK, "重复提交应 200：{body}");
        assert_eq!(body["created"], false);
        assert_eq!(body["voted"], false, "同一用户不得重复投票");
        assert_eq!(body["request"]["id"], request_id);
        assert_eq!(body["request"]["vote_count"], 1, "票数必须是 1");
        assert_eq!(count_requests(&state), 1, "库里只该有 1 行");

        // ASCII 大小写 + 全角字母同样幂等：同一个人换写法再提交也不新增行 / 票
        let (_, body) = post_request(&state, &alice, "Numb", Some("Linkin Park")).await;
        let numb_id = body["request"]["id"].as_i64().expect("request.id");
        let (status, body) = post_request(&state, &alice, "  NUMB ", Some("ＬＩＮＫＩＮ　ＰＡＲＫ")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["created"], false);
        assert_eq!(body["voted"], false);
        assert_eq!(body["request"]["id"], numb_id);
        assert_eq!(body["request"]["vote_count"], 1);
        assert_eq!(count_requests(&state), 2, "只该多了 Numb 这一行");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 4. 状态流转
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「状态流转」：合法链路走通；终态不可回退；rejected 缺理由 400；
    /// done 未关联歌 400；status 取值非法 400；非 admin 一律 403。
    #[tokio::test]
    async fn status_machine_only_allows_the_documented_paths() {
        let (state, _temp) = test_state("s23-status");
        let (_admin_id, admin) = issue_token(&state, "root", Role::Admin);
        let (_user_id, alice) = issue_token(&state, "alice", Role::User);
        let song_id = seed_song(&state, "/music/wanted.mp3");

        let (_, body) = post_request(&state, &alice, "想听的歌", Some("某人")).await;
        let request_id = body["request"]["id"].as_i64().expect("request.id");
        let uri = format!("/api/requests/{request_id}");

        // 普通用户改状态 → 403（不是 401，也不是 404）
        let (status, body) = call(
            &state,
            api(
                "PATCH",
                &uri,
                Some(&alice),
                Some(json!({ "status": "processing" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "普通用户改状态必须 403：{body}");
        assert_eq!(body["error"]["code"], "FORBIDDEN");

        // 非法状态值 → 400
        let (status, body) = call(
            &state,
            api("PATCH", &uri, Some(&admin), Some(json!({ "status": "weird" }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "非法状态值必须 400：{body}");

        // pending → pending（自环）非法
        let (status, _) = call(
            &state,
            api("PATCH", &uri, Some(&admin), Some(json!({ "status": "pending" }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "自环流转必须 400");

        // 合法：pending → processing
        let (status, body) = call(
            &state,
            api(
                "PATCH",
                &uri,
                Some(&admin),
                Some(json!({ "status": "processing" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "pending → processing 应 200：{body}");
        assert_eq!(body["request"]["status"], "processing");

        // processing → done 但没关联歌 → 400
        let (status, body) = call(
            &state,
            api("PATCH", &uri, Some(&admin), Some(json!({ "status": "done" }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "done 未关联歌必须 400：{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("link"),
            "错误说明应指出走 link：{body}"
        );

        // processing → rejected 缺理由 → 400
        let (status, _) = call(
            &state,
            api(
                "PATCH",
                &uri,
                Some(&admin),
                Some(json!({ "status": "rejected", "reject_reason": "   " })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "rejected 缺理由必须 400");

        // processing → rejected 带理由 → 200，理由落库
        let (status, body) = call(
            &state,
            api(
                "PATCH",
                &uri,
                Some(&admin),
                Some(json!({ "status": "rejected", "reject_reason": "无版权" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "processing → rejected 应 200：{body}");
        assert_eq!(body["request"]["status"], "rejected");
        assert_eq!(body["request"]["reject_reason"], "无版权");

        // 终态回退 → 400
        let (status, _) = call(
            &state,
            api("PATCH", &uri, Some(&admin), Some(json!({ "status": "pending" }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "rejected → pending 必须 400");

        // 另一条请求走 link 直达 done（画布：link 把状态置 done）
        let (_, body) = post_request(&state, &alice, "另一首歌", None).await;
        let other = body["request"]["id"].as_i64().expect("request.id");
        let (status, body) = call(
            &state,
            api(
                "POST",
                &format!("/api/requests/{other}/link"),
                Some(&admin),
                Some(json!({ "song_id": song_id })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "link 应 200：{body}");
        assert_eq!(body["request"]["status"], "done");
        assert_eq!(body["request"]["song_id"], song_id);

        // done 是终态：再改回 pending → 400
        let (status, _) = call(
            &state,
            api(
                "PATCH",
                &format!("/api/requests/{other}"),
                Some(&admin),
                Some(json!({ "status": "pending" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "done → pending 必须 400");

        // 请求不存在 → 404
        let (status, _) = call(
            &state,
            api(
                "PATCH",
                "/api/requests/999999",
                Some(&admin),
                Some(json!({ "status": "processing" })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 5. user 只读自己的
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT「user 只读自己的」：A 两条、B 一条；B 的列表只有 B 的；
    /// B 带 ?status=pending → 403；admin 能看到全部（且可按状态筛）。
    #[tokio::test]
    async fn regular_users_only_see_their_own_requests() {
        let (state, _temp) = test_state("s23-isolation");
        let (_alice_id, alice) = issue_token(&state, "alice", Role::User);
        let (_bob_id, bob) = issue_token(&state, "bob", Role::User);
        let (_admin_id, admin) = issue_token(&state, "root", Role::Admin);

        let (_, body) = post_request(&state, &alice, "A 的第一首", None).await;
        let alice_first = body["request"]["id"].as_i64().expect("id");
        let (_, body) = post_request(&state, &alice, "A 的第二首", None).await;
        let alice_second = body["request"]["id"].as_i64().expect("id");
        let (_, body) = post_request(&state, &bob, "B 的歌", None).await;
        let bob_first = body["request"]["id"].as_i64().expect("id");
        assert_eq!(count_requests(&state), 3);

        // B 不带 query → 只有自己的那一条
        let (status, body) = call(&state, api("GET", "/api/requests", Some(&bob), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(item_ids(&body), vec![bob_first], "B 只能看到自己的：{body}");
        assert_eq!(body["total"], 1);

        // B 带 ?mine=1 → 仍然只有自己的
        let (status, body) = call(&state, api("GET", "/api/requests?mine=1", Some(&bob), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(item_ids(&body), vec![bob_first]);

        // 连 ?mine=0 也不能越权看到全部（无论带什么 query）
        let (status, body) = call(&state, api("GET", "/api/requests?mine=0", Some(&bob), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(item_ids(&body), vec![bob_first]);

        // B 带 ?status=pending → 403（按状态列全部是管理端能力）
        let (status, body) = call(
            &state,
            api("GET", "/api/requests?status=pending", Some(&bob), None),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "普通用户按状态列必须 403：{body}"
        );
        assert_eq!(body["error"]["code"], "FORBIDDEN");

        // admin 不带 query → 看到全部三条
        let (status, body) = call(&state, api("GET", "/api/requests", Some(&admin), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut admin_ids = item_ids(&body);
        admin_ids.sort_unstable();
        let mut expected = vec![alice_first, alice_second, bob_first];
        expected.sort_unstable();
        assert_eq!(admin_ids, expected, "admin 应看到全部：{body}");
        assert_eq!(body["total"], 3);

        // admin 按状态筛
        let (status, body) = call(
            &state,
            api("GET", "/api/requests?status=pending", Some(&admin), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 3);
        let (status, body) = call(
            &state,
            api("GET", "/api/requests?status=done", Some(&admin), None),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 0);

        // admin ?mine=1 → 只看自己提交的（admin 没提交过 → 空）
        let (status, body) = call(&state, api("GET", "/api/requests?mine=1", Some(&admin), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 0);

        // 每条都带 vote_count，且不暴露内部字段
        let (_, body) = call(&state, api("GET", "/api/requests", Some(&admin), None)).await;
        for item in body["items"].as_array().expect("items") {
            assert!(item["vote_count"].is_i64(), "每条都要带 vote_count：{item}");
            assert!(item.get("dedup_key").is_none(), "不暴露 dedup_key");
            assert!(item.get("user_id").is_none(), "不暴露 user_id");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 6. 排序
    // ─────────────────────────────────────────────────────────────────────────

    /// 排序：票数降序；票数相同按 id 升序（确定性）。
    #[tokio::test]
    async fn list_is_sorted_by_vote_count_then_id() {
        let (state, _temp) = test_state("s23-sort");
        let (_alice_id, alice) = issue_token(&state, "alice", Role::User);
        let (_bob_id, bob) = issue_token(&state, "bob", Role::User);
        let (_carol_id, carol) = issue_token(&state, "carol", Role::User);
        let (_admin_id, admin) = issue_token(&state, "root", Role::Admin);

        let (_, body) = post_request(&state, &alice, "票少", None).await;
        let few = body["request"]["id"].as_i64().expect("id");
        let (_, body) = post_request(&state, &alice, "票多", None).await;
        let most = body["request"]["id"].as_i64().expect("id");
        let (_, body) = post_request(&state, &alice, "票中", None).await;
        let middle = body["request"]["id"].as_i64().expect("id");

        // 同一个 dedup_key 再来就是投票：票多 → 3 票，票中 → 2 票，票少 → 1 票
        post_request(&state, &bob, "票多", None).await;
        post_request(&state, &carol, "票多", None).await;
        post_request(&state, &bob, "票中", None).await;

        let (status, body) = call(&state, api("GET", "/api/requests", Some(&admin), None)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            item_ids(&body),
            vec![most, middle, few],
            "按票数降序：3 / 2 / 1"
        );
        let counts: Vec<i64> = body["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["vote_count"].as_i64().expect("vote_count"))
            .collect();
        assert_eq!(counts, vec![3, 2, 1]);

        // 平票：再加一条 1 票的请求，应排在同样 1 票的 few 之后（id 升序兜底）
        let (_, body) = post_request(&state, &alice, "另一条一票", None).await;
        let also_few = body["request"]["id"].as_i64().expect("id");
        assert!(also_few > few, "后建的 id 更大");

        let (_, body) = call(&state, api("GET", "/api/requests", Some(&admin), None)).await;
        assert_eq!(
            item_ids(&body),
            vec![most, middle, few, also_few],
            "平票按 id 升序：先 few 后 also_few"
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 7. link
    // ─────────────────────────────────────────────────────────────────────────

    /// link 到不存在的歌 → 404；成功 → status == done；非 admin → 403；
    /// 终态再 link → 400。
    #[tokio::test]
    async fn link_requires_an_existing_song_and_marks_the_request_done() {
        let (state, _temp) = test_state("s23-link");
        let (_admin_id, admin) = issue_token(&state, "root", Role::Admin);
        let (_user_id, alice) = issue_token(&state, "alice", Role::User);
        let song_id = seed_song(&state, "/music/hit.mp3");

        let (_, body) = post_request(&state, &alice, "命中的歌", None).await;
        let request_id = body["request"]["id"].as_i64().expect("id");
        let uri = format!("/api/requests/{request_id}/link");

        // 普通用户 link → 403
        let (status, _) = call(
            &state,
            api("POST", &uri, Some(&alice), Some(json!({ "song_id": song_id }))),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // 缺 song_id → 400
        let (status, _) = call(&state, api("POST", &uri, Some(&admin), Some(json!({})))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 歌不存在 → 404
        let (status, body) = call(
            &state,
            api("POST", &uri, Some(&admin), Some(json!({ "song_id": 999999 }))),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "不存在的歌必须 404：{body}");
        assert_eq!(body["error"]["code"], "NOT_FOUND");

        // 请求不存在 → 404
        let (status, _) = call(
            &state,
            api(
                "POST",
                "/api/requests/999999/link",
                Some(&admin),
                Some(json!({ "song_id": song_id })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // 成功 → done + song_id
        let (status, body) = call(
            &state,
            api("POST", &uri, Some(&admin), Some(json!({ "song_id": song_id }))),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "link 应 200：{body}");
        assert_eq!(body["request"]["status"], "done");
        assert_eq!(body["request"]["song_id"], song_id);

        // 直接查库核对
        {
            let conn = state.db.acquire().expect("借连接");
            let row = requests::get(&conn, request_id)
                .expect("查请求")
                .expect("在库里");
            assert_eq!(row.status, RequestStatus::Done);
            assert_eq!(row.song_id, Some(song_id));
        }

        // 终态再 link → 400
        let (status, _) = call(
            &state,
            api("POST", &uri, Some(&admin), Some(json!({ "song_id": song_id }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "终态不能再 link");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 8. fetch
    // ─────────────────────────────────────────────────────────────────────────

    /// fetch：请求存在 → 503 + 说明未配置 **provider** 插件（不得假装成功）；
    /// 请求不存在 → 404；非 admin → 403。
    #[tokio::test]
    async fn fetch_reports_missing_provider_plugin() {
        let (state, _temp) = test_state("s23-fetch");
        let (_admin_id, admin) = issue_token(&state, "root", Role::Admin);
        let (_user_id, alice) = issue_token(&state, "alice", Role::User);
        let (_, body) = post_request(&state, &alice, "想听的歌", None).await;
        let request_id = body["request"]["id"].as_i64().expect("id");
        let uri = format!("/api/requests/{request_id}/fetch");

        // 普通用户 → 403
        let (status, _) = call(&state, api("POST", &uri, Some(&alice), None)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // 请求不存在 → 404
        let (status, _) = call(
            &state,
            api("POST", "/api/requests/999999/fetch", Some(&admin), None),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // 存在 → 503，且文案必须点名 provider（不能含糊成「未配置刮削插件」——
        // 刮削插件早已接通，含糊的文案会把排查方向带偏）
        let (status, body) = call(&state, api("POST", &uri, Some(&admin), None)).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "没有插件必须 503，绝不假装成功：{body}"
        );
        assert_eq!(body["error"]["code"], "SERVICE_UNAVAILABLE");
        let message = body["error"]["message"].as_str().unwrap_or("");
        assert!(
            message.contains("插件") && message.contains("未配置"),
            "文案要说清是缺插件：{body}"
        );
        assert!(
            message.contains("provider"),
            "缺的是 provider（下载）插件而不是 scraper，文案必须点名：{body}"
        );
        assert_eq!(message, FETCH_UNAVAILABLE_MESSAGE);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 9. 未登录
    // ─────────────────────────────────────────────────────────────────────────

    /// 六条接口（GET 两个 query 变体分开算）未登录一律 401。
    #[tokio::test]
    async fn request_routes_require_authentication() {
        let (state, _temp) = test_state("s23-auth");

        let cases: [(&str, &str, Option<Value>); 7] = [
            ("POST", "/api/requests", Some(json!({ "title": "x" }))),
            ("GET", "/api/requests", None),
            ("GET", "/api/requests?mine=1", None),
            ("GET", "/api/requests?status=pending", None),
            (
                "PATCH",
                "/api/requests/1",
                Some(json!({ "status": "processing" })),
            ),
            ("POST", "/api/requests/1/fetch", None),
            ("POST", "/api/requests/1/link", Some(json!({ "song_id": 1 }))),
        ];
        for (method, uri, body) in cases {
            let (status, response) = call(&state, api(method, uri, None, body)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} 必须 401");
            assert_eq!(response["error"]["code"], "UNAUTHORIZED");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 10. 提交校验
    // ─────────────────────────────────────────────────────────────────────────

    /// 提交校验：title 缺失 / 空 → 400；title 与 artist 归一化后都为空 → 400；
    /// note / album 超长 → 400；类型不对 → 400；artist 缺席视为 None。
    #[tokio::test]
    async fn submission_validates_title_and_lengths() {
        let (state, _temp) = test_state("s23-validate");
        let (_user_id, alice) = issue_token(&state, "alice", Role::User);

        // title 缺失 / 空串 / 类型不对 → 400
        for body in [json!({}), json!({ "title": "" }), json!({ "title": 5 })] {
            let (status, response) = call(
                &state,
                api("POST", "/api/requests", Some(&alice), Some(body.clone())),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body} 必须 400");
            assert_eq!(response["error"]["code"], "BAD_REQUEST");
        }

        // title 看着非空、归一化后为空，且 artist 也为空 → 400
        let (status, body) = call(
            &state,
            api(
                "POST",
                "/api/requests",
                Some(&alice),
                Some(json!({ "title": "　\t ", "artist": "  " })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "归一化后全空必须 400：{body}");

        // note / album 超长 → 400
        let long_note = "嗯".repeat(MAX_NOTE_CHARS + 1);
        let (status, _) = call(
            &state,
            api(
                "POST",
                "/api/requests",
                Some(&alice),
                Some(json!({ "title": "歌", "note": long_note })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "note 超长必须 400");
        let long_album = "专".repeat(MAX_ALBUM_CHARS + 1);
        let (status, _) = call(
            &state,
            api(
                "POST",
                "/api/requests",
                Some(&alice),
                Some(json!({ "title": "歌", "album": long_album })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "album 超长必须 400");

        // note 类型不对 → 400
        let (status, _) = call(
            &state,
            api(
                "POST",
                "/api/requests",
                Some(&alice),
                Some(json!({ "title": "歌", "note": 42 })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 合法：artist 缺席 + album/note 正常；字段被 trim 后落库
        let (status, body) = call(
            &state,
            api(
                "POST",
                "/api/requests",
                Some(&alice),
                Some(json!({ "title": "  只填标题  ", "note": "  麻烦快点  " })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["request"]["title"], "只填标题");
        assert_eq!(body["request"]["artist"], Value::Null);
        assert_eq!(body["request"]["note"], "麻烦快点");
        assert_eq!(body["request"]["status"], "pending");
        assert_eq!(body["request"]["song_id"], Value::Null);
    }
}
