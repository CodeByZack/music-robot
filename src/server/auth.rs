//! S15 · 认证：JWT 签发 / 验证 + 鉴权中间件与提取器。
//!
//! ## 安全约定（每一条都有对应测试）
//!
//! 1. **密钥为空绝不签发 / 验证**：`ensure_secret` 在签发与验证的入口各调一次，
//!    空串 / 全空白一律返回 `AuthError::MissingSecret`，对外是 500 +
//!    `AUTH_NOT_CONFIGURED`，文案里点名环境变量 `MR_JWT_SECRET`。配置默认值就是空串，
//!    若拿空串当 HMAC 密钥签名，等于完全没有认证。
//! 2. **只接受 HS256**：解析 header 后比对 `alg == "HS256"`，`alg: none`、
//!    `RS256`、`HS512` 等一律 401。**绝不用 header 里的 alg 去决定验签方式** ——
//!    那正是算法混淆漏洞的根源。
//! 3. **恒定时间验签**：用 `hmac::Mac::verify_slice`，它内部是恒定时间比较；
//!    自己写 `==` 比字节会按位置提前返回，泄漏签名信息。
//! 4. **校验 exp / iat**：`exp` 必须存在且晚于当前时间（严格，不留宽限）；
//!    `iat` 不在未来（允许 `CLOCK_SKEW_SECS` 的时钟偏移）。
//! 5. **口令用 argon2id**：每个口令独立 16 字节随机盐
//!    （`SaltString::generate`，内部走 getrandom / OS RNG），校验恒定时间；
//!    明文与哈希都不写日志。argon2 是慢 KDF（默认参数几十毫秒），
//!    调用点**必须**放在 `tokio::task::spawn_blocking` 里（见 routes::auth）。
//! 6. **鉴权以数据库为准**：JWT 里的 role 只作展示 / 前端快速判断，中间件每次都按
//!    `sub` 查库，用户被降权或删除后旧 token 立刻失效（见 `authenticate`）。
//!
//! ## 中间件与提取器的分工
//!
//! `require_auth` 是挂在受保护路由上的中间件：取出 `Authorization: Bearer <token>`、
//! 验签、**按 sub 查库**，成功后把 `AuthUser` 放进请求扩展。
//! `AuthUser` / `AdminUser` 是读取扩展的提取器：前者要求「已登录」，
//! 后者再要求 `role == admin`；普通用户访问管理路由是 **403（不是 401）** ——
//! 401 表示「没证明身份」，403 表示「身份有效但没权限」。
//!
//! ## 为什么中间件里每次都查库
//!
//! 每个受保护请求多一次「按主键查 SQLite」（命中页缓存时微秒级），换来的是
//! 「令牌里的 role 不能当授权依据」这条硬性质：降权 / 删号立即生效，而不必等
//! token 自然过期。若将来这一步成为瓶颈，正确的优化是加带失效时间的缓存，
//! 而不是改回信任 token 里的 role。

use std::sync::Arc;

