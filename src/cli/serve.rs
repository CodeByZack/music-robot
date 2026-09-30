//! serve 子命令 —— 启动 HTTP 服务（前台运行）。
//!
//! ## 为什么这里自己建 tokio 运行时
//!
//! `cli::run` 是一个**同步**的纯函数（返回退出码、不起子进程、不 async），
//! read / write / blank / scan / doctor / wash 六个命令都靠这个性质被测试直接调用。
//! 为了一个 serve 把整条链改成 async 得不偿失，所以这里用 `Runtime::block_on`
//! 把异步的 `server::run` 桥接成同步调用 —— 异步只存在于这一层里面。

use std::path::Path;
use std::sync::Arc;

use crate::cli::args::{parse_args, wants_help, ParsedArgs, UsageError};
use crate::cli::io::CommandIO;
use crate::config::Config;
use crate::db::{migrations, pool::DbPool};
use crate::server::{self, AppState};

pub const SERVE_USAGE: &str = r#"用法: music-robot serve [options]

启动 HTTP 服务（前台运行；Ctrl-C 或 SIGTERM 优雅退出）。

选项:
  --host <host>     监听地址（覆盖配置里的 server.host）
  --port <port>     监听端口（覆盖配置里的 server.port；0 = 由内核分配）
  --config <path>   配置文件路径（不传则用内置默认值 + MR_* 环境变量）

退出码: 0 = 正常关闭；1 = 启动/配置/迁移失败；2 = 用法错误"#;

/// 取单值 flag：没出现 → None；重复出现 → 用法错误。
fn flag_value(parsed: &ParsedArgs, name: &str) -> Result<Option<String>, UsageError> {
    match parsed.flags.get(name) {
        Some(f) => f.single(name),
        None => Ok(None),
    }
}

