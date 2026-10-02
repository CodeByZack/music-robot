//! 路由组装与处理器。
//!
//! S14 只搭骨架：健康检查 + 统一 404 / 405 + CORS。
//! S15 在这里接线认证：公开的注册 / 登录，以及挂鉴权中间件的 /api/auth/me 与
//! /api/admin/ping 占位。
//! S16 在这里挂曲库 API（library / songs / albums / artists / search），
//! 按画布要求全部进**受保护**子 Router。
//! S17 在这里挂扫描 / 刮削 API（触发 + 轮询 + 任务列表），同样进受保护子 Router，
//! 且全部要求 admin。
//! S18 在这里挂流式播放（GET /api/stream/:id，支持 Range），同样进受保护子 Router。
//! S20 在这里挂封面（GET /api/songs/:id/cover），同样进受保护子 Router；它的数据来源
//! 是「先读专辑表、空了回退到文件内嵌封面」，为什么必须有回退见 crate::audio::cover。
//! S22 在这里挂播放列表（/api/playlists 的 8 条 CRUD + 权限 + 排序），同样进受保护子
//! Router；「看不见的歌单 → 404、看得见但不是属主 → 403」的口径见 routes::playlists 头注释。
//! S21 在这里挂播放周边（/api/history、/api/favorites、/api/settings 共 7 条），同样进
//! 受保护子 Router；按登录用户隔离、收藏幂等与 settings 滥用上限见 routes::playback 头注释。
//! S23 在这里挂歌曲请求（/api/requests 的提交 / 列表 / 状态机 / fetch / link），同样进
//! 受保护子 Router；去重键归一化、合并投票、权限口径与状态机见 routes::requests 头注释。
//! 管理端其余路由属于 S19–S23 中尚未落地的部分，不要提前塞进来。

use axum::extract::State;
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{middleware, Json, Router};
use serde_json::json;
use tower_http::cors::{AllowOrigin, CorsLayer};

use super::error::ApiError;
use super::state::AppState;

/// S15 认证路由（注册 / 登录 / 当前用户 + admin 占位接口）。
pub mod auth;

/// S16 曲库路由（library / songs / albums / artists / search）。
///
/// 全部挂在下面的受保护子 Router 上：画布明确「所有 API 先过 JWT 鉴权」，
/// 未携带有效令牌一律 401。
pub mod library;

/// S17 扫描 / 刮削路由（触发 + 轮询 + 任务列表）。
///
/// 这五个接口**全部要求 admin**：扫描 / 刮削是重操作，普通用户不该触发。
/// 未登录 → 受保护子 Router 的 require_auth 中间件给 401；非 admin → AdminUser
/// 提取器给 403。
pub mod jobs;

/// S18 流式播放路由（GET /api/stream/{id}，支持 HTTP Range）。
///
/// 挂在受保护子 Router 上：画布要求播放接口要登录（handler 里再用 AuthUser 兜底）。
pub mod stream;

/// S20 封面路由（GET /api/songs/{id}/cover）。
///
/// 挂在受保护子 Router 上：封面与其它业务接口一致，要求登录（handler 里再用 AuthUser 兜底）。
pub mod cover;

/// S22 播放列表路由（CRUD + 权限 + 排序）。
///
/// 8 条路由全部挂在受保护子 Router 上：未登录一律 401。权限口径（**别统一成 403**）：
/// 看不见的歌单（不存在 / 别人的私有）一律 404，看得见但不是属主（别人的公开歌单）
/// 才是 403。完整理由见本模块头注释。
pub mod playlists;

/// S21 播放周边路由（history / favorites / settings）。
///
/// 7 条路由全部挂在受保护子 Router 上：未登录一律 401。三组数据**按登录用户隔离**
/// （user_id 只来自 AuthUser，绝不从请求体取），收藏幂等与 settings 的滥用上限
/// 见本模块头注释。断点续播的键约定（resume:{song_id} = 毫秒字符串）也写在那边。
pub mod playback;

/// S23 歌曲请求（点歌）路由（提交 / 列表 / 状态机 / fetch / link）。
///
/// 全部挂在受保护子 Router 上：未登录一律 401。权限口径：普通用户**只看得到自己
/// 提交的**（?status= 与全量列表 → 403），PATCH / fetch / link 三条写接口仅 admin。
/// 去重键归一化（大小写 / 空白 / 全半角）、合并投票幂等与状态机见 routes::requests
/// 模块头注释 —— 归一化是本步骤的核心，规则与理由都写在那里。
pub mod requests;

