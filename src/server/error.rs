//! S14 · 统一 API 错误。
//!
//! 服务端所有 handler 的错误都收敛到 ApiError，响应体形状固定为：
//!
//!     { "error": { "code": "NOT_FOUND", "message": "请求的接口不存在", "details": null } }
//!
//! * code    —— 稳定的机器可读大写串，前端按它分支，不随文案变化；
//! * message —— 面向用户的中文说明；
//! * details —— 补充信息（可为 null）。只放安全内容，永远不放内部路径 / SQL。
//!
//! ## 内部错误不泄漏
//!
//! 借连接失败 / SQL 报错 / 文件路径这些底层细节只写日志（stderr），对外统一降级成
//! INTERNAL + 通用中文文案。错误串里常带数据库绝对路径、表结构甚至 SQL 片段，
//! 直接回给客户端等于把内部信息送给任何能发请求的人。

use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::db::pool::DbPoolError;
use crate::db::repos::RepoError;
use crate::service::{LibraryError, ScrapeError};

/// 内部错误对外的统一文案（不随底层原因变化）。
const INTERNAL_MESSAGE: &str = "服务器内部错误，请稍后重试";

/// handler 的返回别名：问号运算符会经各 From 实现自动收敛到 ApiError。
pub type ApiResult<T> = Result<T, ApiError>;

/// 统一 API 错误。
///
/// 除 status（HTTP 状态码）与 code（机器可读串）之外，其余都只用于响应体；
/// details 默认 null，始终存在，保证响应形状严格稳定。
#[derive(Debug, Clone)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Value,
}

impl ApiError {
    /// 通用构造：指定状态码、稳定 code 与中文文案。
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            details: Value::Null,
        }
    }

    /// 追加安全的补充信息（调用方自行保证其中没有内部路径 / SQL 等敏感内容）。
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    /// 404：请求的资源 / 接口不存在。
    pub fn not_found(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::NOT_FOUND, "NOT_FOUND", message)
    }

    /// 400：请求参数不合法。
    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, "BAD_REQUEST", message)
    }

    /// 401：未认证（S15+ 鉴权用）。
    pub fn unauthorized(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", message)
    }

    /// 403：已认证但无权限（S15+ 鉴权用）。
    pub fn forbidden(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::FORBIDDEN, "FORBIDDEN", message)
    }

    /// 409：与已有资源冲突（如重复提交）。
    pub fn conflict(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::CONFLICT, "CONFLICT", message)
    }

    /// 405：路径存在但方法不支持。
    pub fn method_not_allowed(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::METHOD_NOT_ALLOWED, "METHOD_NOT_ALLOWED", message)
    }

    /// 503：依赖不可用（健康检查探到数据库挂掉时用）。
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            message,
        )
    }

    /// 500：内部错误。完整原因只写日志，对外只给通用中文文案。
    pub fn internal(source: impl std::fmt::Display) -> Self {
        // 日志里保留完整原因，便于运维定位；响应体里一个字都不带。
        eprintln!("[server] 内部错误：{source}");
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL",
            INTERNAL_MESSAGE,
        )
    }

    /// HTTP 状态码。
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// 机器可读的稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 面向用户的中文文案。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 补充信息（可能为 null）。
    pub fn details(&self) -> &Value {
        &self.details
    }

    /// 按统一形状手写响应体。
    ///
    /// 响应体全项目手写 serde_json::Value（不引 serde derive）；构造集中在这一处，
    /// handler 与测试不会各写一份而漂移。
    pub fn body(&self) -> Value {
        let mut error = serde_json::Map::new();
        let _ = error.insert("code".to_string(), Value::String(self.code.to_string()));
        let _ = error.insert("message".to_string(), Value::String(self.message.clone()));
        let _ = error.insert("details".to_string(), self.details.clone());

        let mut root = serde_json::Map::new();
        let _ = root.insert("error".to_string(), Value::Object(error));
        Value::Object(root)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Json<Value> 会带上 Content-Type: application/json。
        (self.status, Json(self.body())).into_response()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 已有错误类型 转为 ApiError
//
// 有了这些 From，handler 里用问号就能把服务层 / 仓库层 / 连接池的错误收敛进来。
// 口径：
//   * 属于「客户端能理解」的业务分支（曲目不存在、唯一冲突）转为 4xx + 安全中文；
//   * 其余一律转为 500 INTERNAL，完整原因进日志、不透传细节。
// ─────────────────────────────────────────────────────────────────────────────

/// 连接池错误：内部故障，细节只进日志。
impl From<DbPoolError> for ApiError {
    fn from(e: DbPoolError) -> Self {
        ApiError::internal(e)
    }
}

/// 仓库层错误：唯一冲突是正常业务分支，其余算内部故障。
impl From<RepoError> for ApiError {
    fn from(e: RepoError) -> Self {
        if matches!(&e, RepoError::Conflict { .. }) {
            return ApiError::conflict("相同记录已存在，请勿重复提交");
        }
        ApiError::internal(e)
    }
}

/// 曲库服务错误：无法扫描属于服务端配置 / 依赖问题，对外不区分细节。
impl From<LibraryError> for ApiError {
    fn from(e: LibraryError) -> Self {
        ApiError::internal(e)
    }
}

/// 刮削服务错误：曲目不在库是正常业务分支，其余算内部故障。
impl From<ScrapeError> for ApiError {
    fn from(e: ScrapeError) -> Self {
        if matches!(&e, ScrapeError::SongNotFound { .. }) {
            return ApiError::not_found("请求的曲目不存在");
        }
        ApiError::internal(e)
    }
}

/// handler 里直接写裸 SQL 时的 rusqlite 错误。
impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError::internal(e)
    }
}

/// Json 提取器拒绝请求体：属于客户端问题，但原始文案可能含字段路径，统一换固定中文。
impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        eprintln!("[server] 请求体解析失败：{e}");
        ApiError::bad_request("请求体不是合法的 JSON")
    }
}