pub fn run_serve(argv: &[&str], io: &dyn CommandIO) -> Result<i32, UsageError> {
    if wants_help(argv) {
        io.log(SERVE_USAGE);
        return Ok(0);
    }
    let parsed = parse_args(argv)?;
    parsed.reject_unknown(
        &["host", "port", "config"],
        "serve 支持: --host / --port / --config",
    )?;
    if !parsed.positionals.is_empty() {
        io.error("serve 不接受位置参数\n");
        io.error(SERVE_USAGE);
        return Ok(2);
    }

    // ── 1) 配置：内置默认值 → 文件 → 环境变量 → 命令行（后者覆盖前者）──
    let mut cfg = match flag_value(&parsed, "config")? {
        Some(path) => match Config::load(Path::new(&path)) {
            Ok(c) => c,
            Err(e) => {
                io.error(&format!("配置文件加载失败：{e}"));
                return Ok(1);
            }
        },
        None => Config::defaults(),
    };
    // `.env` 兜底：dotenvy 从当前目录**及其父目录**找 `.env` 并注入进程环境，
    // 且**不覆盖已存在的变量** —— 正好就是「真环境变量优先」的语义。
    // 找不到 `.env` 是常态（全新克隆就没有），不算错误。
    if let Ok(path) = dotenvy::dotenv() {
        io.log(&format!("已加载 .env：{}", path.display()));
    }
    if let Err(e) = cfg.apply_env(&|k| std::env::var(k).ok()) {
        io.error(&format!("环境变量配置有误：{e}"));
        return Ok(1);
    }
    if let Some(host) = flag_value(&parsed, "host")? {
        cfg.server.host = host;
    }
    if let Some(port) = flag_value(&parsed, "port")? {
        match port.parse::<u16>() {
            Ok(n) => cfg.server.port = n,
            Err(_) => {
                io.error(&format!("--port 必须是 0..65535 的整数，实际：{port}"));
                return Ok(2);
            }
        }
    }
    if let Err(e) = cfg.validate() {
        io.error(&format!("配置校验失败：{e}"));
        return Ok(1);
    }

    // ── 2) 日志：先初始化，之后所有输出都走它（写文件 + 回显 stderr）──
    // 级别已在 validate 里校验过，解析不会失败；仍不 unwrap，退回 info 兜底。
    let log_level = crate::serverlog::Level::parse(&cfg.log.level)
        .unwrap_or(crate::serverlog::Level::Info);
    match crate::serverlog::init(Path::new(&cfg.log.dir), log_level, cfg.log.keep_days) {
        Ok(path) => crate::serverlog::info("server", format!("日志写入 {}", path.display())),
        Err(e) => io.error(&format!(
            "日志目录不可用（{}）：{e} —— 日志只回显终端，不落盘",
            cfg.log.dir
        )),
    }
    crate::serverlog::info(
        "server",
        format!("music-robot v{} 启动", env!("CARGO_PKG_VERSION")),
    );
    crate::serverlog::info(
        "server",
        format!(
            "配置：曲库根 {:?} · 数据库 {} · 缓存 {} · 插件目录 {} · 日志级别 {}",
            cfg.storage.library_roots,
            cfg.database.path,
            cfg.audio.cache_dir,
            cfg.plugins.dir,
            cfg.log.level
        ),
    );

    // ── 3) 启动维护：清一次过期的转码缓存 ──
    // 只在这一刻清，**不是**每次转码都清、也没有定时任务：清理是尽力而为的磁盘治理，
    // 常驻期反复扫目录只会白耗 IO。删除判据（为什么不会误删用户文件）见
    // crate::audio::transcode::prune_cache。
    let pruned = crate::audio::transcode::prune_cache(
        Path::new(&cfg.audio.cache_dir),
        cfg.audio.cache_expiry_days,
        std::time::SystemTime::now(),
    );
    if pruned.removed > 0 || pruned.failed > 0 {
        crate::serverlog::info(
            "cache",
            format!(
                "转码缓存清理：删除 {} 个过期文件，保留 {} 个，{} 个删不掉（目录 {}）",
                pruned.removed, pruned.kept, pruned.failed, cfg.audio.cache_dir
            ),
        );
    }

    // ── 4) 数据库 ──
    // 先把父目录建出来：默认路径是 ~/.local/share/music-robot/music.db，
    // 首次部署时那个目录并不存在，SQLite 不会替我们建，直接开库会失败。
    if let Some(parent) = Path::new(&cfg.database.path).parent() {
        // 空 parent（纯文件名）时 create_dir_all("") 会报错，跳过即可
        if !parent.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                io.error(&format!("创建数据库目录失败（{}）：{e}", parent.display()));
                return Ok(1);
            }
        }
    }
    let pool = match DbPool::new(&cfg.database.path) {
        Ok(p) => p,
        Err(e) => {
            io.error(&format!("打开数据库失败（{}）：{e}", cfg.database.path));
            return Ok(1);
        }
    };
    // 迁移必须先跑：否则 /healthz 的 SELECT 1 能过，业务查询却因缺表而炸。
    {
        let mut conn = match pool.acquire() {
            Ok(c) => c,
            Err(e) => {
                io.error(&format!("从连接池取连接失败：{e}"));
                return Ok(1);
            }
        };
        match migrations::apply(&mut conn) {
            Ok(report) => {
                if report.changed() {
                    crate::serverlog::info(
                        "db",
                        format!(
                            "数据库迁移：v{} → v{}（应用 {} 条）",
                            report.from_version,
                            report.to_version,
                            report.applied.len()
                        ),
                    );
                }
            }
            Err(e) => {
                io.error(&format!("数据库迁移失败：{e}"));
                return Ok(1);
            }
        }
    }

    // ── 5) 起服务（异步只活在这一层里面）──
    let state = AppState::new(Arc::new(pool), Arc::new(cfg));
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            io.error(&format!("创建 tokio 运行时失败：{e}"));
            return Ok(1);
        }
    };
    match rt.block_on(server::run(state)) {
        Ok(()) => {
            crate::serverlog::info("server", "服务已停止");
            Ok(0)
        }
        Err(e) => {
            io.error(&format!("服务运行失败：{e}"));
            Ok(1)
        }
    }
}