/// S26 手工编辑标签（/api/songs/{id}/tags）。
///
/// GET 读当前**文件**标签（登录即可）；PATCH 预览 / 写入标签，**仅 admin** ——
/// 它会直接覆盖原文件且不可撤销（`atomic_replace` 无备份），与扫描 / 刮削同一量级。
/// 请求体的 `dry_run` **默认为 true**，所以「预览」是默认行为，「写盘」要显式要求。
/// 字段语义（不传 = 保持 / null = 清空 / 传值 = 设置）见 routes::tags 模块头注释。
pub mod tags;

/// 组装好的路由别名（state 已经塞进去，服务与测试都直接用）。
pub type AppRouter = Router;

/// 组装 Router。
///
/// 特意**不绑定端口**：测试可以直接对返回的 Router 用 tower::ServiceExt::oneshot
/// 发请求，不必真的监听 socket；只有 crate::server::run 才做 bind + serve。
pub fn build_router(state: AppState) -> AppRouter {
    // 需要「已登录」的路由单独拼一个子 Router，把鉴权中间件只挂在这一组上。
    // 若直接对整个 Router 用 route_layer，连 /healthz、/api/auth/login 都会被要求登录。
    //
    // 中间件的职责：取 Bearer 令牌 → 验签 → 按 sub 查库（鉴权以数据库为准）→
    // 把 AuthUser 放进请求扩展。失败（缺失 / 格式错 / 过期 / 篡改 / alg:none）都是 401，
    // 响应体是 S14 的统一形状 { error: { code, message, details } }。
    let protected = Router::new()
        .route("/api/auth/me", get(auth::me))
        // S16+ 管理端路由的占位：用来验证「普通用户访问管理路由是 403」这条 UT。
        .route("/api/admin/ping", get(auth::admin_ping))
        // 建号唯一入口：`/api/auth/register` 只在库空时可用（初始化引导），
        // 之后由管理员走这里建用户。只有 admin 能调。
        .route("/api/admin/users", post(auth::admin_create_user))
        // ── S16 曲库 API ──────────────────────────────────────────────────
        // 画布：所有 API 先过 JWT。这 5 条一律放在受保护子 Router 里，由下面的
        // require_auth 中间件统一拦截；handler 里还各自提取 AuthUser 兜底，
        // 免得将来有人把路由挪去公开组时静默变成匿名可用。
        //
        // 路径参数用 axum 0.8 的花括号写法（旧写法 :id 在 matchit 0.8 下会 panic）。
        .route("/api/library", get(library::library))
        .route("/api/songs/{id}", get(library::song))
        // S26 标签编辑：GET 读当前**文件**标签（登录即可），PATCH 预览 / 写入标签。
        // PATCH 会**直接覆盖原文件**且不可撤销（atomic_replace 无备份），所以它
        // 在 handler 里用 AdminUser 提取器要求管理员，与 /api/scrape 同档；
        // 且请求体的 `dry_run` **默认为 true**（忘了传不会误写）。
        .route("/api/songs/{id}/tags", get(tags::get_tags).patch(tags::patch_tags))
        // 列表与详情并存：/api/albums 与 /api/albums/{id}、/api/artists 与 /api/artists/{name}
        .route("/api/albums", get(library::albums_list))
        .route("/api/albums/{id}", get(library::album))
        .route("/api/artists", get(library::artists_list))
        .route("/api/artists/{name}", get(library::artists))
        .route("/api/search", get(library::search))
        // ── S17 扫描 / 刮削 API ───────────────────────────────────────────
        // 画布：首次全盘扫描手动触发；刮削队列 = songs.scrape_status='pending'；
        // 进度用轮询（已定，不用 WebSocket）。五个接口**全部 admin 专属** ——
        // 扫描 / 刮削是重操作，普通用户不该触发（handler 里用 AdminUser 提取器兜底）。
        //
        // POST 触发立刻 202 + batch_id，长任务在 jobs 注册表登记的独立线程里跑；
        // 同类任务已在跑时由注册表的原子单例锁拒绝 → 409。
        .route("/api/scan", post(jobs::start_scan))
        .route("/api/scan/{batch_id}", get(jobs::scan_status))
        .route("/api/scrape", post(jobs::start_scrape))
        .route("/api/scrape/{batch_id}", get(jobs::scrape_status))
        .route("/api/jobs", get(jobs::list))
        // ── S22 播放列表 ──────────────────────────────────────────────────
        // 画布：GET/POST /api/playlists、GET/PUT/DELETE /api/playlists/:id、
        // POST/DELETE/PUT /api/playlists/:id/items[/:song_id]。8 条一律放在受保护
        // 子 Router 里，由下面的 require_auth 中间件统一拦截；handler 里还各自提取
        // AuthUser 兜底，免得将来有人把路由挪去公开组时静默变成匿名可用。
        //
        // 路径参数用 axum 0.8 的花括号写法（旧写法 :id 在 matchit 0.8 下会 panic）。
        // 权限判定（404 还是 403）只在 routes::playlists 的 load_visible / load_owned
        // 两个助手里，**不要**在这里或 handler 里另判一套。
        .route(
            "/api/playlists",
            get(playlists::list).post(playlists::create),
        )
        .route(
            "/api/playlists/{id}",
            get(playlists::detail)
                .put(playlists::update)
                .delete(playlists::delete),
        )
        .route(
            "/api/playlists/{id}/items",
            post(playlists::add_item).put(playlists::reorder_items),
        )
        .route(
            "/api/playlists/{id}/items/{song_id}",
            delete(playlists::remove_item),
        )
        // ── S21 播放周边 API ──────────────────────────────────────────────
        // 画布：POST/GET /api/history、GET/POST/DELETE /api/favorites、
        // GET/PUT /api/settings。7 条一律放在受保护子 Router 里，由下面的
        // require_auth 中间件统一拦截；handler 里还各自提取 AuthUser 兜底。
        // 历史 / 收藏 / 设置**全部按登录用户隔离**：user_id 只来自 AuthUser，
        // 绝不从请求体或查询串读取（那是越权漏洞）。
        //
        // 路径参数用 axum 0.8 的花括号写法（旧写法 :id 在 matchit 0.8 下会 panic）。
        .route(
            "/api/history",
            get(playback::recent_history).post(playback::record_history),
        )
        .route("/api/favorites", get(playback::list_favorites))
        .route(
            "/api/favorites/{song_id}",
            post(playback::add_favorite).delete(playback::remove_favorite),
        )
        .route(
            "/api/settings",
            get(playback::get_settings).put(playback::put_settings),
        )
        // ── S23 歌曲请求（点歌）API ────────────────────────────────────────
        // 画布：POST /api/requests（提交 + 去重合并）、GET /api/requests（?mine=1 我的 /
        // ?status= 按状态，后者仅 admin）、PATCH /api/requests/:id（状态机，仅 admin）、
        // POST /api/requests/:id/fetch（当前恒 503：服务端还没加载刮削插件）、
        // POST /api/requests/:id/link（关联歌曲并置 done，仅 admin）。
        //
        // 路径参数用 axum 0.8 的花括号写法（旧写法 :id 在 matchit 0.8 下会 panic）。
        // 权限判定集中在 routes::requests 的 list / AdminUser 提取器里，别在这里另判一套。
        .route(
            "/api/requests",
            get(requests::list).post(requests::submit),
        )
        .route("/api/requests/{id}", patch(requests::update))
        .route("/api/requests/{id}/fetch", post(requests::fetch))
        .route("/api/requests/{id}/link", post(requests::link))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            super::auth::require_auth,
        ));

    // ── 媒体子路由：鉴权与其余 API 不同 ──────────────────────────────────
    //
    // 这两条的数据要交给 `<audio src>` / `<img src>`，而它们发的是**浏览器自发的
    // 裸 GET，带不了 `Authorization` 头**。所以单独挂 `require_auth_media`：
    // 先试请求头，再试登录时下发的 HttpOnly cookie。
    //
    // ⚠️ 别把这两条挪回 `protected`：那样媒体就只能在 JS fetch 里用，标签全失效。
    let media = Router::new()
        .route("/api/stream/{id}", get(stream::stream))
        .route("/api/songs/{id}/cover", get(cover::cover))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            super::auth::require_auth_media,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        // 公开的认证路由：注册 / 登录不要求携带令牌。
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/login", post(auth::login))
        // 登出也不要求令牌：令牌过期后更需要能登出（cookie 是 HttpOnly，JS 删不掉）
        .route("/api/auth/logout", post(auth::logout))
        .merge(protected)
        .merge(media)
        // 未知路径与不支持的方法都回统一错误形状，而不是 axum 默认的空 body。
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(build_cors())
        .with_state(state)
}

