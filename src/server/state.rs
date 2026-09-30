//! S14 · 应用状态（axum 的 State）。
//!
//! ## 为什么是 Arc + Clone
//!
//! axum 的 state 会在每个请求上被克隆一次，所以它必须 Clone 且廉价。这里所有字段
//! 都是 Arc<_>，克隆只加一次引用计数，不会复制连接池或配置。
//!
//! ## 同步阻塞服务的调用约定（重要）
//!
//! LibraryService::scan / ScrapeService 全是同步阻塞实现（std::fs + rusqlite），
//! 直接在 async handler 里调用会占死 tokio 的工作线程，请求一多就把运行时拖垮。
//!
//! 分两种：
//!   * **短**阻塞（一次查库 / 探活，毫秒级）→ tokio::task::spawn_blocking 包起来再
//!     await；
//!   * **长**任务（扫描 / 刮削，几秒到几分钟）→ 不能占 blocking 线程池，必须交给
//!     crate::server::jobs 的注册表，由它 spawn 独立 OS 线程（见 routes::jobs）。
//!
//! 完整理由与现成范例见 [crate::server] 模块头「同步阻塞服务的铁律」。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use crate::db::pool::DbPool;
use crate::plugin::{LoadReport, PluginRegistry};
use crate::service::{BatchRunner, LibraryService, ScrapeService};
use crate::watcher::SelfWriteRegistry;

use super::error::ApiError;
use super::jobs::JobRegistry;

/// 插件工作目录根的名字：落在系统临时目录下。
///
/// work_dir 是**一次插件调用内**的临时产物（池读完封面就删），所以不跟
/// `audio.cache_dir`（转码缓存）混在一起，也没有单独开配置项。
const PLUGIN_WORK_ROOT: &str = "music-robot-plugin-work";

/// 全局共享状态。
///
/// axum 每个请求都会 clone 一次 State，所以字段一律是 Arc<_>：克隆只加引用计数，
/// 不复制连接池 / 配置 / 服务。要加字段就在这里加。
#[derive(Clone)]
pub struct AppState {
    /// SQLite 连接池（所有仓库 / 服务共用同一份）。
    pub db: Arc<DbPool>,
    /// 生效配置（只读快照）。
    pub config: Arc<Config>,
    /// 曲库服务句柄。它自己是同步阻塞的，调用方式见模块级注释。
    pub library: Arc<LibraryService>,
    /// 后台长任务注册表：扫描 / 刮削的单例锁 + 状态快照。
    pub jobs: Arc<JobRegistry>,
    /// 批量刮削执行器。同步阻塞，调用方必须把它搬进独立 OS 线程
    /// （std::thread），**不要**用 spawn_blocking —— 见模块级注释。
    pub scrape: Arc<BatchRunner>,
    /// 启动时扫描插件目录的结果：加载了哪些、跳过了哪些及原因。
    ///
    /// 单独存一份的原因：刮削没工作时，「是没插件、还是插件全坏了」必须能一眼看到。
    /// 两个出口：启动时打 stderr（带跳过原因，含路径），以及 GET /healthz 的 `plugins`
    /// 字段（公开端点，只给文件名与跳过数量，**不给原因**）。
    pub plugin_report: Arc<LoadReport>,
}

impl AppState {
    /// 用连接池 + 配置组装状态（曲库服务从 storage 段构造，刮削执行器从 scrape 段构造）。
    ///
    /// **插件接线**：启动时扫 `plugins.dir`、解析每个文件的清单、按文件名升序组装刮削
    /// 插件表交给 ScrapeService。此前这里传的是 `Vec::new()`，于是每一首歌都会落到
    /// ScrapeService 的「没有配置任何刮削插件」分支被标记 failed —— 刮削整体是死的。
    pub fn new(db: Arc<DbPool>, config: Arc<Config>) -> Self {
        let library = Arc::new(LibraryService::from_config(
            Arc::clone(&db),
            &config.storage,
        ));
        // 自写抑制窗口与 watcher 段共用同一个配置项，避免两处各写一套默认值。
        let self_write = Arc::new(SelfWriteRegistry::new(Duration::from_millis(
            config.watcher.self_write_suppress_ms,
        )));

        let registry = load_plugin_registry(&config);
        let plugin_report = Arc::new(registry.report().clone());
        let scrape_service = ScrapeService::new(
            Arc::clone(&db),
            registry.into_plugins(),
            config.scrape.clone(),
            self_write,
        );
        AppState {
            db,
            config,
            library,
            jobs: Arc::new(JobRegistry::new()),
            scrape: Arc::new(BatchRunner::new(Arc::new(scrape_service))),
            plugin_report,
        }
    }

    /// 健康检查用的数据库探活：借一条连接执行 SELECT 1。
    ///
    /// 同步阻塞（会等连接、会读磁盘），async 上下文里必须 spawn_blocking。
    /// 失败一律折叠成 503 + 通用中文：不把连接池 / SQLite 的原始报错透给客户端。
    pub fn probe_db(&self) -> Result<(), ApiError> {
        if let Err(detail) = self.probe_db_inner() {
            crate::serverlog::warn("db", format!("健康检查：数据库探活失败：{detail}"));
            return Err(ApiError::service_unavailable("数据库暂时不可用"));
        }
        Ok(())
    }

    /// 探活的实现：把不同类型的错误统一成字符串（这里只用于日志）。
    fn probe_db_inner(&self) -> Result<(), String> {
        let conn = self.db.acquire().map_err(|e| e.to_string())?;
        conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// 扫插件目录、组装注册表，并把加载报告打到 stderr。
///
/// 打日志不是可选装饰：目录不存在、插件全被跳过、kind 不是 scraper……这些都不会让
/// 启动失败；不显式打出来，「刮削为什么没工作」就会变成排查噩梦。
fn load_plugin_registry(config: &Config) -> PluginRegistry {
    let dir = PathBuf::from(&config.plugins.dir);
    let work_root = std::env::temp_dir().join(PLUGIN_WORK_ROOT);
    let registry = PluginRegistry::load(&dir, config.plugins.sandbox_config(), work_root);
    log_load_report(&dir, registry.report());
    registry
}

/// 把加载报告写 stderr：加载了哪些、跳过了哪些及原因；一个都没加载到时给醒目警告。
fn log_load_report(dir: &Path, report: &LoadReport) {
    if report.loaded.is_empty() {
        if dir.is_dir() {
            crate::serverlog::warn(
                "plugin",
                format!(
                    "插件目录「{}」里一个可用的刮削插件都没有，POST /api/scrape 会把所有歌标记为 failed。\
                     请放入 scraper 单文件插件（可参考 plugins/example.js），或改 plugins.dir / MR_PLUGINS_DIR。",
                    dir.display()
                ),
            );
        } else {
            crate::serverlog::warn(
                "plugin",
                format!(
                    "插件目录「{}」不存在，没有任何刮削插件，POST /api/scrape 会把所有歌标记为 failed。\
                     请把插件放进去，或改 plugins.dir / MR_PLUGINS_DIR 指向正确目录。",
                    dir.display()
                ),
            );
        }
    } else {
        crate::serverlog::info(
            "plugin",
            format!(
                "已加载 {} 个刮削插件（按文件名升序尝试、命中即停）：{}",
                report.loaded.len(),
                report.loaded.join(" / ")
            ),
        );
    }
    for (file, reason) in &report.skipped {
        crate::serverlog::warn("plugin", format!("跳过「{file}」：{reason}"));
    }
}
