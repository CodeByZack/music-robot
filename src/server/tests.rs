//! S14 · server 骨架测试。
//!
//! 除「启动成功」真绑一次 127.0.0.1:0（内核分配端口）外，其余全部用
//! tower::ServiceExt::oneshot 直调 build_router，不占端口、不起真实监听。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use tower::ServiceExt;

use crate::config::Config;
use crate::db::pool::{DbPool, TempDb};
use crate::watcher::test_support::TempDir;

use super::error::ApiError;
use super::routes::build_router;
use super::state::AppState;

/// 建一个临时文件库的状态。健康检查只跑 SELECT 1，不需要跑迁移。
/// 返回 TempDb 让调用方持有，测试结束才删文件，避免池里的连接指向已删除路径。
fn test_state() -> (AppState, TempDb) {
    let (pool, temp) = DbPool::open_temp("server-s14").expect("建临时文件库池");
    let mut cfg = Config::defaults();
    cfg.storage.library_roots = vec!["/tmp/server-s14-music".to_string()];
    (AppState::new(Arc::new(pool), Arc::new(cfg)), temp)
}

/// 读响应体并按 JSON 解析。
async fn body_json(resp: Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("读响应体");
    serde_json::from_slice(&bytes).expect("响应体必须是合法 JSON")
}

/// 断言一段文案含中文（CJK 统一汉字区任一字符即可）。
fn assert_has_chinese(text: &str, what: &str) {
    let has = text
        .chars()
        .any(|c| (c as u32) >= 0x4E00 && (c as u32) <= 0x9FFF);
    assert!(has, "{what} 必须是中文，实际是：{text}");
}

/// 构造一个简单的 GET 请求。
fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("构造请求")
}

fn has_cjk(text: &str) -> bool {
    text.chars()
        .any(|c| (c as u32) >= 0x4E00 && (c as u32) <= 0x9FFF)
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. 健康检查
// ─────────────────────────────────────────────────────────────────────────────

/// 验证：GET /healthz 返回 200 + JSON { status: ok }（临时库探活能过）。
#[tokio::test]
async fn healthz_returns_ok() {
    let (state, _temp) = test_state();
    let resp = build_router(state)
        .oneshot(get("/healthz"))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["status"], "ok");
}

/// 验证：/healthz 带上插件加载概况（加载成功的文件名 + 跳过数量），
/// 且**绝不带跳过原因** —— 本端点免鉴权，原因里拼了服务器绝对路径。
///
/// 这条用例同时是「插件报告有没有真的连到出口」的闸：只把报告塞进 AppState 而不暴露，
/// 刮削没工作时用户仍然只能去翻启动日志。
#[tokio::test]
async fn healthz_exposes_plugin_summary_without_paths() {
    let dir = TempDir::new("healthz-plugins");
    let plugins_dir = dir.path().join("plugins");
    write_scraper(&plugins_dir, "a.js", "a");
    std::fs::write(plugins_dir.join("broken.js"), "console.log('no manifest');\n").expect("写坏插件");
    let leaky_path = plugins_dir.to_string_lossy().into_owned();

    let (pool, _temp) = DbPool::open_temp("healthz-plugins").expect("建临时文件库池");
    let mut cfg = Config::defaults();
    cfg.storage.library_roots = vec!["/tmp/healthz-plugins-music".to_string()];
    cfg.plugins.dir = leaky_path.clone();
    let state = AppState::new(Arc::new(pool), Arc::new(cfg));

    let resp = build_router(state)
        .oneshot(get("/healthz"))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;

    assert_eq!(v["status"], "ok");
    assert_eq!(
        v["plugins"]["loaded"].as_array().map(Vec::len),
        Some(1),
        "加载成功的插件文件名要能看到：{v}"
    );
    assert_eq!(v["plugins"]["loaded"][0], "a.js");
    assert_eq!(v["plugins"]["skipped_count"], 1, "跳过数量要能看到：{v}");

    // 免鉴权端点不得泄漏服务器路径。
    let raw = v.to_string();
    assert!(
        !raw.contains(&leaky_path),
        "公开端点不得出现插件目录的绝对路径：{raw}"
    );
    assert!(
        !raw.contains("broken.js"),
        "跳过的文件名与原因都不该出现在公开端点：{raw}"
    );
}