use argon2::password_hash::phc::{PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::db::models::{Role, User};
use crate::db::repos::users;

use super::error::ApiError;
use super::state::AppState;

/// HMAC-SHA256（HS256 的底座）。
type HmacSha256 = Hmac<Sha256>;

/// 唯一接受的签名算法。
pub const JWT_ALG: &str = "HS256";

/// 密钥所在的环境变量名。写进错误信息，运维照着设置即可。
pub const JWT_SECRET_ENV: &str = "MR_JWT_SECRET";

/// 固定 JWT header。验签方只认 `alg = HS256`，不接受任何其他取值。
const JWT_HEADER: &str = r#"{"alg":"HS256","typ":"JWT"}"#;

/// `iat` 允许的时钟偏移（秒）。只用于「不在未来」的判断；
/// `exp` 是严格判定（过期即 401），所以这里不给过期留宽限。
const CLOCK_SKEW_SECS: i64 = 60;

/// 当前 Unix 秒。系统时钟早于 1970 时返回 0（复用 db 层同样的兜底策略）。
fn now_unix_secs() -> i64 {
    crate::db::now_unix_ms() / 1000
}

// ─────────────────────────────────────────────────────────────────────────────
// 错误
// ─────────────────────────────────────────────────────────────────────────────

/// 令牌 / 密钥相关错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// 密钥未配置（空串 / 全空白）
    MissingSecret,
    /// 令牌结构不合法（段数、base64、JSON、字段类型）
    Malformed,
    /// header 里的 alg 不是 HS256（含 `none` 与算法混淆）
    UnsupportedAlgorithm,
    /// 签名不匹配（含被篡改的 payload / 签名）
    BadSignature,
    /// 已过期
    Expired,
    /// iat 在未来（超出允许的时钟偏移）
    NotYetValid,
    /// 内部失败（序列化 / HMAC 构造等），只进日志不透传
    Internal(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::MissingSecret => write!(
                f,
                "服务端未配置 JWT 密钥：请设置环境变量 {JWT_SECRET_ENV} 后再启用认证"
            ),
            AuthError::Malformed => write!(f, "令牌格式不正确"),
            AuthError::UnsupportedAlgorithm => write!(f, "令牌签名算法不受支持（仅接受 HS256）"),
            AuthError::BadSignature => write!(f, "令牌签名校验失败"),
            AuthError::Expired => write!(f, "登录状态已过期，请重新登录"),
            AuthError::NotYetValid => write!(f, "令牌尚未生效"),
            AuthError::Internal(detail) => write!(f, "令牌处理失败：{detail}"),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        match e {
            // 配置问题：500 + 明确中文，点名要设置的环境变量。
            // 不降级成 401，否则运维会把「服务没配好」误当成「用户密码错」。
            AuthError::MissingSecret => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "AUTH_NOT_CONFIGURED",
                format!("服务端未配置 JWT 密钥（环境变量 {JWT_SECRET_ENV}），已拒绝签发 / 校验令牌"),
            ),
            // 内部失败：原因只进服务端日志，响应体是通用中文。
            AuthError::Internal(detail) => ApiError::internal(detail),
            // 其余一律 401。
            other => ApiError::unauthorized(other.to_string()),
        }
    }
}

