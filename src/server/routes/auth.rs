//! S15 · 认证路由：注册 / 登录 / 当前用户 + admin 占位接口。
//!
//! 路径（画布未规定，本步骤定下）：
//!
//! * `POST /api/auth/register` —— **初始化引导**：只在库空时可用，首个用户成为
//!   admin（见下）；之后一律 403，建号改走 `POST /api/admin/users`；
//! * `POST /api/auth/login`    —— 登录，返回 Bearer token；
//! * `GET  /api/auth/me`       —— 返回当前登录用户（验证中间件真的生效）；
//! * `POST /api/admin/users`   —— 管理员建号（注册关闭后的唯一入口），仅 admin；
//! * `GET  /api/admin/ping`    —— 仅 admin 可访问的**占位**接口（见下方注释）。
//!
//! 响应体一律手写 [`serde_json::Value`]（项目规范：不引 serde derive）。
//!
//! ## 为什么注册只在库空时可用
//!
//! 全新部署时库里没有任何用户，如果注册出来的都是普通用户，就没人能进管理端。
//! 因此「当前用户表为空 → 第一个注册者成为 admin」。
//!
//! 但这同时把「谁先注册」变成了「谁能拿到管理员权限」—— 服务一旦先在公网可见，
//! 攻击者可以抢在部署方之前注册。所以注册**只承担初始化引导这一次**：
//! 库非空即 403，之后建号必须由管理员发起（`POST /api/admin/users`）。
//! 这与 Navidrome（首个 admin 在 UI 引导，其余用户由 `navidrome user create` 建）
//! 和 Jellyfin（管理员添加用户）的做法一致。
//!
//! 早期版本曾把注册一直开着，理由是「单机自部署的开箱体验」；现在由引导 +
//! 管理端建号覆盖同一个体验，且不留口子。

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::TransactionBehavior;
use serde_json::{json, Value};

use crate::db::models::{Role, User};
use crate::db::repos::users;
use crate::server::auth::{
    dummy_password_hash, hash_password, sign_token, verify_password, AdminUser, AuthUser,
};
use crate::server::error::{ApiError, ApiResult};
use crate::server::state::AppState;

/// 登录失败对外统一的文案。
///
/// **用户不存在**与**密码错误**必须返回同一条响应（状态码 + body 完全一致），
/// 否则攻击者可以靠响应差异枚举出哪些用户名存在。
const LOGIN_FAILED_MESSAGE: &str = "用户名或密码错误";

/// 库非空时再调 `/api/auth/register` 的对外文案。
///
/// 注册是**初始化引导**（bootstrap），不是开放注册：首个管理员建出来后即关闭，
/// 之后由管理员走 `POST /api/admin/users` 建号。这与 Navidrome / Jellyfin 一致
/// （首个管理员引导出来，其余用户由管理员创建）。
const REGISTER_CLOSED_MESSAGE: &str =
    "注册已关闭：管理员已存在，请由管理员在管理端创建用户";

/// 口令最少字符数（按 Unicode 字符计，不是字节）。
const MIN_PASSWORD_CHARS: usize = 8;

/// 用户名最大字符数（防止超长字符串写库 / 撑爆日志）。
const MAX_USERNAME_CHARS: usize = 64;

/// 把用户序列化成对外 JSON。**绝不包含 password_hash**。
fn user_json(user: &User) -> Value {
    json!({
        "id": user.id,
        "username": user.username,
        "role": user.role.as_str(),
        "created_at": user.created_at,
        "last_login": user.last_login,
    })
}

/// 读取请求体里的 username / password（都必须是字符串）。
fn read_credentials(payload: &Value) -> Result<(String, String), ApiError> {
    let username = payload
        .get("username")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("请求体缺少字符串字段 username"))?;
    let password = payload
        .get("password")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("请求体缺少字符串字段 password"))?;
    Ok((username.to_string(), password.to_string()))
}

/// 用户名基本规范化：去掉首尾空白，拒绝空 / 超长 / 含控制字符。
///
/// 只 trim 与拒绝非法字符，**不改大小写**（SQLite 的 UNIQUE 默认区分大小写，
/// 改成小写反而会让用户以为 a 与 A 是同一个账号）。
fn normalize_username(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("用户名不能为空"));
    }
    if name.chars().count() > MAX_USERNAME_CHARS {
        return Err(ApiError::bad_request(format!(
            "用户名最多 {MAX_USERNAME_CHARS} 个字符"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiError::bad_request("用户名不能包含控制字符"));
    }
    Ok(name.to_string())
}

/// 口令强度校验：至少 8 个字符，且不能全是空白。
///
/// 这里只做**最低限度**的检查：长度与空白。复杂度规则（大小写 / 符号）会诱导用户
/// 用可预测的变形（Password1!），对离线爆破帮助有限，真正的防线是 argon2 的慢哈希
/// 与登录限流（S16+）。错误信息里**不回显口令本身**。
fn validate_password(password: &str) -> Result<(), ApiError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(ApiError::bad_request(format!(
            "口令至少需要 {MIN_PASSWORD_CHARS} 个字符"
        )));
    }
    if password.trim().is_empty() {
        return Err(ApiError::bad_request("口令不能全部是空白字符"));
    }
    Ok(())
}