/// 验证：数据库探活失败时状态码如实变成 503，且是统一错误形状（不会永远 200 ok）。
#[tokio::test]
async fn healthz_reports_503_when_db_unavailable() {
    let (state, _temp) = test_state();
    // 关掉连接池，之后的 acquire 会返回 DbPoolError::Closed，模拟数据库不可用。
    state.db.close();
    let resp = build_router(state)
        .oneshot(get("/healthz"))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "SERVICE_UNAVAILABLE");
    let msg = v["error"]["message"].as_str().expect("message 是字符串");
    assert_has_chinese(msg, "503 的 message");
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. 未知路由 404（必须是统一错误格式，而不是 axum 默认空 body）
// ─────────────────────────────────────────────────────────────────────────────

/// 验证：不存在的路径返回 404，且 body 是 { error: { code, message, details } }。
#[tokio::test]
async fn unknown_route_returns_unified_404() {
    let (state, _temp) = test_state();
    let resp = build_router(state)
        .oneshot(get("/no/such/route"))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let v = body_json(resp).await;

    // 顶层只有 error 一个键
    assert_eq!(v.as_object().expect("顶层是对象").len(), 1);
    assert_eq!(v["error"]["code"], "NOT_FOUND");
    let msg = v["error"]["message"].as_str().expect("message 是字符串");
    assert_has_chinese(msg, "404 的 message");
    assert!(v["error"].get("details").is_some(), "统一形状必须始终带 details 字段");
}

/// 验证：路径存在但方法不支持时也是统一错误形状（405）。
#[tokio::test]
async fn unsupported_method_returns_unified_405() {
    let (state, _temp) = test_state();
    let req = Request::builder()
        .method("POST")
        .uri("/healthz")
        .body(Body::empty())
        .expect("构造请求");
    let resp = build_router(state).oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "METHOD_NOT_ALLOWED");
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. 错误响应格式（严格形状 + 不泄漏内部细节）
// ─────────────────────────────────────────────────────────────────────────────

/// 验证：ApiError 的响应体形状严格是 { error: { code, message, details } }，
/// message 是中文，且内部错误的原因（含内部路径）一个字都不出现在响应体里。
#[tokio::test]
async fn internal_error_shape_is_strict_and_hides_details() {
    const SECRET: &str = "/vol1/private/music/library.db";
    let err = ApiError::internal(format!(
        "打开数据库失败：{SECRET}（rusqlite: disk I/O error）",
    ));
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let v = body_json(resp).await;

    // 形状：顶层只有 error，error 只有 code / message / details 三个键
    assert_eq!(v.as_object().expect("顶层是对象").len(), 1);
    let error = v["error"].as_object().expect("error 是对象");
    let mut keys: Vec<&str> = error.keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["code", "details", "message"]);

    assert_eq!(v["error"]["code"], "INTERNAL");
    assert!(v["error"]["details"].is_null(), "内部错误的 details 必须是 null");
    let msg = v["error"]["message"].as_str().expect("message 是字符串");
    assert_has_chinese(msg, "INTERNAL 的 message");

    // 不泄漏：响应体里不能出现内部路径或底层报错
    let raw = v.to_string();
    assert!(!raw.contains(SECRET), "响应体泄漏了内部路径：{raw}");
    assert!(!raw.contains("rusqlite"), "响应体泄漏了底层依赖名：{raw}");
    assert!(!raw.contains("disk I/O"), "响应体泄漏了底层报错：{raw}");
}