// ─────────────────────────────────────────────────────────────────────────────
// CORS
// ─────────────────────────────────────────────────────────────────────────────

/// 构造 CORS 层。
///
/// 当前策略（开发默认）：只放行本机回环地址（localhost / 127.0.0.1 / [::1]）的
/// 任意端口，其余来源一律不加 Access-Control-Allow-Origin。
///
/// 背景：前后端分离开发时前端跑在 Vite（http://localhost:5173），与 API 不同源，
/// 浏览器会先发预检；生产若用反向代理把页面与 /api 放在同一 origin，则根本不需要 CORS。
///
/// 生产部署收紧方式（S24+ 前端与部署形态确定后再定）：
///   1. 把允许的站点 origin（如 https://music.example.com）做成配置项，这里改成
///      从 state / 配置读取的精确白名单（AllowOrigin::list），或交给反代统一处理；
///   2. 只放行真正用到的方法与请求头，不要图省事全开；
///   3. 只有用 Cookie 会话时才需要 allow_credentials(true)，而那时来源绝不能是通配。
///
/// 这里**不用 CorsLayer::permissive()**：它等于 Access-Control-Allow-Origin: *，
/// 配合将来的鉴权头会把接口暴露给任意站点发起的跨站请求。
fn build_cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(allowed_origin))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION])
}

