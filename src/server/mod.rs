//! S14 · HTTP 服务骨架（axum）。
//!
//! 这一层只搭框架，不含业务：
//!
//! * [`state`]  —— AppState（连接池 + 配置 + 服务句柄，Arc 共享、可 Clone）；
//! * [`error`]  —— ApiError（统一错误形状 { error: { code, message, details } }）；
//! * [`routes`] —— build_router（不绑端口，测试可直接调用）+ 健康检查 / 404 / CORS；
//! * [`jobs`]   —— S17 后台长任务注册表（扫描 / 刮削的单例锁 + 状态快照）。
//!
//! ## 组装与启动分开
//!
//! [`build_router`] 只拼 Router，不碰 socket，测试可以 oneshot 直调；
//! [`run`] 才负责 TcpListener::bind + axum::serve。这样「启动成功」也能在测试里
//! 绑 127.0.0.1:0（内核分配端口）真跑一次，不必占固定端口。
//!
//! ## 同步阻塞服务的铁律（重要）
//!
//! LibraryService::scan / ScrapeService / 健康检查的探活都是同步阻塞实现
//! （std::fs + rusqlite）。**绝不能在 async handler 里直接调用**：那会占死 tokio 的
//! 工作线程，请求一多就把运行时拖垮。分两种调用方式：
//!
//!   * **短**阻塞（探活 / 一次查库，毫秒级）→ 用 `tokio::task::spawn_blocking`
//!     包起来再 await。现成范例见 [`routes`] 里的 `healthz`（探一次 SELECT 1），
//!     以及 `routes::library::run_db`（所有单次查库都走它）；
//!   * **长**任务（扫描 / 刮削，几秒到几分钟）→ **不要**用 spawn_blocking：它会把
//!     blocking 线程池占满，连鉴权查库都会跟着饿死。必须交给 [`jobs`] 注册表
//!     spawn 独立 OS 线程，handler 立刻返回 202。
//!
//! 这条约定原先靠 S14 的示例路由 `/api/v1/_example/blocking` 演示，现在示例路由已删
//! （真实路由齐了），约定本身挪到这里，并由 `healthz` / `run_db` 两个真实调用点守着。

pub mod auth;
pub mod error;
pub mod jobs;
pub mod routes;
pub mod state;

#[cfg(test)]
mod tests;

pub use auth::{AdminUser, AuthUser};
pub use error::{ApiError, ApiResult};
pub use routes::build_router;
pub use state::AppState;

use std::time::Instant;

use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;

/// 请求日志中间件：一行一条 `METHOD uri -> 状态码 (耗时)`。
///
/// 级别按结果分档：5xx → `error`，4xx → `warn`，其余 → `info`。
/// `/healthz` 会被探活频繁打到，**降到 debug**，免得刷屏把有用的行冲掉。
///
/// ⚠️ **绝不打印请求体，也不打印 Authorization 头** —— 注册 / 登录的 body 里有明文口令，
/// 令牌进了日志文件就等于长期泄漏。只记方法与路径。
///
/// ⚠️ 只在 [`run`] 里挂，**不挂进 `build_router`**：测试全部走 `build_router`，
/// 挂那里会把每个用例的请求都刷出来。
async fn access_log(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16();
    let ms = start.elapsed().as_millis();
    let line = format!("{method} {uri} -> {status} ({ms}ms)");

    if uri.path() == "/healthz" {
        crate::serverlog::debug("http", line);
    } else if status >= 500 {
        crate::serverlog::error("http", line);
    } else if status >= 400 {
        crate::serverlog::warn("http", line);
    } else {
        crate::serverlog::info("http", line);
    }
    resp
}

/// 启动 HTTP 服务：绑定配置里的 host:port 并跑 axum，直到收到停机信号。
///
/// 返回 std::io::Result：绑定失败 / serve 出错都会如实返回，由调用方决定怎么报。
pub async fn run(state: AppState) -> std::io::Result<()> {
    let server_cfg = &state.config.server;
    let addr = format!("{}:{}", server_cfg.host, server_cfg.port);
    let app = build_router(state).layer(middleware::from_fn(access_log));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    // 端口配 0 时这里打的是内核实际分配的端口，方便本地起服务时看真实地址。
    let local = listener.local_addr()?;
    crate::serverlog::info("server", format!("已监听 http://{local}"));

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
}

/// 停机信号：Ctrl-C（SIGINT）或 SIGTERM 一到就返回，让 axum 停止接收新连接、
/// 等在途请求收尾（优雅关闭）。
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            crate::serverlog::error("server", format!("监听 Ctrl-C 失败：{e}"));
            // 永远挂起：宁可关不掉，也不要因为注册失败就误触发停机。
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                crate::serverlog::error("server", format!("监听 SIGTERM 失败：{e}"));
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => crate::serverlog::info("server", "收到中断信号，开始优雅关闭"),
        _ = terminate => crate::serverlog::info("server", "收到终止信号，开始优雅关闭"),
    }
}