/// 密钥必须非空（trim 后）才允许签发 / 验证 / 注册。
///
/// 这是「空密钥 = 没有认证」这条硬要求的唯一落点。**注册也要过这里** ——
/// 注册本身不需要密钥，但如果不拦，「首个注册用户自动成为 admin」这条规则会制造
/// 一个提权窗口：运维漏配 MR_JWT_SECRET 时有人抢先注册，等运维补上密钥后，
/// 那个人就直接成了管理员。认证子系统没配好，整个认证入口都应失败关闭。
pub(crate) fn ensure_secret(secret: &str) -> Result<(), AuthError> {
    if secret.trim().is_empty() {
        return Err(AuthError::MissingSecret);
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// 口令哈希（argon2id）
// ─────────────────────────────────────────────────────────────────────────────

/// 用 argon2id 计算口令哈希，返回 PHC 字符串（`$argon2id$v=19$...`）。
///
/// 每个口令独立随机盐：`SaltString::generate` 走 phc 的 getrandom 后端，
/// 即操作系统的 CSPRNG。**慢**（默认参数几十毫秒），调用方必须 spawn_blocking。
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate();
    let salt = salt.to_salt();
    let hashed = Argon2::default()
        .hash_password_with_salt(password.as_bytes(), salt.as_ref())
        .map_err(|e| AuthError::Internal(format!("argon2 计算口令哈希失败：{e}")))?;
    Ok(hashed.to_string())
}

/// 校验口令是否匹配 PHC 字符串。
///
/// * 用 `PasswordVerifier::verify_password`（内部恒定时间比较）；
/// * 参数从哈希串自身解析，将来调 argon2 参数不会让旧哈希失效；
/// * 哈希串损坏时返回 false，而不是报错 —— 对调用方来说就是「不匹配」。
///
/// 慢 KDF，调用方必须 spawn_blocking。
pub fn verify_password(password: &str, password_hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(password_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// 登录失败（用户不存在）时用来「陪跑」一次 argon2 的占位哈希。
///
/// 用户不存在与密码错误必须返回同一条响应（避免用户名枚举），但两者的**耗时**
/// 也应尽量一致：直接跳过校验会让「用户不存在」明显更快，攻击者靠计时就能枚举。
/// 这里对不存在的用户也做一次同参数的 argon2 校验，抹平这个时间差。
///
/// 仅在第一次未命中时生成一次（OnceLock），之后复用；生成失败则退化成空串，
/// 此时 `PasswordHash::new` 直接失败、不做 KDF —— 只是损失计时对策，不影响正确性。
pub(crate) fn dummy_password_hash() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY
        .get_or_init(|| hash_password("music-robot-login-timing-placeholder").unwrap_or_default())
        .as_str()
}

// ─────────────────────────────────────────────────────────────────────────────
// JWT
// ─────────────────────────────────────────────────────────────────────────────

/// 验证通过后从令牌里解出的声明（**只取必要字段**）。
///
/// 载荷里还带了 `username` / `role` 供前端展示与排查，但**不在这里解析、
/// 也绝不用于授权** —— 授权一律以数据库里的行为准（见 `authenticate`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    /// 用户主键（JWT 规范的 sub 是字符串，这里解析回 i64）
    pub sub: i64,
    /// 签发时间（Unix 秒）
    pub issued_at: i64,
    /// 过期时间（Unix 秒）
    pub expires_at: i64,
}

/// 按配置签发令牌（`expiry_hours` 小时有效）。
pub fn sign_token(secret: &str, expiry_hours: u64, user: &User) -> Result<String, AuthError> {
    // 小时 -> 秒，饱和运算 + 夹到 i64 上限，不做任何可能 panic 的转换。
    let ttl_secs = expiry_hours.saturating_mul(3600).min(i64::MAX as u64) as i64;
    sign_token_with_ttl(secret, user, ttl_secs)
}

/// 按「相对当前时间的存在时长」签发。
///
/// `ttl_secs` 允许为负 —— 测试就是用负数直接造一个已过期的 token
/// （不必 sleep），见模块内的单元测试。
pub fn sign_token_with_ttl(secret: &str, user: &User, ttl_secs: i64) -> Result<String, AuthError> {
    sign_token_at(secret, user, now_unix_secs(), ttl_secs)
}

/// 指定签发时间与有效期签发（测试用可控时间）。
pub fn sign_token_at(
    secret: &str,
    user: &User,
    issued_at: i64,
    ttl_secs: i64,
) -> Result<String, AuthError> {
    // 空密钥绝不签发：否则任何人拿到这个「无签名」token 都能冒充用户。
    ensure_secret(secret)?;

    let header_b64 = URL_SAFE_NO_PAD.encode(JWT_HEADER);
    let payload = json!({
        "sub": user.id.to_string(),
        // 下面两个字段只作展示 / 排查，授权不看它们。
        "username": user.username,
        "role": user.role.as_str(),
        "iat": issued_at,
        "exp": issued_at.saturating_add(ttl_secs),
    });
    let payload_bytes = serde_json::to_vec(&payload)
        .map_err(|e| AuthError::Internal(format!("序列化令牌载荷失败：{e}")))?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(&payload_bytes);

    let signing_input = format!("{header_b64}.{payload_b64}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| AuthError::Internal(format!("构造 HMAC 失败：{e}")))?;
    mac.update(signing_input.as_bytes());
    let signature = mac.finalize().into_bytes();

    Ok(format!(
        "{signing_input}.{}",
        URL_SAFE_NO_PAD.encode(signature)
    ))
}

/// 验证令牌并解出声明。
///
/// 顺序很重要：先校验 alg（拒绝 none / 混淆），再验签（恒定时间），
/// **只有签名通过之后**才解析并信任 payload，最后才校验 exp / iat。
pub fn verify_token(secret: &str, token: &str) -> Result<Claims, AuthError> {
    // 空密钥绝不验证：否则等于接受任何用空密钥签的东西。
    ensure_secret(secret)?;

    let mut segments = token.split('.');
    let header_b64 = segments.next().ok_or(AuthError::Malformed)?;
    let payload_b64 = segments.next().ok_or(AuthError::Malformed)?;
    let signature_b64 = segments.next().ok_or(AuthError::Malformed)?;
    // 多出第 4 段（或任何多余内容）一律拒绝，避免解析歧义。
    if segments.next().is_some()
        || header_b64.is_empty()
        || payload_b64.is_empty()
        || signature_b64.is_empty()
    {
        return Err(AuthError::Malformed);
    }

    // 1) header：只认 HS256。用「比对常量」而不是「按 header 里的 alg 分派验签方式」，
    //    这是拒绝 alg:none 与算法混淆的关键。
    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|_| AuthError::Malformed)?;
    let header: Value = serde_json::from_slice(&header_bytes).map_err(|_| AuthError::Malformed)?;
    if header.get("alg").and_then(Value::as_str) != Some(JWT_ALG) {
        return Err(AuthError::UnsupportedAlgorithm);
    }

    // 2) 签名：verify_slice 内部恒定时间比较，且会校验长度。
    let signature = URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|_| AuthError::BadSignature)?;
    let signing_input = format!("{header_b64}.{payload_b64}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| AuthError::Internal(format!("构造 HMAC 失败：{e}")))?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| AuthError::BadSignature)?;

    // 3) 签名通过，payload 才可信。
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| AuthError::Malformed)?;
    let payload: Value = serde_json::from_slice(&payload_bytes).map_err(|_| AuthError::Malformed)?;
    let claims = claims_from_value(&payload)?;

    // 4) 时间校验。exp 严格：过了就是 401，不留宽限。
    let now = now_unix_secs();
    if claims.issued_at > now.saturating_add(CLOCK_SKEW_SECS) {
        return Err(AuthError::NotYetValid);
    }
    if claims.expires_at <= now {
        return Err(AuthError::Expired);
    }
    Ok(claims)
}