/// POST /api/auth/register —— 注册。
///
/// 成功返回 201 + 新用户（**不返回 token**，前端拿它去登录）。
/// 用户名重复映射成 409（RepoError::Conflict → ApiError::conflict）。
///
/// argon2 是慢 KDF，计算哈希与写库都放在 spawn_blocking 里（S14 铁律）。
///
/// 建号的共用路径：算哈希 → IMMEDIATE 事务 → 定角色 → 插入 → 读回。
///
/// `pick_role` 拿到「库里已有多少用户」，返回该给什么角色（或拒绝）：
/// 引导注册靠它做「0 个 → admin，否则 403」，管理端建号则直接给固定角色。
///
/// ⚠️ **数数与插入必须在同一个 IMMEDIATE 事务里** —— 否则两个并发请求会同时读到 0，
/// 双双变成 admin。
async fn create_user(
    state: &AppState,
    username: String,
    password: String,
    pick_role: impl FnOnce(i64) -> Result<Role, ApiError> + Send + 'static,
) -> Result<User, ApiError> {
    let db = Arc::clone(&state.db);
    tokio::task::spawn_blocking(move || -> Result<User, ApiError> {
        // 先算哈希（几十毫秒的 CPU 密集操作），再借连接，缩短占着池的时间。
        let password_hash = hash_password(&password)?;
        let mut conn = db.acquire()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = users::count(&tx)?;
        let role = pick_role(existing)?;
        let new_user = User {
            id: 0,
            username,
            password_hash,
            role,
            created_at: 0,
            last_login: None,
        };
        let id = users::insert(&tx, &new_user)?;
        // 读回数据库里的那一行，保证返回的 role / created_at / id 与库一致。
        let stored = users::get(&tx, id)?
            .ok_or_else(|| ApiError::internal("建号后读不回新用户"))?;
        tx.commit()?;
        Ok(stored)
    })
    .await
    .map_err(|join| ApiError::internal(format!("建号任务异常退出：{join}")))?
}

/// 把新用户包成 201 响应，并打一行日志（**只说用户名与角色，绝不打印口令或哈希**）。
fn created_response(who: &str, created: &User) -> Response {
    eprintln!(
        "[server] {}：{}（角色 {}）",
        who,
        created.username,
        created.role.as_str()
    );
    (
        StatusCode::CREATED,
        Json(json!({ "user": user_json(created) })),
    )
        .into_response()
}