/// 验证：4xx 业务错误同样是统一形状，message 也是中文。
#[tokio::test]
async fn business_error_shapes_are_unified() {
    let cases = vec![
        (ApiError::not_found("请求的曲目不存在"), StatusCode::NOT_FOUND, "NOT_FOUND"),
        (ApiError::bad_request("参数不合法"), StatusCode::BAD_REQUEST, "BAD_REQUEST"),
        (
            ApiError::conflict("相同记录已存在，请勿重复提交"),
            StatusCode::CONFLICT,
            "CONFLICT",
        ),
    ];
    for (err, status, code) in cases {
        let resp = err.into_response();
        assert_eq!(resp.status(), status);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], code);
        assert!(v["error"].get("details").is_some());
        let msg = v["error"]["message"].as_str().expect("message 是字符串");
        assert_has_chinese(msg, code);
        assert!(!msg.is_empty());
    }
}

/// 验证：已存在的错误类型经 From 转换后口径正确（业务分支 4xx、内部故障 500）。
#[tokio::test]
async fn existing_errors_map_into_api_error() {
    // 刮削：曲目不在库是正常业务分支
    let e = ApiError::from(crate::service::ScrapeError::SongNotFound { id: 7 });
    assert_eq!(e.status(), StatusCode::NOT_FOUND);
    assert_eq!(e.code(), "NOT_FOUND");
    assert!(has_cjk(e.message()), "message 必须是中文");

    // 仓库：唯一冲突是正常业务分支
    let e = ApiError::from(crate::db::repos::RepoError::Conflict {
        constraint: "songs.audio_hash".to_string(),
    });
    assert_eq!(e.status(), StatusCode::CONFLICT);
    assert!(has_cjk(e.message()), "message 必须是中文");

    // 仓库：写后读一致性被破坏属于内部故障，细节只进日志
    let e = ApiError::from(crate::db::repos::RepoError::Invariant {
        message: "songs 表缺列 opus_bitrate".to_string(),
    });
    assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(e.code(), "INTERNAL");
    let raw = e.body().to_string();
    assert!(has_cjk(e.message()), "message 必须是中文");
    assert!(!raw.contains("opus_bitrate"), "内部细节不该进响应体：{raw}");

    // 连接池：内部故障
    let e = ApiError::from(crate::db::pool::DbPoolError::Closed);
    assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(e.code(), "INTERNAL");
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. CORS 预检
// ─────────────────────────────────────────────────────────────────────────────

/// 发带 Origin + Access-Control-Request-Method 的 OPTIONS 预检请求。
fn preflight(uri: &str, origin: &str) -> Request<Body> {
    Request::builder()
        .method("OPTIONS")
        .uri(uri)
        .header(header::ORIGIN, origin)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())
        .expect("构造预检请求")
}

/// 验证：本机回环来源的预检成功，并回 Access-Control-Allow-Origin / -Methods。
#[tokio::test]
async fn cors_preflight_allows_local_dev_origin() {
    let (state, _temp) = test_state();
    let resp = build_router(state)
        .oneshot(preflight("/healthz", "http://localhost:5173"))
        .await
        .expect("oneshot");
    assert!(resp.status().is_success(), "预检应当成功：{}", resp.status());

    let allow_origin = resp
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .expect("缺少 Access-Control-Allow-Origin")
        .to_str()
        .expect("头不是合法字符串");
    assert_eq!(allow_origin, "http://localhost:5173");
    assert!(
        resp.headers().contains_key(header::ACCESS_CONTROL_ALLOW_METHODS),
        "预检必须回 Access-Control-Allow-Methods"
    );
}