/// 从已验签的 payload 里解出声明；字段缺失 / 类型不对一律 Malformed。
///
/// 注意：这里**故意不解析 role**。role 一定要以数据库为准，见 `authenticate`。
fn claims_from_value(payload: &Value) -> Result<Claims, AuthError> {
    let sub = payload
        .get("sub")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<i64>().ok())
        .ok_or(AuthError::Malformed)?;
    let issued_at = payload
        .get("iat")
        .and_then(Value::as_i64)
        .ok_or(AuthError::Malformed)?;
    let expires_at = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or(AuthError::Malformed)?;
    Ok(Claims {
        sub,
        issued_at,
        expires_at,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// 中间件 + 提取器
// ─────────────────────────────────────────────────────────────────────────────

/// 已认证用户，由 `require_auth` 中间件放进请求扩展。
///
/// 其中 `User` 是**数据库里的当前行**，不是 token 里那份快照 ——
/// 这就是「鉴权以数据库为准」的载体。
#[derive(Debug, Clone)]
pub struct AuthUser(pub User);

impl AuthUser {
    /// 只读借用当前用户。
    pub fn user(&self) -> &User {
        &self.0
    }
}

/// 已认证且角色为 admin 的用户（非 admin 的请求是 403）。
#[derive(Debug, Clone)]
pub struct AdminUser(pub User);

impl AdminUser {
    /// 只读借用当前用户。
    pub fn user(&self) -> &User {
        &self.0
    }
}

/// 从 `Authorization: Bearer <token>` 取令牌，格式不对返回 401。
fn bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    let raw = headers
        .get(header::AUTHORIZATION)
        .ok_or_else(|| ApiError::unauthorized("缺少 Authorization 请求头"))?;
    let text = raw
        .to_str()
        .map_err(|_| ApiError::unauthorized("Authorization 请求头不是合法的 ASCII 文本"))?;
    let (scheme, token) = text
        .split_once(' ')
        .ok_or_else(|| ApiError::unauthorized("Authorization 请求头格式应为 Bearer <token>"))?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(ApiError::unauthorized("仅支持 Bearer 认证方式"));
    }
    let token = token.trim();
    if token.is_empty() {
        return Err(ApiError::unauthorized("Authorization 请求头缺少令牌"));
    }
    Ok(token)
}

