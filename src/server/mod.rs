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

/// 启动 HTTP 服务：绑定配置里的 host:port 并跑 axum，直到收到停机信号。
///
/// 返回 std::io::Result：绑定失败 / serve 出错都会如实返回，由调用方决定怎么报。
pub async fn run(state: AppState) -> std::io::Result<()> {
    let server_cfg = &state.config.server;
    let addr = format!("{}:{}", server_cfg.host, server_cfg.port);
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    // 端口配 0 时这里打印的是内核实际分配的端口，方便本地起服务时看真实地址。
    let local = listener.local_addr()?;
    eprintln!("[server] music-robot 已监听 http://{local}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
}

/// 停机信号：Ctrl-C（SIGINT）或 SIGTERM 一到就返回，让 axum 停止接收新连接、
/// 等在途请求收尾（优雅关闭）。
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            eprintln!("[server] 监听 Ctrl-C 失败：{e}");
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
                eprintln!("[server] 监听 SIGTERM 失败：{e}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