/// 验证：非回环来源不放行（证明没有图省事写成 permissive）。
#[tokio::test]
async fn cors_rejects_foreign_origin() {
    let (state, _temp) = test_state();
    let resp = build_router(state)
        .oneshot(preflight("/healthz", "https://evil.example.com"))
        .await
        .expect("oneshot");
    assert!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "非白名单来源不该拿到 Access-Control-Allow-Origin"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. 插件接线（AppState 启动时扫 plugins.dir，并把报告存进状态）
// ─────────────────────────────────────────────────────────────────────────────

/// 造一个带合法清单的 scraper 插件文件。
fn write_scraper(dir: &std::path::Path, file: &str, name: &str) {
    let text = format!(
        "// @music-robot\n// {{\"name\":\"{name}\",\"kind\":\"scraper\",\"protocol\":1}}\n// @end\n"
    );
    std::fs::create_dir_all(dir).expect("建插件目录");
    std::fs::write(dir.join(file), text).expect("写插件文件");
}

/// 验证：AppState 构造时真的按 `plugins.dir` 扫了目录，报告与目录实际情况一致
/// （两个 scraper 按文件名升序加载、一个清单缺失被跳过且原因是中文）。
///
/// 这条用例就是「刮削是不是活的」的总闸：没有它，`Vec::new()` 回归了也没人发现。
#[tokio::test]
async fn app_state_exposes_plugin_load_report() {
    let dir = TempDir::new("plugin-wiring");
    let plugins_dir = dir.path().join("plugins");
    write_scraper(&plugins_dir, "b.js", "b");
    write_scraper(&plugins_dir, "a.js", "a");
    std::fs::write(plugins_dir.join("broken.js"), "console.log('no manifest');\n")
        .expect("写坏插件");

    let (pool, _temp) = DbPool::open_temp("server-plugin-wiring").expect("建临时文件库池");
    let mut cfg = Config::defaults();
    cfg.storage.library_roots = vec!["/tmp/server-plugin-wiring-music".to_string()];
    cfg.plugins.dir = plugins_dir.to_string_lossy().into_owned();
    let state = AppState::new(Arc::new(pool), Arc::new(cfg));

    assert_eq!(
        state.plugin_report.loaded,
        vec!["a.js", "b.js"],
        "加载报告必须与目录实际情况一致，且按文件名升序"
    );
    assert_eq!(state.plugin_report.skipped.len(), 1, "坏插件必须被记进 skipped");
    assert_eq!(state.plugin_report.skipped[0].0, "broken.js");
    assert!(
        has_cjk(&state.plugin_report.skipped[0].1),
        "跳过原因必须是中文：{}",
        state.plugin_report.skipped[0].1
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. 启动成功（真绑一次端口）
// ─────────────────────────────────────────────────────────────────────────────

/// 验证：TcpListener::bind("127.0.0.1:0") 能拿到内核分配的真实端口，axum::serve
/// 真的在跑（用 std TcpStream 手写一个最小 HTTP/1.1 请求连上去拿 200），最后优雅关闭。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_binds_ephemeral_port_and_serves_healthz() {
    let (state, _temp) = test_state();
    let app = build_router(state);

    // 端口写 0，由内核分配，测试之间不会互撞。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定 127.0.0.1:0");
    let addr = listener.local_addr().expect("取实际监听地址");
    assert_ne!(addr.port(), 0, "内核必须分配一个真实端口");

    // 停机信号：tokio 只开了 net/time/signal，没有 sync 特性，所以不用 oneshot channel，
    // 改用 std mpsc + spawn_blocking 充当一个可等待的 future。
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = stop_rx.recv();
                })
                .await;
            })
            .await
    });

    // 真连一次：用阻塞 std TcpStream 写在 blocking 池里，不占 async 工作线程。
    let raw = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(addr).expect("连接服务器");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("设置读超时");
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("发送请求");
        let mut buf = String::new();
        stream.read_to_string(&mut buf).expect("读取响应");
        buf
    })
    .await
    .expect("HTTP 客户端任务");

    assert!(raw.starts_with("HTTP/1.1 200"), "响应首行不是 200：{raw}");
    assert!(raw.contains("\"status\":\"ok\""), "响应体缺少 status=ok：{raw}");

    // 优雅关闭：发停机信号，等 serve 正常返回。
    stop_tx.send(()).expect("发送停机信号");
    let joined = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("serve 未在 5 秒内退出")
        .expect("serve 任务 panic");
    joined.expect("serve 返回错误");
}

// ─────────────────────────────────────────────────────────────────────────────
// 媒体端点鉴权（cookie 通道）
//
// 背景：`/api/stream/{id}` 与 `/api/songs/{id}/cover` 的数据要交给
// `<audio src>` / `<img src>`，而它们发的是**浏览器自发的裸 GET，带不了
// `Authorization` 头**。所以这两条单独挂 `require_auth_media`，额外接受登录时
// 下发的 HttpOnly cookie。
//
// 这一组测试钉住三件事：cookie 能用、**cookie 不能变成通用凭据**、请求头照旧能用。
// ─────────────────────────────────────────────────────────────────────────────