/// `POST /api/auth/register` —— **初始化引导**：只在库里一个用户都没有时可用。
///
/// 首个用户成为 admin；此后本接口一律 403（[`REGISTER_CLOSED_MESSAGE`]），
/// 建号改走 `POST /api/admin/users`。
///
/// ⚠️ **注册也要检查密钥是否已配置**。注册本身不需要密钥，但不拦就会给
/// 「首个注册用户自动成为 admin」留出一个提权窗口：运维漏配 MR_JWT_SECRET 时
/// 有人抢先注册，等运维补上密钥后那个人直接就是管理员。
/// 认证子系统没配好，注册 / 登录一起失败关闭。
pub async fn register(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> ApiResult<Response> {
    crate::server::auth::ensure_secret(&state.config.server.jwt_secret)?;
    let (raw_username, password) = read_credentials(&payload)?;
    let username = normalize_username(&raw_username)?;
    validate_password(&password)?;

    let created = create_user(&state, username, password, |existing| {
        if existing == 0 {
            Ok(Role::Admin)
        } else {
            Err(ApiError::forbidden(REGISTER_CLOSED_MESSAGE))
        }
    })
    .await?;

    Ok(created_response("首个管理员注册", &created))
}

/// `POST /api/admin/users` —— 管理员建号（注册关闭后**唯一**的建号入口）。
///
/// 请求体 `{"username": "...", "password": "...", "role": "user"|"admin"}`；
/// `role` 省略或为 `null` 即 `user`。只有 admin 能调用（非 admin 403、未登录 401）。
pub async fn admin_create_user(
    State(state): State<AppState>,
    AdminUser(_admin): AdminUser,
    Json(payload): Json<Value>,
) -> ApiResult<Response> {
    let (raw_username, password) = read_credentials(&payload)?;
    let username = normalize_username(&raw_username)?;
    validate_password(&password)?;
    let role = match payload.get("role") {
        None | Some(Value::Null) => Role::User,
        Some(Value::String(s)) if s == "user" => Role::User,
        Some(Value::String(s)) if s == "admin" => Role::Admin,
        Some(_) => {
            return Err(ApiError::bad_request(
                "role 只能是 \"user\" 或 \"admin\"",
            ))
        }
    };

    // 管理端建号不看「库里已有多少用户」，角色由调用方定。
    let created = create_user(&state, username, password, move |_existing| Ok(role)).await?;

    Ok(created_response("管理员创建用户", &created))
}

/// POST /api/auth/login —— 登录，成功返回 Bearer token。
///
/// 失败（用户不存在 / 密码错误）统一 401 + [`LOGIN_FAILED_MESSAGE`]，不做区分。
/// 校验口令与查库都在 spawn_blocking 里（慢 KDF + 同步 SQLite）。
pub async fn login(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> ApiResult<Response> {
    // ⚠️ 密钥检查必须放在**最前面**，早于任何用户查询。
    // 若放到最后（签发前），空密钥时会变成「用户存在 → 500、不存在 → 401」，
    // 响应又能区分用户名是否存在了 —— 正好把「不泄漏存在性」这条破掉。
    // 配置错误是全局性的，不该取决于请求内容。
    crate::server::auth::ensure_secret(&state.config.server.jwt_secret)?;
    let (raw_username, password) = read_credentials(&payload)?;
    let username = normalize_username(&raw_username)?;

    let db = Arc::clone(&state.db);
    let found = tokio::task::spawn_blocking(move || -> Result<Option<User>, ApiError> {
        let conn = db.acquire()?;
        let found = users::find_by_username(&conn, &username)?;
        let authenticated = match &found {
            Some(user) => verify_password(&password, &user.password_hash),
            None => {
                // 计时对策：用户不存在也陪跑一次同成本的 argon2，抹平时间差，
                // 让攻击者无法靠响应耗时枚举用户名。
                let _ = verify_password(&password, dummy_password_hash());
                false
            }
        };
        if !authenticated {
            return Ok(None);
        }
        if let Some(user) = &found {
            users::update_last_login(&conn, user.id)?;
        }
        Ok(found)
    })
    .await
    .map_err(|join| ApiError::internal(format!("登录任务异常退出：{join}")))??;

    let Some(user) = found else {
        // 不区分「不存在」与「密码错」，也不写任何带用户名的日志。
        return Err(ApiError::unauthorized(LOGIN_FAILED_MESSAGE));
    };

    // 签发用配置里的密钥；密钥为空时这里会返回 500 AUTH_NOT_CONFIGURED（绝不签出可用 token）。
    let token = sign_token(
        &state.config.server.jwt_secret,
        state.config.server.token_expiry_hours,
        &user,
    )?;
    let expires_in = state
        .config
        .server
        .token_expiry_hours
        .saturating_mul(3600)
        .min(i64::MAX as u64) as i64;

    Ok(Json(json!({
        "token": token,
        "token_type": "Bearer",
        "expires_in": expires_in,
        "user": user_json(&user),
    }))
    .into_response())
}

/// GET /api/auth/me —— 当前登录用户。
///
/// 用户信息取自中间件按 sub 查库得到的那一行，所以**降权 / 改名立即反映**，
/// 而不是 token 里那份签发时的快照。
pub async fn me(AuthUser(user): AuthUser) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "user": user_json(&user) })))
}