/// 验签 + 按 sub 查库，返回**数据库里的**当前用户。
///
/// 查库这一步是刻意的取舍：多一次主键查询，换来「降权 / 删号立即生效」。
/// 查库是同步阻塞的，所以包在 spawn_blocking 里（S14 铁律）。
async fn authenticate(state: &AppState, token: &str) -> Result<User, ApiError> {
    let claims = verify_token(&state.config.server.jwt_secret, token)?;
    let db = Arc::clone(&state.db);
    let user_id = claims.sub;
    let loaded = tokio::task::spawn_blocking(move || -> Result<Option<User>, ApiError> {
        let conn = db.acquire()?;
        Ok(users::get(&conn, user_id)?)
    })
    .await
    .map_err(|join| ApiError::internal(format!("读取登录用户的任务异常退出：{join}")))??;

    // 令牌有效但用户已被删除：401（让前端去重新登录），而不是 500。
    loaded.ok_or_else(|| ApiError::unauthorized("登录用户不存在或已被删除，请重新登录"))
}

/// 鉴权中间件：验签 + 查库，成功后把 `AuthUser` 注入请求扩展。
///
/// 挂在「需要登录」的子 Router 上（见 routes::build_router）；公开路由
/// （健康检查 / 注册 / 登录）不挂它。
pub async fn require_auth(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = match bearer_token(request.headers()) {
        Ok(token) => token.to_string(),
        Err(e) => return e.into_response(),
    };
    match authenticate(&state, &token).await {
        Ok(user) => {
            request.extensions_mut().insert(AuthUser(user));
            next.run(request).await
        }
        // 401 的响应体是 S14 的统一形状 { error: { code, message, details } }。
        Err(e) => e.into_response(),
    }
}

/// 提取器：要求请求已通过 `require_auth`（扩展里有 `AuthUser`）。
///
/// 没挂中间件的路由上直接用它一定 401 —— 这是有意的：公开路由不该顺手拿到用户。
impl<S: Send + Sync> FromRequestParts<S> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthUser>()
            .cloned()
            .ok_or_else(|| ApiError::unauthorized("未认证：请先登录并携带 Bearer 令牌"))
    }
}