mod media_auth {
    use super::*;
    use crate::db::models::{Role, User};
    use crate::server::auth::{clear_media_cookie, media_cookie, sign_token_with_ttl, MEDIA_COOKIE};

    const SECRET: &str = "media-auth-test-secret";

    /// 带非空 jwt_secret 的 state —— `test_state()` 用的是空密钥，
    /// 那样签发不出可用令牌（`ensure_secret` 会直接报错）。
    fn media_state() -> (AppState, TempDb) {
        let (pool, temp) = DbPool::open_temp("server-media-auth").expect("建临时库池");
        // ⚠️ open_temp 不建表，必须自己 apply —— 少这一步会得到
        //    「内部错误：no such table: users」，且看起来像业务 bug
        {
            let mut guard = pool.acquire().expect("借连接");
            crate::db::migrations::apply(&mut guard).expect("应用迁移");
        }
        let mut cfg = Config::defaults();
        cfg.server.jwt_secret = SECRET.to_string();
        cfg.storage.library_roots = vec!["/tmp/server-media-auth-music".to_string()];
        (AppState::new(Arc::new(pool), Arc::new(cfg)), temp)
    }

    /// 预置一个用户并签一个有效令牌。
    fn seed_token(state: &AppState) -> String {
        let conn = state.db.acquire().expect("借连接");
        conn.execute(
            "INSERT INTO users (username, password_hash, role, created_at)
             VALUES ('media-user', 'hash', 'user', ?1)",
            rusqlite::params![crate::db::now_unix_ms()],
        )
        .expect("预置用户");
        let id = conn.last_insert_rowid();
        drop(conn);
        let user = User {
            id,
            username: "media-user".to_string(),
            password_hash: "hash".to_string(),
            role: Role::User,
            created_at: 0,
            last_login: None,
        };
        sign_token_with_ttl(SECRET, &user, 3600).expect("签发令牌")
    }