/// 来源判定：只认本机回环的 http(s) 源（端口不限）。
fn allowed_origin(origin: &HeaderValue, _parts: &axum::http::request::Parts) -> bool {
    let Ok(text) = origin.to_str() else {
        return false;
    };
    // 依次剥掉 scheme，再按 host 前缀判断，要求 host 后紧跟端口或结束。
    ["http://", "https://"].iter().any(|scheme| {
        text.strip_prefix(scheme).is_some_and(|rest| {
            ["localhost", "127.0.0.1", "[::1]"].iter().any(|host| {
                rest.strip_prefix(host)
                    .is_some_and(|tail| tail.is_empty() || tail.starts_with(':'))
            })
        })
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// handlers
// ─────────────────────────────────────────────────────────────────────────────

/// GET /healthz —— 健康检查。
///
/// 顺带探一次数据库（SELECT 1）：探到故障返回 503，状态码如实体现依赖状态，
/// 而不是永远 200。探活会阻塞，所以包在 spawn_blocking 里。
///
/// 成功时另外带上**插件加载概况**：刮削没工作时，「是没插件、还是插件全坏了」必须能
/// 一眼看到，而不是去翻启动日志（见 AppState::plugin_report 的注释）。
///
/// ⚠️ 本端点**免鉴权**（见 build_router 的说明），所以只给加载成功的**文件名**与跳过
/// **数量**，**不给跳过的原因** —— 那些原因里拼了服务器绝对路径（建工作目录失败 /
/// 读目录失败都带 path），不该出现在公开端点上。要看原因翻启动 stderr。
async fn healthz(State(state): State<AppState>) -> Response {
    // 先把概况拷出来：下面 state 会被 move 进 spawn_blocking。
    let loaded = state.plugin_report.loaded.clone();
    let skipped_count = state.plugin_report.skipped_count();

    let probed = tokio::task::spawn_blocking(move || state.probe_db()).await;
    match probed {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(json!({
                "status": "ok",
                "plugins": { "loaded": loaded, "skipped_count": skipped_count },
            })),
        )
            .into_response(),
        Ok(Err(e)) => e.into_response(),
        // 只有阻塞任务 panic 才会走到这里；生产路径没有 panic，但 join 错误必须处理。
        Err(join) => ApiError::internal(format!("健康检查任务异常退出：{join}")).into_response(),
    }
}

/// 未知路径：统一 404 形状。
async fn not_found(uri: Uri) -> ApiError {
    ApiError::not_found(format!("请求的接口不存在: {}", uri.path()))
}

/// 路径存在但方法不支持：统一 405 形状（axum 默认是空 body）。
async fn method_not_allowed(method: Method, uri: Uri) -> ApiError {
    ApiError::method_not_allowed(format!("不支持用 {method} 请求: {}", uri.path()))
}