/// 提取器：在 `AuthUser` 之上要求 `role == admin`。
///
/// 非 admin 返回 **403**（身份有效但无权限），不是 401 —— 画布 UT 明确要求。
impl<S: Send + Sync> FromRequestParts<S> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let AuthUser(user) = AuthUser::from_request_parts(parts, state).await?;
        // 角色来自数据库里的那一行（中间件刚查过），不是 token 里的 role。
        if user.role != Role::Admin {
            return Err(ApiError::forbidden("需要管理员权限"));
        }
        Ok(AdminUser(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn sample_user(id: i64, role: Role) -> User {
        User {
            id,
            username: "alice".to_string(),
            password_hash: "ignored".to_string(),
            role,
            created_at: 0,
            last_login: None,
        }
    }

    /// 手工拼一个「alg 可控」的 token。签名段放一段假的 32 字节，
    /// 这样验证会走到 alg 检查（而不是被签名段为空的 Malformed 提前拦下）。
    fn unsigned_token(alg: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(format!(r#"{{"alg":"{alg}","typ":"JWT"}}"#));
        let payload = URL_SAFE_NO_PAD.encode(
            r#"{"sub":"1","username":"alice","role":"admin","iat":0,"exp":4102444800}"#,
        );
        let bogus_signature = URL_SAFE_NO_PAD.encode([0u8; 32]);
        format!("{header}.{payload}.{bogus_signature}")
    }

    /// 只改签名段（保持 base64 合法），payload 不动。
    fn tamper_signature(token: &str) -> String {
        let parts: Vec<&str> = token.split('.').collect();
        let mut signature = URL_SAFE_NO_PAD.decode(parts[2]).expect("签名段可解码");
        signature[0] ^= 0x01;
        format!(
            "{}.{}.{}",
            parts[0],
            parts[1],
            URL_SAFE_NO_PAD.encode(&signature)
        )
    }

    /// 改 payload（这里把 role 改成 admin、exp 推到 2100 年），签名沿用旧的。
    fn tamper_payload(token: &str) -> String {
        let parts: Vec<&str> = token.split('.').collect();
        let bytes = URL_SAFE_NO_PAD.decode(parts[1]).expect("载荷段可解码");
        let mut value: Value = serde_json::from_slice(&bytes).expect("载荷是 JSON");
        value["role"] = json!("admin");
        value["exp"] = json!(4102444800i64);
        let payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).expect("重新序列化载荷"));
        format!("{}.{}.{}", parts[0], payload, parts[2])
    }

    /// 验证：合法签发的令牌能验证通过，并解出正确的 sub / 时间。
    #[test]
    fn jwt_round_trip_recovers_the_subject() {
        let user = sample_user(7, Role::User);
        let token = sign_token_with_ttl("top-secret", &user, 3600).expect("签发");
        let claims = verify_token("top-secret", &token).expect("验证");
        assert_eq!(claims.sub, 7);
        assert!(claims.expires_at > claims.issued_at);
    }

    /// 验证：exp 已过去的 token 返回 Expired（用负 TTL 造，不 sleep）。
    #[test]
    fn expired_token_is_rejected() {
        let user = sample_user(7, Role::User);
        let token = sign_token_with_ttl("top-secret", &user, -3600).expect("签发过期 token");
        assert!(matches!(
            verify_token("top-secret", &token),
            Err(AuthError::Expired)
        ));
    }

    /// 验证：iat 在未来的 token 被拒绝（超出时钟偏移）。
    #[test]
    fn future_issued_at_is_rejected() {
        let user = sample_user(7, Role::User);
        let now = now_unix_secs();
        let token = sign_token_at("top-secret", &user, now + 3600, 3600).expect("签发");
        assert!(matches!(
            verify_token("top-secret", &token),
            Err(AuthError::NotYetValid)
        ));
    }

    /// 验证：改签名段 → BadSignature。
    #[test]
    fn tampered_signature_is_rejected() {
        let user = sample_user(7, Role::Admin);
        let token = sign_token_with_ttl("top-secret", &user, 3600).expect("签发");
        assert!(matches!(
            verify_token("top-secret", &tamper_signature(&token)),
            Err(AuthError::BadSignature)
        ));
    }

    /// 验证：改 payload（提权成 admin / 延长 exp）但签名不动 → BadSignature。
    #[test]
    fn tampered_payload_is_rejected() {
        let user = sample_user(7, Role::User);
        let token = sign_token_with_ttl("top-secret", &user, 3600).expect("签发");
        assert!(matches!(
            verify_token("top-secret", &tamper_payload(&token)),
            Err(AuthError::BadSignature)
        ));
    }

    /// 验证：alg:none、HS512、RS256 一律 UnsupportedAlgorithm，绝不按 header 分派验签。
    #[test]
    fn alg_none_and_algorithm_confusion_are_rejected() {
        for alg in ["none", "None", "HS512", "RS256", "ES256", ""] {
            let token = unsigned_token(alg);
            assert!(
                matches!(
                    verify_token("top-secret", &token),
                    Err(AuthError::UnsupportedAlgorithm)
                ),
                "alg={alg:?} 必须被拒绝"
            );
        }

        // 合法 token 的 header 换成 alg:none 也要拒绝
        let user = sample_user(1, Role::Admin);
        let token = sign_token_with_ttl("top-secret", &user, 3600).expect("签发");
        let parts: Vec<&str> = token.split('.').collect();
        let forged = format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#),
            parts[1],
            parts[2]
        );
        assert!(verify_token("top-secret", &forged).is_err());
    }

    /// 验证：空 / 全空白密钥时，签发与验证都明确失败，而不是签出一个「无密钥」token。
    #[test]
    fn empty_or_blank_secret_refuses_to_sign_and_verify() {
        let user = sample_user(1, Role::Admin);
        for secret in ["", "   ", "\t\n"] {
            assert!(matches!(
                sign_token_with_ttl(secret, &user, 3600),
                Err(AuthError::MissingSecret)
            ));
        }

        // 用非空密钥签一个合法 token，再用空密钥验 → 也必须 MissingSecret
        let token = sign_token_with_ttl("real-secret", &user, 3600).expect("签发");
        assert!(matches!(
            verify_token("", &token),
            Err(AuthError::MissingSecret)
        ));
        assert!(matches!(
            verify_token("   ", &token),
            Err(AuthError::MissingSecret)
        ));

        // 错误信息必须点名要设置的环境变量
        let text = AuthError::MissingSecret.to_string();
        assert!(
            text.contains(JWT_SECRET_ENV),
            "错误信息要提到 {JWT_SECRET_ENV}：{text}"
        );
    }

    /// 验证：结构不合法 / 字段缺失的 token 一律拒绝，不会 panic。
    #[test]
    fn malformed_tokens_are_rejected() {
        let cases = [
            "",
            "a",
            "a.b",
            "a.b.c.d",
            "!!!.???.@@@",
            "a.b.c",
            // 合法 base64 但 header 不是 JSON
            "aGVsbG8.YQ.c2ln",
        ];
        for token in cases {
            assert!(
                verify_token("top-secret", token).is_err(),
                "非法 token 必须被拒绝：{token:?}"
            );
        }

        // 缺 exp / iat 的签名合法 token
        let mangled = |payload: &str| {
            let header_b64 = URL_SAFE_NO_PAD.encode(JWT_HEADER);
            let payload_b64 = URL_SAFE_NO_PAD.encode(payload);
            let signing_input = format!("{header_b64}.{payload_b64}");
            let mut mac = HmacSha256::new_from_slice(b"top-secret").expect("HMAC");
            mac.update(signing_input.as_bytes());
            format!(
                "{signing_input}.{}",
                URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
            )
        };
        assert!(verify_token("top-secret", &mangled(r#"{"sub":"1"}"#)).is_err());
        assert!(
            verify_token("top-secret", &mangled(r#"{"sub":"abc","iat":0,"exp":4102444800}"#))
                .is_err()
        );
        assert!(verify_token("top-secret", &mangled(r#"{"sub":"1","iat":0}"#)).is_err());
    }

    /// 验证：argon2id + 独立随机盐；同一口令两次哈希不同但都能校验通过。
    #[test]
    fn password_hashing_is_argon2id_with_unique_salts() {
        let first = hash_password("correct horse battery staple").expect("哈希");
        let second = hash_password("correct horse battery staple").expect("哈希");
        assert!(first.starts_with("$argon2id$"), "必须是 argon2id：{first}");
        assert_ne!(first, second, "每个口令必须用独立随机盐");
        assert!(verify_password("correct horse battery staple", &first));
        assert!(verify_password("correct horse battery staple", &second));
        assert!(!verify_password("wrong password", &first));
        assert!(!verify_password("correct horse battery staple", "not-a-hash"));
    }

    /// 验证：Bearer 解析只接受规范写法。
    #[test]
    fn bearer_token_parsing_accepts_only_well_formed_headers() {
        let mut headers = HeaderMap::new();
        assert!(bearer_token(&headers).is_err(), "缺头必须失败");

        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Token abc"));
        assert!(bearer_token(&headers).is_err(), "非 Bearer 方案必须失败");

        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer"));
        assert!(bearer_token(&headers).is_err(), "没有令牌必须失败");

        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer   "));
        assert!(bearer_token(&headers).is_err(), "空白令牌必须失败");

        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("bearer xyz"));
        assert_eq!(bearer_token(&headers).expect("小写方案也应接受"), "xyz");

        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer xyz"));
        assert_eq!(bearer_token(&headers).expect("标准写法"), "xyz");
    }
}