    async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Value, String) {
        let resp = build_router(state.clone()).oneshot(req).await.expect("oneshot");
        let status = resp.status();
        let set_cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        (status, body_json(resp).await, set_cookie)
    }

    fn req_with(uri: &str, header_name: &str, header_value: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header(header_name, header_value)
            .body(Body::empty())
            .expect("构造请求")
    }

    /// 未带任何凭据 → 401。
    #[tokio::test]
    async fn media_route_rejects_without_any_credential() {
        let (state, _temp) = media_state();
        let (status, body, _) = call(&state, get("/api/stream/1")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_has_chinese(body["error"]["message"].as_str().unwrap_or(""), "401 文案");
    }

    /// 只带 cookie → 鉴权通过（歌不存在所以是 404，不是 401 —— 这就是证据）。
    #[tokio::test]
    async fn media_route_accepts_the_login_cookie() {
        let (state, _temp) = media_state();
        let token = seed_token(&state);
        let cookie = format!("{MEDIA_COOKIE}={token}");
        let (status, _, _) = call(&state, req_with("/api/stream/1", header::COOKIE.as_str(), &cookie)).await;
        assert_ne!(status, StatusCode::UNAUTHORIZED, "带 cookie 不该 401");
        assert_eq!(status, StatusCode::NOT_FOUND, "曲目不存在应当是 404（证明鉴权已过）");

        let (cover_status, _, _) =
            call(&state, req_with("/api/songs/1/cover", header::COOKIE.as_str(), &cookie)).await;
        assert_ne!(cover_status, StatusCode::UNAUTHORIZED, "封面同样要走得通");
    }

    /// 伪造 / 乱写的 cookie → 401（别把 cookie 当免检通道）。
    #[tokio::test]
    async fn media_route_rejects_a_bad_cookie() {
        let (state, _temp) = media_state();
        for bad in [
            format!("{MEDIA_COOKIE}=not-a-jwt"),
            format!("{MEDIA_COOKIE}="),
            "other=1; another=2".to_string(),
        ] {
            let (status, _, _) = call(&state, req_with("/api/stream/1", header::COOKIE.as_str(), &bad)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "cookie「{bad}」不该放行");
        }
    }

    /// 请求头通道照旧可用（老客户端 / 桌面端不受影响）。
    #[tokio::test]
    async fn media_route_still_accepts_the_bearer_header() {
        let (state, _temp) = media_state();
        let token = seed_token(&state);
        let auth = format!("Bearer {token}");
        let (status, _, _) = call(&state, req_with("/api/stream/1", header::AUTHORIZATION.as_str(), &auth)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "带请求头也应当走通到 404");
    }

    /// ⭐ **本组最重要的一条**：cookie **不是**通用凭据。
    ///
    /// 它只在媒体子路由上被接受；其余 API 仍然只认 `Authorization` 头。
    /// 否则浏览器自动携带 cookie 就等于把 CSRF 面全打开了。
    #[tokio::test]
    async fn cookie_is_not_a_general_credential() {
        let (state, _temp) = media_state();
        let token = seed_token(&state);
        let cookie = format!("{MEDIA_COOKIE}={token}");

        for uri in ["/api/library", "/api/auth/me", "/api/favorites", "/api/jobs"] {
            let (status, _, _) = call(&state, req_with(uri, header::COOKIE.as_str(), &cookie)).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{uri} 不该认 cookie —— 认了就等于开了 CSRF 面"
            );
        }
    }

    /// 登录下发 cookie，且三个安全属性一个都不能少。
    ///
    /// 注意流程：`register` **不返回令牌**（只回 user），客户端注册后要再调一次 `login`。
    /// 所以 cookie 只由 `login` 下发 —— 这条测试也顺带把这个约定钉住了。
    #[tokio::test]
    async fn login_sets_the_media_cookie_with_all_security_attributes() {
        let (state, _temp) = media_state();
        let creds = serde_json::json!({
            "username": "cookie-user",
            "password": "verysecret123",
        });
        let post = |uri: &'static str| {
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&creds).expect("序列化")))
                .expect("构造请求")
        };

        let (registered, _, register_cookie) = call(&state, post("/api/auth/register")).await;
        assert_eq!(registered, StatusCode::CREATED, "首个用户注册应当成功");
        assert!(
            register_cookie.is_empty(),
            "register 不发令牌，也就不该下发媒体 cookie：{register_cookie}"
        );

        let (status, body, set_cookie) = call(&state, post("/api/auth/login")).await;
        assert_eq!(status, StatusCode::OK, "登录应当成功");
        assert!(body["token"].is_string(), "登录要回令牌");

        assert!(
            set_cookie.starts_with(&format!("{MEDIA_COOKIE}=")),
            "没下发媒体 cookie：{set_cookie}"
        );
        for attr in ["HttpOnly", "SameSite=Lax", "Path=/api"] {
            assert!(set_cookie.contains(attr), "cookie 少了 {attr}：{set_cookie}");
        }
    }

    /// 登出清 cookie。用 Max-Age=0（删 cookie 的标准写法），不是只发个空值。
    #[tokio::test]
    async fn logout_clears_the_media_cookie() {
        let (state, _temp) = media_state();
        let req = Request::builder()
            .method("POST")
            .uri("/api/auth/logout")
            .body(Body::empty())
            .expect("构造请求");
        let (status, _, set_cookie) = call(&state, req).await;
        assert_eq!(status, StatusCode::OK, "登出不该要求令牌（过期后更需要能登出）");
        assert!(set_cookie.contains("Max-Age=0"), "没清 cookie：{set_cookie}");
    }

    /// 两个构造函数本身的形状（纯函数，顺手钉住属性顺序与取值）。
    #[test]
    fn cookie_builders_carry_the_expected_attributes() {
        let c = media_cookie("abc.def.ghi", 86400);
        assert_eq!(
            c,
            "mr_media=abc.def.ghi; HttpOnly; SameSite=Lax; Path=/api; Max-Age=86400"
        );
        assert!(clear_media_cookie().ends_with("Max-Age=0"));
    }
}