/// GET /api/admin/ping —— **admin 专用占位接口**。
///
/// 它存在的唯一目的是让「普通用户访问管理路由得到 403」这条画布 UT 可测：
/// 真正的管理端路由（用户管理、扫描控制、插件配置）是 S16+ 的事，
/// 到那时这个占位可以删掉或替换。
///
/// 非 admin 由 [`AdminUser`] 提取器拦下并返回 **403**（身份有效但无权限）；
/// 未登录才是 401。
pub async fn admin_ping(AdminUser(user): AdminUser) -> ApiResult<Json<Value>> {
    Ok(Json(json!({
        "ok": true,
        "message": "管理端占位接口（S16+ 在此挂真实管理路由）",
        "user": user_json(&user),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{header, Request};
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use rusqlite::params;
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::db::pool::{DbPool, TempDb};
    use crate::server::auth::{sign_token_with_ttl, JWT_SECRET_ENV};
    use crate::server::routes::build_router;

    /// 建一个跑完迁移的临时文件库状态（画布指定：DbPool::open_temp）。
    /// 必须持有返回的 TempDb，它一析构就会删库文件。
    fn test_state(secret: &str) -> (AppState, TempDb) {
        let (pool, temp) = DbPool::open_temp("server-s15-auth").expect("建临时文件库池");
        {
            let mut guard = pool.acquire().expect("借连接");
            crate::db::migrations::apply(&mut guard).expect("应用迁移");
        }
        let mut cfg = Config::defaults();
        cfg.server.jwt_secret = secret.to_string();
        cfg.storage.library_roots = vec!["/tmp/server-s15-auth-music".to_string()];
        (AppState::new(Arc::new(pool), Arc::new(cfg)), temp)
    }

    fn post_json(uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("构造 POST 请求")
    }

    fn get_with_token(uri: &str, token: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .expect("构造带令牌的 GET 请求")
    }

    fn get_with_raw_auth(uri: &str, value: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header(header::AUTHORIZATION, value)
            .body(Body::empty())
            .expect("构造 GET 请求")
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("构造 GET 请求")
    }

    /// 用 tower oneshot 直调 Router（不绑端口），返回状态码 + JSON body。
    async fn call(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
        let resp = build_router(state.clone())
            .oneshot(request)
            .await
            .expect("oneshot");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .expect("读响应体");
        let body = serde_json::from_slice(&bytes).expect("响应体必须是合法 JSON");
        (status, body)
    }

    /// 注册一个用户，返回 (状态码, body)。
    async fn register(state: &AppState, username: &str, password: &str) -> (StatusCode, Value) {
        call(
            state,
            post_json(
                "/api/auth/register",
                json!({ "username": username, "password": password }),
            ),
        )
        .await
    }

    /// 登录，返回 (状态码, body)。
    async fn login(state: &AppState, username: &str, password: &str) -> (StatusCode, Value) {
        call(
            state,
            post_json(
                "/api/auth/login",
                json!({ "username": username, "password": password }),
            ),
        )
        .await
    }

    /// 注册 + 登录，返回 token。
    ///
    /// ⚠️ **只对首个用户成立** —— 库非空后 `/api/auth/register` 就是 403 了，
    /// 第二个用户起一律用 [`admin_create`] + [`login_token`]。
    async fn token_for(state: &AppState, username: &str, password: &str) -> String {
        let (status, body) = register(state, username, password).await;
        assert_eq!(status, StatusCode::CREATED, "注册失败：{body}");
        login_token(state, username, password).await
    }

    /// 只登录拿 token（不注册）。
    async fn login_token(state: &AppState, username: &str, password: &str) -> String {
        let (status, body) = login(state, username, password).await;
        assert_eq!(status, StatusCode::OK, "登录失败：{body}");
        body["token"].as_str().expect("token 是字符串").to_string()
    }

    fn post_json_with_token(uri: &str, body: Value, token: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .expect("构造带令牌的 POST 请求")
    }

    /// 管理员建号，返回 (状态码, body)。`role` 传 `None` 即省略该字段（默认 user）。
    async fn admin_create(
        state: &AppState,
        token: &str,
        username: &str,
        password: &str,
        role: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut body = json!({ "username": username, "password": password });
        if let Some(r) = role {
            body["role"] = json!(r);
        }
        call(state, post_json_with_token("/api/admin/users", body, token)).await
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 1. 注册 / 登录 / me 全流程
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT 1：注册成功 → 登录拿 token → 带 token 访问 /api/auth/me 得到该用户。
    #[tokio::test]
    async fn register_login_and_me_round_trip() {
        let (state, _temp) = test_state("s15-secret");

        let (status, body) = register(&state, "alice", "s3cret-pw").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["user"]["username"], "alice");
        assert_eq!(body["user"]["role"], "admin", "首个用户必须是 admin");
        assert!(
            body["user"].get("password_hash").is_none(),
            "响应体绝不能带口令哈希"
        );

        let (status, body) = login(&state, "alice", "s3cret-pw").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["token_type"], "Bearer");
        assert!(body["expires_in"].as_i64().unwrap_or(0) > 0);
        let token = body["token"].as_str().expect("token 是字符串").to_string();

        let (status, body) = call(&state, get_with_token("/api/auth/me", &token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["user"]["username"], "alice");
        assert_eq!(body["user"]["role"], "admin");
        assert!(body["user"]["last_login"].as_i64().unwrap_or(0) > 0, "登录后要盖 last_login");
    }

    /// 验证：首个注册者是 admin；**库非空后公开注册关闭**，建号改由管理员发起。
    #[tokio::test]
    async fn first_registered_user_is_admin_then_registration_closes() {
        let (state, _temp) = test_state("s15-secret");

        let (status, first) = register(&state, "root", "root-password").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(first["user"]["role"], "admin", "首个用户必须是 admin");

        // 库非空 → 公开注册关闭（这是本次改动的核心）
        let (status, body) = register(&state, "bob", "bob-password").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "库非空后注册必须关闭：{body}");
        assert_eq!(body["error"]["code"], "FORBIDDEN");

        // 建号改走管理端
        let admin = login_token(&state, "root", "root-password").await;
        let (status, bob) = admin_create(&state, &admin, "bob", "bob-password", None).await;
        assert_eq!(status, StatusCode::CREATED, "管理员建号应成功：{bob}");
        assert_eq!(bob["user"]["role"], "user", "省略 role 默认是 user");

        let (status, carol) = admin_create(
            &state,
            &admin,
            "carol",
            "carol-password",
            Some("admin"),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "role=admin 应能建出管理员：{carol}");
        assert_eq!(carol["user"]["role"], "admin");
    }

    /// 非管理员不能建号；未登录更不能。
    #[tokio::test]
    async fn only_admin_can_create_users() {
        let (state, _temp) = test_state("s15-secret");
        let admin = token_for(&state, "root", "root-password").await;
        admin_create(&state, &admin, "bob", "bob-password", None).await;
        let bob = login_token(&state, "bob", "bob-password").await;

        let (status, body) = admin_create(&state, &bob, "mallory", "mallory-pw", None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "普通用户建号必须 403：{body}");

        // 未登录：中间件先拦，401（不是 403）
        let (status, _) = call(
            &state,
            post_json(
                "/api/admin/users",
                json!({ "username": "eve", "password": "eve-password" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// `role` 只认 "user" / "admin"，其它一律 400，且**不落库**。
    #[tokio::test]
    async fn admin_create_rejects_bad_role() {
        let (state, _temp) = test_state("s15-secret");
        let admin = token_for(&state, "root", "root-password").await;

        for bad in ["root", "ADMIN", "", "1"] {
            let (status, body) = admin_create(&state, &admin, "bob", "bob-password", Some(bad)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "role={bad:?} 应被拒：{body}");
        }
        // 非字符串同样拒绝，而不是静默当成 user
        let (status, _) = call(
            &state,
            post_json_with_token(
                "/api/admin/users",
                json!({ "username": "bob", "password": "bob-password", "role": 1 }),
                &admin,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // 上面全部失败 → 库里只该有 root 一个用户
        let (status, body) = login(&state, "bob", "bob-password").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "被拒的建号不该落库：{body}");
    }

    /// 验证：重复用户名返回 409（唯一冲突映射）。
    ///
    /// 注册关闭后重名只能通过管理端建号撞出来 —— 顺带把这条路径也覆盖了。
    #[tokio::test]
    async fn duplicate_username_returns_409() {
        let (state, _temp) = test_state("s15-secret");
        let admin = token_for(&state, "root", "root-password").await;
        let (status, _) = admin_create(&state, &admin, "alice", "s3cret-pw", None).await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, body) = admin_create(&state, &admin, "alice", "another-pw", None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "CONFLICT");
    }

    /// 验证：口令强度 —— 少于 8 字符、全空白都返回 400。
    #[tokio::test]
    async fn weak_or_blank_password_is_rejected() {
        let (state, _temp) = test_state("s15-secret");
        for password in ["short", "", "1234567", "        "] {
            let (status, body) = register(&state, "alice", password).await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "口令 {password:?} 必须被拒绝：{body}"
            );
            assert_eq!(body["error"]["code"], "BAD_REQUEST");
        }
        // 8 个字符（含空白但非全空白）可以通过
        let (status, _) = register(&state, "alice", "a b c d e").await;
        assert_eq!(status, StatusCode::CREATED);
    }

    /// 验证：空 / 全空白 / 含控制字符的用户名返回 400。
    #[tokio::test]
    async fn invalid_username_is_rejected() {
        let (state, _temp) = test_state("s15-secret");
        for username in ["", "   ", "a\u{7}b"] {
            let (status, body) = register(&state, username, "s3cret-pw").await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "用户名 {username:?} 必须被拒绝：{body}"
            );
        }
        // 首尾空白被规范化（trim）后能用
        let (status, body) = register(&state, "  alice  ", "s3cret-pw").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["user"]["username"], "alice");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 2. 鉴权：401 / 403
    // ─────────────────────────────────────────────────────────────────────────

    /// 验证：缺 Authorization、格式不对、非 Bearer 一律 401，且是统一错误形状。
    #[tokio::test]
    async fn missing_or_malformed_authorization_returns_401() {
        let (state, _temp) = test_state("s15-secret");
        let requests = [
            get("/api/auth/me"),
            get_with_raw_auth("/api/auth/me", "Token abc"),
            get_with_raw_auth("/api/auth/me", "Bearer"),
            get_with_raw_auth("/api/auth/me", "Bearer   "),
            get_with_raw_auth("/api/auth/me", "Bearer not-a-jwt"),
        ];
        for request in requests {
            let (status, body) = call(&state, request).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["error"]["code"], "UNAUTHORIZED");
            assert!(body["error"].get("details").is_some(), "统一形状要带 details");
        }
    }

    /// 画布 UT 4：普通用户访问 admin 路由是 **403（不是 401）**；admin 访问是 200。
    #[tokio::test]
    async fn regular_user_gets_403_on_admin_route_while_admin_gets_200() {
        let (state, _temp) = test_state("s15-secret");
        let admin_token = token_for(&state, "root", "root-password").await;
        let (status, _) = admin_create(&state, &admin_token, "bob", "bob-password", None).await;
        assert_eq!(status, StatusCode::CREATED);
        let user_token = login_token(&state, "bob", "bob-password").await;

        let (status, body) = call(&state, get_with_token("/api/admin/ping", &user_token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "普通用户必须是 403：{body}");
        assert_eq!(body["error"]["code"], "FORBIDDEN");

        let (status, body) = call(&state, get_with_token("/api/admin/ping", &admin_token)).await;
        assert_eq!(status, StatusCode::OK, "admin 必须能进：{body}");
        assert_eq!(body["ok"], true);
    }

    /// 验证：未登录访问 admin 路由是 401（先证明身份，才谈权限）。
    #[tokio::test]
    async fn anonymous_request_to_admin_route_is_401() {
        let (state, _temp) = test_state("s15-secret");
        let (status, body) = call(&state, get("/api/admin/ping")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    }

    /// 验证：令牌里的 role 不能提权 —— 数据库里是 user，签一个 role:admin 的 token 也没用。
    ///
    /// 这正是「授权以数据库为准」的核心：中间件按 sub 查库，返回的是库里的 role。
    #[tokio::test]
    async fn token_role_claim_cannot_escalate_privileges() {
        let (state, _temp) = test_state("s15-secret");
        let admin = token_for(&state, "root", "root-password").await; // 让 bob 不是首个用户
        let (_, body) = admin_create(&state, &admin, "bob", "bob-password", None).await;
        assert_eq!(body["user"]["role"], "user");
        let bob_id = body["user"]["id"].as_i64().expect("id 是整数");

        // 手工签一个 role 写成 admin 的 token（数据库里 bob 仍是 user）
        let forged_user = User {
            id: bob_id,
            username: "bob".to_string(),
            password_hash: String::new(),
            role: Role::Admin,
            created_at: 0,
            last_login: None,
        };
        let forged = sign_token_with_ttl("s15-secret", &forged_user, 3600).expect("签发");

        let (status, body) = call(&state, get_with_token("/api/admin/ping", &forged)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "必须以库里的 role 为准：{body}");
    }

    /// 验证（硬要求 9）：数据库降权后，旧 token 立刻失去 admin 权限，/me 也反映库里的新角色。
    #[tokio::test]
    async fn database_role_change_takes_effect_immediately() {
        let (state, _temp) = test_state("s15-secret");
        let (_, body) = register(&state, "root", "root-password").await;
        let root_id = body["user"]["id"].as_i64().expect("id 是整数");
        let (_, body) = login(&state, "root", "root-password").await;
        let token = body["token"].as_str().expect("token").to_string();

        // 降权前：admin 能进
        let (status, _) = call(&state, get_with_token("/api/admin/ping", &token)).await;
        assert_eq!(status, StatusCode::OK);

        // 直接改库（模拟管理端降权）
        {
            let conn = state.db.acquire().expect("借连接");
            conn.execute(
                "UPDATE users SET role = 'user' WHERE id = ?1",
                params![root_id],
            )
            .expect("降权");
        }

        // 同一个 token：admin 路由立刻 403
        let (status, _) = call(&state, get_with_token("/api/admin/ping", &token)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "降权必须立即生效");

        // /me 也必须报库里的新角色，而不是 token 里那个 admin
        let (status, body) = call(&state, get_with_token("/api/auth/me", &token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["user"]["role"], "user");
    }

    /// 验证：令牌有效但用户已被删除 → 401（而不是 500 或当成匿名）。
    #[tokio::test]
    async fn deleted_user_token_is_rejected_with_401() {
        let (state, _temp) = test_state("s15-secret");
        let (_, body) = register(&state, "alice", "s3cret-pw").await;
        let id = body["user"]["id"].as_i64().expect("id 是整数");
        let (_, body) = login(&state, "alice", "s3cret-pw").await;
        let token = body["token"].as_str().expect("token").to_string();

        {
            let conn = state.db.acquire().expect("借连接");
            conn.execute("DELETE FROM users WHERE id = ?1", params![id])
                .expect("删用户");
        }

        let (status, body) = call(&state, get_with_token("/api/auth/me", &token)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 3. 令牌过期 / 篡改 / alg:none（路由级）
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT 2：exp 已过去的 token 访问受保护路由 → 401。
    #[tokio::test]
    async fn expired_token_is_401_on_protected_route() {
        let (state, _temp) = test_state("s15-secret");
        let (_, body) = register(&state, "alice", "s3cret-pw").await;
        let id = body["user"]["id"].as_i64().expect("id 是整数");

        let user = User {
            id,
            username: "alice".to_string(),
            password_hash: String::new(),
            role: Role::Admin,
            created_at: 0,
            last_login: None,
        };
        // 负 TTL 直接造过期 token，不 sleep
        let token = sign_token_with_ttl("s15-secret", &user, -3600).expect("签发过期 token");

        let (status, body) = call(&state, get_with_token("/api/auth/me", &token)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    }

    /// 画布 UT 3：改签名段 / 改 payload 段（各一条）→ 都 401。
    #[tokio::test]
    async fn tampered_token_is_401_on_protected_route() {
        let (state, _temp) = test_state("s15-secret");
        let token = token_for(&state, "alice", "s3cret-pw").await;
        let parts: Vec<&str> = token.split('.').collect();

        // 改签名段
        let mut signature = URL_SAFE_NO_PAD.decode(parts[2]).expect("签名可解码");
        signature[0] ^= 0x01;
        let tampered_signature = format!(
            "{}.{}.{}",
            parts[0],
            parts[1],
            URL_SAFE_NO_PAD.encode(&signature)
        );

        // 改 payload 段（换用户名 + 提权成 admin），签名沿用旧的
        let mut value: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).expect("载荷可解码"))
                .expect("载荷是 JSON");
        value["username"] = json!("mallory");
        value["role"] = json!("admin");
        let tampered_payload = format!(
            "{}.{}.{}",
            parts[0],
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).expect("重编码")),
            parts[2]
        );

        for broken in [tampered_signature, tampered_payload] {
            let (status, body) = call(&state, get_with_token("/api/auth/me", &broken)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "篡改必须 401：{body}");
            assert_eq!(body["error"]["code"], "UNAUTHORIZED");
        }
    }

    /// 我加的 UT 5：alg:none 且签名段为空的 token → 401。
    #[tokio::test]
    async fn alg_none_token_is_401_on_protected_route() {
        let (state, _temp) = test_state("s15-secret");
        let (_, body) = register(&state, "alice", "s3cret-pw").await;
        let id = body["user"]["id"].as_i64().expect("id 是整数");

        let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            json!({
                "sub": id.to_string(),
                "username": "alice",
                "role": "admin",
                "iat": 0,
                "exp": 4102444800i64,
            })
            .to_string(),
        );
        let forged = format!("{header}.{payload}.");

        let (status, body) = call(&state, get_with_token("/api/auth/me", &forged)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "alg:none 必须 401：{body}");
        assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 4. 配置 / 泄漏
    // ─────────────────────────────────────────────────────────────────────────

    /// 我加的 UT 6：jwt_secret 为空时登录必须**明确失败**（500 + 指名 MR_JWT_SECRET），
    /// 而不是签出一个无密钥、谁都能伪造的 token。
    #[tokio::test]
    async fn empty_jwt_secret_makes_login_fail_closed() {
        let (state, _temp) = test_state("");
        // 密钥检查在最前面，所以「用户存在与否」不该影响结果 ——
        // 两种输入都必须 500，否则响应又能用来枚举用户名。
        for who in ["alice", "nobody-at-all"] {
            let (status, body) = login(&state, who, "s3cret-pw").await;
            assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "用户 {who} 应当是 500：{body}");
            assert_eq!(body["error"]["code"], "AUTH_NOT_CONFIGURED");
        }
        let (_, body) = login(&state, "alice", "s3cret-pw").await;
        assert_eq!(body["error"]["code"], "AUTH_NOT_CONFIGURED");
        assert!(
            body["token"].is_null(),
            "绝不能签出可用 token：{body}"
        );
        let message = body["error"]["message"].as_str().expect("message 是字符串");
        assert!(
            message.contains(JWT_SECRET_ENV),
            "错误信息必须点名 {JWT_SECRET_ENV}：{message}"
        );
    }


    /// 回归：jwt_secret 为空时**注册也必须失败关闭**。
    ///
    /// 这条是端到端实跑才发现的洞：原先只有登录被拦，注册放行。
    /// 于是「首个注册用户自动成为 admin」会开出一个提权窗口 ——
    /// 运维漏配 MR_JWT_SECRET 时有人抢先注册，等运维补上密钥后，
    /// 那个人登录进去就是管理员。认证没配好，注册/登录要一起拦。
    #[tokio::test]
    async fn empty_jwt_secret_makes_register_fail_closed_too() {
        let (state, _temp) = test_state("");
        let (status, body) = register(&state, "alice", "s3cret-pw").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "空密钥时注册必须失败：{body}");
        assert_eq!(body["error"]["code"], "AUTH_NOT_CONFIGURED");

        // 关键：不能留下任何「准 admin」用户等着密钥补上后生效
        let conn = state.db.acquire().expect("借连接");
        assert_eq!(
            users::count(&conn).expect("数用户"),
            0,
            "失败关闭时不得写入任何用户"
        );
    }

    /// 我加的 UT 7：登录失败不泄漏用户名是否存在 —— 两条响应完全一致（状态码 + body）。
    #[tokio::test]
    async fn login_failure_does_not_reveal_username_existence() {
        let (state, _temp) = test_state("s15-secret");
        register(&state, "alice", "correct-password").await;

        let (missing_status, missing_body) = login(&state, "nobody", "whatever-pw").await;
        let (wrong_status, wrong_body) = login(&state, "alice", "wrong-password").await;

        assert_eq!(missing_status, StatusCode::UNAUTHORIZED);
        assert_eq!(missing_status, wrong_status, "状态码必须一致");
        assert_eq!(missing_body, wrong_body, "响应体必须逐字节一致");

        // 文案里也不能出现「用户不存在」这类区分性措辞
        let message = missing_body["error"]["message"]
            .as_str()
            .expect("message 是字符串");
        assert_eq!(message, LOGIN_FAILED_MESSAGE);
    }

    /// 我加的 UT 8：口令不落响应 —— 注册 / 登录取回来的 body 里不含明文口令，
    /// 库里存的也是 argon2id 哈希而不是明文。
    #[tokio::test]
    async fn responses_and_storage_never_contain_the_plaintext_password() {
        const PASSWORD: &str = "Sup3r-Secret-Pw";
        let (state, _temp) = test_state("s15-secret");

        let (_, registered) = register(&state, "alice", PASSWORD).await;
        let (_, logged_in) = login(&state, "alice", PASSWORD).await;
        let token = logged_in["token"].as_str().expect("token").to_string();
        let (_, me_body) = call(&state, get_with_token("/api/auth/me", &token)).await;

        for body in [&registered, &logged_in, &me_body] {
            let raw = body.to_string();
            assert!(!raw.contains(PASSWORD), "响应体泄漏了明文口令：{raw}");
        }

        // 库里必须是 argon2id 哈希，不是明文
        let hash = {
            let conn = state.db.acquire().expect("借连接");
            users::find_by_username(&conn, "alice")
                .expect("查用户")
                .expect("用户存在")
                .password_hash
        };
        assert_ne!(hash, PASSWORD, "库里不能存明文口令");
        assert!(hash.starts_with("$argon2id$"), "必须是 argon2id：{hash}");
    }

    /// 验证：管理端建出的普通用户立刻就能登录并通过中间件（写入的哈希是可校验的）。
    #[tokio::test]
    async fn registered_user_can_authenticate_immediately() {
        let (state, _temp) = test_state("s15-secret");
        // 先建首个 admin，再由他建出普通用户 alice
        let admin = token_for(&state, "root", "root-password").await;
        let (status, body) = admin_create(&state, &admin, "alice", "s3cret-pw", None).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["user"]["role"], "user", "管理端建出来的是普通用户");

        let (_, body) = login(&state, "alice", "s3cret-pw").await;
        let token = body["token"].as_str().expect("token").to_string();
        let (status, body) = call(&state, get_with_token("/api/auth/me", &token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["user"]["username"], "alice");
        assert_eq!(body["user"]["role"], "user");
    }
}
