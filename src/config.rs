//! 配置模块 —— 服务端全部可调项的唯一来源。
//!
//! 设计要点：
//!
//! * **来源优先级**：命令行参数 > 环境变量 > 配置文件 > 内置默认值。
//!   本模块负责后三层：配置文件的读入走 [`Config::load`]，环境变量层走
//!   [`Config::apply_env`]；命令行参数由 CLI 层在 [`Config::validate`] 之前
//!   直接改 `pub` 字段（所以所有字段都是公开的）。组合示例：
//!
//!   ```ignore
//!   let mut cfg = Config::load(Path::new("config.json"))?;  // 缺失 → 内置默认值
//!   cfg.apply_env(&|k| std::env::var(k).ok())?;             // MR_* 覆盖
//!   cfg.server.port = cli.port;                             // CLI 覆盖（最高优先级）
//!   cfg.validate()?;
//!   ```
//!
//! * **默认值复用实现里的常量**：[`watcher::suppress::DEFAULT_TTL`]、
//!   [`plugin::manifest::DEFAULT_TIMEOUT_MS`]、[`plugin::sandbox::DEFAULT_MEMORY_MB`] 等。
//!   配置里绝不重写第二份魔数，这样「设计稿写 512、代码默认 256」式的漂移不可能发生。
//!   唯一的例外是那些**没有实现常量可复用**的展示性默认值（端口、并发数等），
//!   它们只出现在本文件的 [`Config::defaults`] 一处。
//!
//! * **手写 `serde_json::Value` 解析**（项目不引 serde derive）。未知 JSON 字段一律
//!   忽略：旧二进制要能读新配置文件，向前兼容。
//!
//! * **路径一律在内存里展开 `~`**（见 [`expand_tilde_with`]）：`Config` 里拿到的
//!   `database.path` / `audio.cache_dir` / `log.dir` / `plugins.dir` 可以直接用，
//!   不需要调用方再展开。
//!   `HOME` 取不到时原样保留 `~`，不报错、不 panic。
//!
//! * **敏感项不落配置文件**：`server.jwt_secret` 只从环境变量 `MR_JWT_SECRET`
//!   或 CLI 来；配置文件里写了也会被接受（便于本地调试），但默认值是空串。

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// `plugins.sandbox` 允许的取值（与 S12 沙箱的实现能力一一对应）。
/// `auto` = 启动时探测，从强到弱降级。
pub const SANDBOX_MODES: [&str; 7] =
    ["auto", "none", "posix", "dropped", "bwrap", "systemd", "docker"];

/// 运行期数据根目录（`~` 在 [`Config::defaults`] 里按 `HOME` 展开）。
///
/// 数据库 / 转码缓存 / 日志**都放在它下面**，所以只需要一个 `MR_DATA_DIR`
/// 就能把这三样一起搬家，不必分别设三个变量。
const DEFAULT_DATA_DIR: &str = "~/.local/share/music-robot";

/// 数据根下的某一项（`~` 已展开）。`leaf` 是相对路径，如 `music.db` / `transcode`。
fn data_dir_path(leaf: &str) -> String {
    format!("{}/{}", expand_tilde(DEFAULT_DATA_DIR), leaf)
}
/// 插件目录缺省值。**相对进程 cwd**，所以不做 `~` 展开（也允许用户显式写 `~/plugins`）。
const DEFAULT_PLUGINS_DIR: &str = "plugins";

/// 库根的环境变量名。
///
/// 单独提成常量是因为它不只在这里被读：`GET /api/library/stats` 要把
/// **「环境变量这一层压过了 config.json」**这件事告诉界面 —— 配置优先级是
/// CLI > 环境变量 > config.json > 默认值，而 `serve` 是「先读文件、再
/// apply_env()」。所以环境变量一旦设了，用户改 config.json 就是白改。
/// 界面上要显示变量名，两处写死的字符串迟早会漂，引用同一个常量才是单一口径。
pub const LIBRARY_ROOTS_ENV: &str = "MR_LIBRARY_ROOTS";

/// 服务端配置总入口。
///
/// 每个顶层段对应设计稿里的一个 JSON 段，字段类型都是具体类型（不用 `Option` 兜底）：
/// 「没写」= 回落 [`Config::defaults`]，配置文件里写 `null` 也按「没写」处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// HTTP 服务：监听地址 / JWT / token 有效期
    pub server: ServerConfig,
    /// 存储：后端类型 + 曲库根目录（可多根）
    pub storage: StorageConfig,
    /// 数据库
    pub database: DatabaseConfig,
    /// 音频：转码 / 缓存 / ffmpeg
    pub audio: AudioConfig,
    /// 曲库文件监听
    pub watcher: WatcherConfig,
    /// 刮削任务调度
    pub scrape: ScrapeConfig,
    /// 插件目录 + 运行方式 + 资源限制
    pub plugins: PluginsConfig,
    /// 日志
    pub log: LogConfig,
}

/// `server` 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// 监听地址（`0.0.0.0` = 所有网卡，`127.0.0.1` = 只本机）
    pub host: String,
    /// 监听端口（`0` 非法，见 [`Config::validate`]）
    pub port: u16,
    /// JWT 签名密钥。**敏感项，只走 `MR_JWT_SECRET` / CLI，不写配置文件**。
    /// 默认空串；服务端启动前必须由部署方填上。
    pub jwt_secret: String,
    /// access token 有效期（小时，必须 > 0）
    pub token_expiry_hours: u64,
}

/// `storage` 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageConfig {
    /// 存储后端类型，对应 JSON 键 `type`（`type` 是 Rust 关键字，字段名改叫 `kind`）。
    /// 目前只有 `local`。
    pub kind: String,
    /// 曲库根目录，**至少要有一个非空项**（见 [`Config::validate`]）
    pub library_roots: Vec<String>,
}

/// `database` 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    /// SQLite 文件路径（`~` 已展开）
    pub path: String,
}

/// `audio` 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfig {
    /// 是否按需转码（`false` = 只做直出）
    pub transcode: bool,
    /// 转码缓存目录（`~` 已展开）
    pub cache_dir: String,
    /// 缓存保留天数
    pub cache_expiry_days: u64,
    /// ffmpeg 可执行文件路径（`ffmpeg` = 走 PATH）
    pub ffmpeg_path: String,
}

/// `watcher` 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatcherConfig {
    /// 是否启用曲库监听
    pub enabled: bool,
    /// 事件去抖窗口（毫秒）
    pub debounce_ms: u64,
    /// 自写抑制窗口（毫秒）。默认取自
    /// [`crate::watcher::suppress::DEFAULT_TTL`]，与监听实现同一份常量。
    pub self_write_suppress_ms: u64,
}

/// `scrape` 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrapeConfig {
    /// 同时进行的刮削任务数（>= 1）
    pub concurrency: u32,
    /// 每批送进刮削器的曲目数（>= 1）
    pub batch_size: u32,
    /// 单次插件调用超时（秒）。默认取自
    /// [`crate::plugin::manifest::DEFAULT_TIMEOUT_MS`]。
    pub task_timeout_sec: u64,
    /// 失败重试次数
    pub max_retry: u32,
    /// 同一插件的相邻请求间隔（毫秒）
    pub request_delay_ms: u64,
    /// 撞上站点限流后的冷却时间（秒）
    pub rate_limit_cooldown_sec: u64,
    /// 插件池空闲多久回收 worker（秒）。默认取自
    /// [`crate::plugin::pool::DEFAULT_IDLE_TIMEOUT`]。
    ///
    /// 设计稿的配置样例里没有这一项（池空闲回收原先写死在代码里），但规格要求配置与
    /// 实现共用同一个常量，所以在这里显式暴露出来。
    pub pool_idle_timeout_sec: u64,
    /// 每个插件各自的并发上限（插件名 → 并发数）
    pub per_plugin_max: HashMap<String, u32>,
}

/// `plugins.limits` 段 —— 对应 [`crate::plugin::sandbox::SandboxConfig`] 的 rlimit。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLimits {
    /// RLIMIT_AS（MB），默认 [`crate::plugin::sandbox::DEFAULT_MEMORY_MB`]
    pub memory_mb: u64,
    /// RLIMIT_CPU（秒），默认 [`crate::plugin::sandbox::DEFAULT_CPU_SEC`]
    pub cpu_sec: u64,
    /// RLIMIT_NPROC，默认 [`crate::plugin::sandbox::DEFAULT_PROCS`]
    pub procs: u64,
    /// RLIMIT_FSIZE（MB，单文件），默认 [`crate::plugin::sandbox::DEFAULT_FILE_MB`]
    pub file_mb: u64,
}

/// `plugins` 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginsConfig {
    /// 插件目录（单文件脚本一层，不递归）。默认 `plugins`（相对进程 cwd）；
    /// 写 `~` 会展开成家目录，口径与 `database.path` / `audio.cache_dir` 一致。
    /// 目录不存在不算错：全新安装本来就没插件，扫描结果为空。
    pub dir: String,
    /// 沙箱模式，取值见 [`SANDBOX_MODES`]
    pub sandbox: String,
    /// 插件降权到的用户名（`none` / 空 = 不降权）
    pub plugin_user: String,
    /// 资源限制
    pub limits: PluginLimits,
}

impl PluginsConfig {
    /// 把 `plugins.limits` 映射成沙箱实现要的
    /// [`crate::plugin::sandbox::SandboxConfig`]。
    ///
    /// `nofile` 在配置里没有对应项（画布只列了内存 / CPU / 进程数 / 单文件四项），
    /// 用实现里的 [`crate::plugin::sandbox::DEFAULT_NOFILE`]，两边不会漂移。
    ///
    /// `plugin_user` 这里**不解析成 uid/gid**：用户名 → uid 需要查 passwd，而画布的口径
    /// 是「降权 uid 不存在时自动跳过并记日志」，属于插件进程启动时的职责。这里给 `None`
    /// 表示不降权，与 [`crate::plugin::sandbox`]「降权是尽力而为」的语义一致。
    pub fn sandbox_config(&self) -> crate::plugin::sandbox::SandboxConfig {
        crate::plugin::sandbox::SandboxConfig {
            memory_mb: self.limits.memory_mb,
            cpu_sec: self.limits.cpu_sec,
            procs: self.limits.procs,
            file_mb: self.limits.file_mb,
            nofile: crate::plugin::sandbox::DEFAULT_NOFILE,
            plugin_uid: None,
            plugin_gid: None,
        }
    }
}

/// `log` 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogConfig {
    /// 日志级别（trace / debug / info / warn / error）
    pub level: String,
    /// 日志目录（`~` 已展开）
    pub dir: String,
    /// 日志保留天数。按 UTC 日期一天一个文件，启动时清掉更早的；`0` = 不清理
    pub keep_days: u64,
}

/// 配置错误。
///
/// 全部带字段名（或文件路径），`Display` 是中文，方便直接打给用户看。
#[derive(Debug)]
pub enum ConfigError {
    /// 读文件失败（权限、是目录……）。**文件不存在不算错**，见 [`Config::load`]。
    Io {
        /// 出错的文件
        path: PathBuf,
        /// 底层 IO 错误
        source: std::io::Error,
    },
    /// JSON 语法错误
    BadJson {
        /// 出错的文件；`None` = 不是从文件来的
        path: Option<PathBuf>,
        /// 底层解析错误
        source: serde_json::Error,
    },
    /// 类型不对（如 `port` 写成了字符串）
    BadType {
        /// 字段路径，如 `server.port`
        field: &'static str,
        /// 期望的类型
        expected: &'static str,
    },
    /// 类型对但取值不在允许范围内
    BadValue {
        /// 字段路径
        field: &'static str,
        /// 实际拿到的值（转成字符串，便于打印）
        value: String,
        /// 允许的取值范围
        allowed: String,
    },
    /// 必填项为空
    Empty {
        /// 字段路径
        field: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "读取配置文件失败（{}）：{source}", path.display())
            }
            ConfigError::BadJson { path, source } => match path {
                Some(p) => write!(f, "配置文件 JSON 解析失败（{}）：{source}", p.display()),
                None => write!(f, "配置 JSON 解析失败：{source}"),
            },
            ConfigError::BadType { field, expected } => {
                write!(f, "配置字段 {field} 类型错误：应为{expected}")
            }
            ConfigError::BadValue {
                field,
                value,
                allowed,
            } => {
                write!(f, "配置字段 {field} 取值非法：{value}（允许：{allowed}）")
            }
            ConfigError::Empty { field } => {
                write!(f, "配置字段 {field} 不能为空")
            }
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Io { source, .. } => Some(source),
            ConfigError::BadJson { source, .. } => Some(source),
            ConfigError::BadType { .. }
            | ConfigError::BadValue { .. }
            | ConfigError::Empty { .. } => None,
        }
    }
}

impl Config {
    /// 内置默认值。
    ///
    /// 资源限制 / 超时 / 抑制窗口这些数字**全部取自实现里的 `DEFAULT_*` 常量**，
    /// 改实现就会自动改默认配置，两边不会漂移。
    pub fn defaults() -> Self {
        Config {
            server: ServerConfig {
                host: "0.0.0.0".to_string(),
                port: 8080,
                // 敏感项：不落配置文件，等 MR_JWT_SECRET 或 CLI 来填
                jwt_secret: String::new(),
                token_expiry_hours: 24,
            },
            storage: StorageConfig {
                kind: "local".to_string(),
                // 曲库根不能为空（validate 会拒绝），默认给容器里最常见的挂载点
                library_roots: vec!["/music".to_string()],
            },
            database: DatabaseConfig {
                path: data_dir_path("music.db"),
            },
            audio: AudioConfig {
                transcode: true,
                cache_dir: data_dir_path("transcode"),
                cache_expiry_days: 30,
                ffmpeg_path: "ffmpeg".to_string(),
            },
            watcher: WatcherConfig {
                enabled: true,
                debounce_ms: 500,
                // 与监听实现同一个常量：5s → 5000ms
                self_write_suppress_ms: crate::watcher::suppress::DEFAULT_TTL.as_millis() as u64,
            },
            scrape: ScrapeConfig {
                concurrency: 3,
                batch_size: 100,
                // 与插件清单缺省超时同一个常量：30_000ms → 30s
                task_timeout_sec: crate::plugin::manifest::DEFAULT_TIMEOUT_MS / 1000,
                max_retry: 2,
                request_delay_ms: 500,
                rate_limit_cooldown_sec: 60,
                // 与 worker 池缺省空闲回收同一个常量：300s
                pool_idle_timeout_sec: crate::plugin::pool::DEFAULT_IDLE_TIMEOUT.as_secs(),
                // 与设计稿 per_plugin_max 逐项一致（netease/qqmusic/musicbrainz）
                per_plugin_max: HashMap::from([
                    ("netease".to_string(), 1u32),
                    ("qqmusic".to_string(), 1u32),
                    ("musicbrainz".to_string(), 2u32),
                ]),
            },
            plugins: PluginsConfig {
                // 相对进程 cwd；不展开 ~，因为默认值里根本没有 ~
                dir: DEFAULT_PLUGINS_DIR.to_string(),
                sandbox: "auto".to_string(),
                plugin_user: "music-plugin".to_string(),
                limits: PluginLimits {
                    memory_mb: crate::plugin::sandbox::DEFAULT_MEMORY_MB,
                    cpu_sec: crate::plugin::sandbox::DEFAULT_CPU_SEC,
                    procs: crate::plugin::sandbox::DEFAULT_PROCS,
                    file_mb: crate::plugin::sandbox::DEFAULT_FILE_MB,
                },
            },
            log: LogConfig {
                level: "info".to_string(),
                dir: data_dir_path("logs"),
                keep_days: 7,
            },
        }
    }

    /// 从 JSON 文件加载。
    ///
    /// **文件不存在不算错误**：直接返回 [`Config::defaults`]（首次启动就是这条路）。
    /// 文件存在但读不了 / JSON 非法才返回 `Err`。
    ///
    /// 本方法只管「配置文件」这一层，不碰进程环境变量 —— 环境变量层请显式调用
    /// [`Config::apply_env`]，这样覆盖顺序对调用方和测试都是显式的，也便于注入假环境。
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::defaults()),
            Err(e) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source: e,
                })
            }
        };
        let value: Value = serde_json::from_str(&text).map_err(|e| ConfigError::BadJson {
            path: Some(path.to_path_buf()),
            source: e,
        })?;
        Self::from_value(&value)
    }

    /// 从已解析的 JSON 构造（不碰文件系统，便于测试）。
    ///
    /// 未知字段忽略；缺失字段回落 [`Config::defaults`]；`null` 等价于「没写」。
    pub fn from_value(v: &Value) -> Result<Self, ConfigError> {
        let root = expect_object(v, "config")?;
        let mut cfg = Self::defaults();

        if let Some(seg) = segment(root, "server")? {
            if let Some(x) = str_at(seg, "host", "server.host")? {
                cfg.server.host = x;
            }
            if let Some(x) = u16_at(seg, "port", "server.port")? {
                cfg.server.port = x;
            }
            if let Some(x) = str_at(seg, "jwt_secret", "server.jwt_secret")? {
                cfg.server.jwt_secret = x;
            }
            if let Some(x) = u64_at(seg, "token_expiry_hours", "server.token_expiry_hours")? {
                cfg.server.token_expiry_hours = x;
            }
        }

        if let Some(seg) = segment(root, "storage")? {
            if let Some(x) = str_at(seg, "type", "storage.type")? {
                cfg.storage.kind = x;
            }
            if let Some(x) = str_vec_at(seg, "library_roots", "storage.library_roots")? {
                cfg.storage.library_roots = x;
            }
        }

        if let Some(seg) = segment(root, "database")? {
            if let Some(x) = str_at(seg, "path", "database.path")? {
                cfg.database.path = expand_tilde(&x);
            }
        }

        if let Some(seg) = segment(root, "audio")? {
            if let Some(x) = bool_at(seg, "transcode", "audio.transcode")? {
                cfg.audio.transcode = x;
            }
            if let Some(x) = str_at(seg, "cache_dir", "audio.cache_dir")? {
                cfg.audio.cache_dir = expand_tilde(&x);
            }
            if let Some(x) = u64_at(seg, "cache_expiry_days", "audio.cache_expiry_days")? {
                cfg.audio.cache_expiry_days = x;
            }
            if let Some(x) = str_at(seg, "ffmpeg_path", "audio.ffmpeg_path")? {
                cfg.audio.ffmpeg_path = x;
            }
        }

        if let Some(seg) = segment(root, "watcher")? {
            if let Some(x) = bool_at(seg, "enabled", "watcher.enabled")? {
                cfg.watcher.enabled = x;
            }
            if let Some(x) = u64_at(seg, "debounce_ms", "watcher.debounce_ms")? {
                cfg.watcher.debounce_ms = x;
            }
            if let Some(x) = u64_at(
                seg,
                "self_write_suppress_ms",
                "watcher.self_write_suppress_ms",
            )? {
                cfg.watcher.self_write_suppress_ms = x;
            }
        }

        if let Some(seg) = segment(root, "scrape")? {
            if let Some(x) = u32_at(seg, "concurrency", "scrape.concurrency")? {
                cfg.scrape.concurrency = x;
            }
            if let Some(x) = u32_at(seg, "batch_size", "scrape.batch_size")? {
                cfg.scrape.batch_size = x;
            }
            if let Some(x) = u64_at(seg, "task_timeout_sec", "scrape.task_timeout_sec")? {
                cfg.scrape.task_timeout_sec = x;
            }
            if let Some(x) = u32_at(seg, "max_retry", "scrape.max_retry")? {
                cfg.scrape.max_retry = x;
            }
            if let Some(x) = u64_at(seg, "request_delay_ms", "scrape.request_delay_ms")? {
                cfg.scrape.request_delay_ms = x;
            }
            if let Some(x) = u64_at(
                seg,
                "rate_limit_cooldown_sec",
                "scrape.rate_limit_cooldown_sec",
            )? {
                cfg.scrape.rate_limit_cooldown_sec = x;
            }
            if let Some(x) = u64_at(
                seg,
                "pool_idle_timeout_sec",
                "scrape.pool_idle_timeout_sec",
            )? {
                cfg.scrape.pool_idle_timeout_sec = x;
            }
            if let Some(x) = u32_map_at(seg, "per_plugin_max", "scrape.per_plugin_max")? {
                cfg.scrape.per_plugin_max = x;
            }
        }

        if let Some(seg) = segment(root, "plugins")? {
            if let Some(x) = str_at(seg, "dir", "plugins.dir")? {
                cfg.plugins.dir = expand_tilde(&x);
            }
            if let Some(x) = str_at(seg, "sandbox", "plugins.sandbox")? {
                cfg.plugins.sandbox = x;
            }
            if let Some(x) = str_at(seg, "plugin_user", "plugins.plugin_user")? {
                cfg.plugins.plugin_user = x;
            }
            if let Some(seg) = segment(seg, "limits")? {
                if let Some(x) = u64_at(seg, "memory_mb", "plugins.limits.memory_mb")? {
                    cfg.plugins.limits.memory_mb = x;
                }
                if let Some(x) = u64_at(seg, "cpu_sec", "plugins.limits.cpu_sec")? {
                    cfg.plugins.limits.cpu_sec = x;
                }
                if let Some(x) = u64_at(seg, "procs", "plugins.limits.procs")? {
                    cfg.plugins.limits.procs = x;
                }
                if let Some(x) = u64_at(seg, "file_mb", "plugins.limits.file_mb")? {
                    cfg.plugins.limits.file_mb = x;
                }
            }
        }

        if let Some(seg) = segment(root, "log")? {
            if let Some(x) = str_at(seg, "level", "log.level")? {
                cfg.log.level = x;
            }
            if let Some(x) = str_at(seg, "dir", "log.dir")? {
                cfg.log.dir = expand_tilde(&x);
            }
            if let Some(x) = u64_at(seg, "keep_days", "log.keep_days")? {
                cfg.log.keep_days = x;
            }
        }

        Ok(cfg)
    }

    /// 用环境变量覆盖当前配置（优先级：环境变量 > 配置文件/默认值）。
    ///
    /// `get` 是注入进来的 getter，测试可以传假环境，**不污染进程环境**；
    /// 生产里传 `&|k| std::env::var(k).ok()`。
    ///
    /// | 环境变量 | 字段 |
    /// |---|---|
    /// | `MR_JWT_SECRET` | `server.jwt_secret` |
    /// | `MR_LIBRARY_ROOTS` | `storage.library_roots`（`:` 或 `,` 分隔） |
    /// | `MR_DATABASE_PATH` | `database.path`（支持 `~`） |
    /// | `MR_FFMPEG_PATH` | `audio.ffmpeg_path` |
    /// | `MR_CACHE_DIR` | `audio.cache_dir`（支持 `~`） |
    /// | `MR_LOG_LEVEL` | `log.level` |
    /// | `MR_SANDBOX` | `plugins.sandbox` |
    /// | `MR_PLUGIN_USER` | `plugins.plugin_user` |
    /// | `MR_PLUGINS_DIR` | `plugins.dir`（支持 `~`） |
    ///
    /// 变量**未设置**（`None`）= 不动；设置成空白串 = `Empty` 错误（几乎必然是
    /// 部署脚本漏填，早失败好过静默用半截配置）。`~` 展开用的 `HOME` 也从这个 getter 取。
    pub fn apply_env(&mut self, get: &dyn Fn(&str) -> Option<String>) -> Result<(), ConfigError> {
        let home = get("HOME");

        if let Some(x) = env_string(get, "MR_JWT_SECRET", "server.jwt_secret")? {
            self.server.jwt_secret = x;
        }
        if let Some(x) = env_string(get, LIBRARY_ROOTS_ENV, "storage.library_roots")? {
            let roots = split_roots(&x);
            if roots.is_empty() {
                return Err(ConfigError::Empty {
                    field: "storage.library_roots",
                });
            }
            self.storage.library_roots = roots;
        }
        // MR_DATA_DIR：一次把数据库 / 转码缓存 / 日志都挪到同一个目录下。
        // ⚠️ 必须**排在**下面几个更具体的路径变量之前 —— 后处理的会覆盖它。
        if let Some(x) = env_string(get, "MR_DATA_DIR", "MR_DATA_DIR")? {
            let base = expand_tilde_with(&x, home.as_deref());
            self.database.path = format!("{base}/music.db");
            self.audio.cache_dir = format!("{base}/transcode");
            self.log.dir = format!("{base}/logs");
        }
        if let Some(x) = env_string(get, "MR_DATABASE_PATH", "database.path")? {
            self.database.path = expand_tilde_with(&x, home.as_deref());
        }
        if let Some(x) = env_string(get, "MR_FFMPEG_PATH", "audio.ffmpeg_path")? {
            self.audio.ffmpeg_path = x;
        }
        // 转码缓存目录此前只能靠配置文件改，这里补上环境变量，口径与 database.path 一致（~ 展开）。
        if let Some(x) = env_string(get, "MR_CACHE_DIR", "audio.cache_dir")? {
            self.audio.cache_dir = expand_tilde_with(&x, home.as_deref());
        }
        if let Some(x) = env_string(get, "MR_LOG_LEVEL", "log.level")? {
            self.log.level = x;
        }
        if let Some(x) = env_string(get, "MR_SANDBOX", "plugins.sandbox")? {
            self.plugins.sandbox = x;
        }
        if let Some(x) = env_string(get, "MR_PLUGIN_USER", "plugins.plugin_user")? {
            self.plugins.plugin_user = x;
        }
        // 插件目录：默认 "plugins" 是相对 cwd 的；用户也可以显式写 ~/plugins。
        if let Some(x) = env_string(get, "MR_PLUGINS_DIR", "plugins.dir")? {
            self.plugins.dir = expand_tilde_with(&x, home.as_deref());
        }

        Ok(())
    }

    /// 校验配置自洽。默认值必须能通过（有测试锁死）。
    ///
    /// 未知字段不算错（在 `from_value` 里就忽略了），这里只管取值是否可用。
    pub fn validate(&self) -> Result<(), ConfigError> {
        // 空数组也走这里：Iterator::all 对空集返回 true
        if self
            .storage
            .library_roots
            .iter()
            .all(|r| r.trim().is_empty())
        {
            return Err(ConfigError::Empty {
                field: "storage.library_roots",
            });
        }
        if self.server.port == 0 {
            return Err(ConfigError::BadValue {
                field: "server.port",
                value: self.server.port.to_string(),
                allowed: "1..=65535".to_string(),
            });
        }
        if self.server.token_expiry_hours == 0 {
            return Err(ConfigError::BadValue {
                field: "server.token_expiry_hours",
                value: self.server.token_expiry_hours.to_string(),
                allowed: ">= 1".to_string(),
            });
        }
        if self.scrape.concurrency == 0 {
            return Err(ConfigError::BadValue {
                field: "scrape.concurrency",
                value: self.scrape.concurrency.to_string(),
                allowed: ">= 1".to_string(),
            });
        }
        if self.scrape.batch_size == 0 {
            return Err(ConfigError::BadValue {
                field: "scrape.batch_size",
                value: self.scrape.batch_size.to_string(),
                allowed: ">= 1".to_string(),
            });
        }
        if !SANDBOX_MODES.contains(&self.plugins.sandbox.as_str()) {
            return Err(ConfigError::BadValue {
                field: "plugins.sandbox",
                value: self.plugins.sandbox.clone(),
                allowed: SANDBOX_MODES.join(" / "),
            });
        }
        // 日志级别写错要在**启动前**报出来 —— 否则服务照跑，但日志静默按默认级别过滤，
        // 用户以为开了 debug 却什么都看不到。
        if crate::serverlog::Level::parse(&self.log.level).is_none() {
            return Err(ConfigError::BadValue {
                field: "log.level",
                value: self.log.level.clone(),
                allowed: crate::serverlog::LEVELS.join(" / "),
            });
        }
        Ok(())
    }
}

impl Default for Config {
    /// 等价于 [`Config::defaults`]（`Default` 永远指向自洽的默认配置）。
    fn default() -> Self {
        Self::defaults()
    }
}

// ---------------------------------------------------------------- 解析辅助

/// 已确认是 JSON 对象的那一层。
type Obj = Map<String, Value>;

/// 顶层 / 嵌套段必须是对象。
fn expect_object<'a>(v: &'a Value, field: &'static str) -> Result<&'a Obj, ConfigError> {
    v.as_object().ok_or(ConfigError::BadType {
        field,
        expected: "JSON 对象",
    })
}

/// 取一个可选的子段（缺失或 `null` → `None`；存在但不是对象 → `BadType`）。
fn segment<'a>(o: &'a Obj, key: &'static str) -> Result<Option<&'a Obj>, ConfigError> {
    match non_null(o, key) {
        None => Ok(None),
        Some(v) => v.as_object().map(Some).ok_or(ConfigError::BadType {
            field: key,
            expected: "JSON 对象",
        }),
    }
}

/// `null` 与「键不存在」等价，都是「没写，回落默认值」。
fn non_null<'a>(o: &'a Obj, key: &str) -> Option<&'a Value> {
    match o.get(key) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v),
    }
}

fn u64_at(o: &Obj, key: &str, field: &'static str) -> Result<Option<u64>, ConfigError> {
    match non_null(o, key) {
        None => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or(ConfigError::BadType {
            field,
            expected: "非负整数",
        }),
    }
}

fn u16_at(o: &Obj, key: &str, field: &'static str) -> Result<Option<u16>, ConfigError> {
    match u64_at(o, key, field)? {
        None => Ok(None),
        Some(n) => u16::try_from(n).map(Some).map_err(|_| ConfigError::BadValue {
            field,
            value: n.to_string(),
            allowed: "0..=65535".to_string(),
        }),
    }
}

fn u32_at(o: &Obj, key: &str, field: &'static str) -> Result<Option<u32>, ConfigError> {
    match u64_at(o, key, field)? {
        None => Ok(None),
        Some(n) => u32::try_from(n).map(Some).map_err(|_| ConfigError::BadValue {
            field,
            value: n.to_string(),
            allowed: "0..=4294967295".to_string(),
        }),
    }
}

fn bool_at(o: &Obj, key: &str, field: &'static str) -> Result<Option<bool>, ConfigError> {
    match non_null(o, key) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or(ConfigError::BadType {
            field,
            expected: "布尔值",
        }),
    }
}

fn str_at(o: &Obj, key: &str, field: &'static str) -> Result<Option<String>, ConfigError> {
    match non_null(o, key) {
        None => Ok(None),
        Some(v) => v.as_str().map(|s| Some(s.to_string())).ok_or(ConfigError::BadType {
            field,
            expected: "字符串",
        }),
    }
}

fn str_vec_at(o: &Obj, key: &str, field: &'static str) -> Result<Option<Vec<String>>, ConfigError> {
    let v = match non_null(o, key) {
        None => return Ok(None),
        Some(v) => v,
    };
    let arr = v.as_array().ok_or(ConfigError::BadType {
        field,
        expected: "字符串数组",
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item.as_str().ok_or(ConfigError::BadType {
            field,
            expected: "字符串数组",
        })?;
        out.push(s.to_string());
    }
    Ok(Some(out))
}

fn u32_map_at(
    o: &Obj,
    key: &str,
    field: &'static str,
) -> Result<Option<HashMap<String, u32>>, ConfigError> {
    let v = match non_null(o, key) {
        None => return Ok(None),
        Some(v) => v,
    };
    let map = v.as_object().ok_or(ConfigError::BadType {
        field,
        expected: "「插件名 → 并发数」JSON 对象",
    })?;
    let mut out = HashMap::with_capacity(map.len());
    for (name, value) in map {
        let n = value.as_u64().ok_or(ConfigError::BadType {
            field,
            expected: "「插件名 → 非负整数」JSON 对象",
        })?;
        let n = u32::try_from(n).map_err(|_| ConfigError::BadValue {
            field,
            value: format!("{name}={n}"),
            allowed: "0..=4294967295".to_string(),
        })?;
        out.insert(name.clone(), n);
    }
    Ok(Some(out))
}

/// 取环境变量：`None` = 没设置，原样返回；空白串 = `Empty` 错误。
///
/// 注意：只在判空时 `trim`，真正返回的是**原值**（JWT 密钥可能合法地含首尾空格）。
fn env_string(
    get: &dyn Fn(&str) -> Option<String>,
    name: &str,
    field: &'static str,
) -> Result<Option<String>, ConfigError> {
    match get(name) {
        None => Ok(None),
        Some(v) if v.trim().is_empty() => Err(ConfigError::Empty { field }),
        Some(v) => Ok(Some(v)),
    }
}

/// `MR_LIBRARY_ROOTS` 按 `:` 或 `,` 拆分，丢掉空段与首尾空白。
///
/// ⚠️ **Windows 上不能按 `:` 拆** —— 盘符本身就是 `C:`，`C:\Music;D:\Songs` 会被切成
/// `["C", "\Music;D", "\Songs"]`，路径全废。所以 Windows 只认 `,`。
fn split_roots(raw: &str) -> Vec<String> {
    let seps: &[char] = if cfg!(windows) { &[','] } else { &[':', ','] };
    raw.split(seps)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// 用进程环境里的 `HOME` 展开 `~`。
fn expand_tilde(s: &str) -> String {
    expand_tilde_with(s, std::env::var("HOME").ok().as_deref())
}

/// `~` / `~/xxx` → 家目录 / `$HOME/xxx`。
///
/// `HOME` 取不到（`None`）或为空串时**原样返回**：不报错、不 panic、不猜。
/// `~user/xxx` 这种别的用户的写法不支持，也原样保留。
fn expand_tilde_with(s: &str, home: Option<&str>) -> String {
    let home = match home {
        Some(h) if !h.is_empty() => h,
        _ => return s.to_string(),
    };
    if s == "~" {
        return home.to_string();
    }
    match s.strip_prefix("~/") {
        Some(rest) => {
            let mut p = PathBuf::from(home);
            p.push(rest);
            p.to_string_lossy().into_owned()
        }
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个假环境 getter：只有 `pairs` 里列出的变量存在。
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, value)| (*value).to_string())
        }
    }

    fn parse(json: &str) -> Value {
        match serde_json::from_str(json) {
            Ok(v) => v,
            Err(e) => panic!("测试用的 JSON 不合法：{e}"),
        }
    }

    /// 写一个进程内唯一的临时配置文件。
    fn temp_file(prefix: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{prefix}{}.json", std::process::id()));
        std::fs::write(&path, content).expect("写临时配置文件");
        path
    }

    /// 进程 HOME（空串按「没有」处理，与 expand_tilde_with 的判据一致）。
    fn real_home() -> Option<String> {
        std::env::var("HOME").ok().filter(|h| !h.is_empty())
    }

    fn assert_bad_value(cfg: &Config, want_field: &str, want_value: &str) {
        match cfg.validate() {
            Err(ConfigError::BadValue {
                field,
                value,
                allowed,
            }) => {
                assert_eq!(field, want_field, "字段名");
                assert_eq!(value, want_value, "非法值");
                assert!(!allowed.is_empty(), "allowed 必须说明合法范围");
            }
            other => panic!("期望 {want_field} 报 BadValue，实际 {other:?}"),
        }
    }

    // ---- 1. 默认值 + 与实现常量对齐 -------------------------------------

    #[test]
    fn defaults_reuse_existing_constants() {
        use crate::plugin::manifest::DEFAULT_TIMEOUT_MS;
        use crate::plugin::pool::DEFAULT_IDLE_TIMEOUT;
        use crate::plugin::sandbox::{
            DEFAULT_CPU_SEC, DEFAULT_FILE_MB, DEFAULT_MEMORY_MB, DEFAULT_PROCS,
        };
        use crate::watcher::suppress::DEFAULT_TTL;

        let c = Config::defaults();
        assert_eq!(
            c.watcher.self_write_suppress_ms,
            DEFAULT_TTL.as_millis() as u64,
            "watcher.self_write_suppress_ms 必须来自 suppress::DEFAULT_TTL"
        );
        assert_eq!(
            c.scrape.task_timeout_sec,
            DEFAULT_TIMEOUT_MS / 1000,
            "scrape.task_timeout_sec 必须来自 manifest::DEFAULT_TIMEOUT_MS"
        );
        assert_eq!(
            c.scrape.pool_idle_timeout_sec,
            DEFAULT_IDLE_TIMEOUT.as_secs(),
            "scrape.pool_idle_timeout_sec 必须来自 pool::DEFAULT_IDLE_TIMEOUT"
        );
        assert_eq!(c.plugins.limits.memory_mb, DEFAULT_MEMORY_MB);
        assert_eq!(c.plugins.limits.cpu_sec, DEFAULT_CPU_SEC);
        assert_eq!(c.plugins.limits.procs, DEFAULT_PROCS);
        assert_eq!(c.plugins.limits.file_mb, DEFAULT_FILE_MB);
        // 默认 limits 必须原样映射成沙箱实现的默认配置（nofile 用实现里的常量，不写第二份）
        assert_eq!(
            c.plugins.sandbox_config(),
            crate::plugin::sandbox::SandboxConfig::default(),
            "默认 plugins.limits 应映射成 SandboxConfig::default()"
        );
    }

    #[test]
    fn defaults_have_expected_values() {
        let c = Config::defaults();

        assert_eq!(c.server.host, "0.0.0.0");
        assert_eq!(c.server.port, 8080);
        assert!(
            c.server.jwt_secret.is_empty(),
            "jwt_secret 只走环境变量，默认为空"
        );
        assert_eq!(c.server.token_expiry_hours, 24);

        assert_eq!(c.storage.kind, "local");
        assert_eq!(c.storage.library_roots, vec!["/music"]);

        // 数据库 / 缓存 / 日志必须同处一个数据根下 —— 这正是「一个 MR_DATA_DIR 就能搬家」的前提
        let want_data = match real_home() {
            Some(h) => format!("{h}/.local/share/music-robot"),
            None => DEFAULT_DATA_DIR.to_string(),
        };
        assert_eq!(c.database.path, format!("{want_data}/music.db"));

        assert!(c.audio.transcode);
        assert_eq!(c.audio.cache_dir, format!("{want_data}/transcode"));
        assert_eq!(c.audio.cache_expiry_days, 30);
        assert_eq!(c.audio.ffmpeg_path, "ffmpeg");

        assert!(c.watcher.enabled);
        assert_eq!(c.watcher.debounce_ms, 500);

        assert_eq!(c.scrape.concurrency, 3);
        assert_eq!(c.scrape.batch_size, 100);
        assert_eq!(c.scrape.max_retry, 2);
        assert_eq!(c.scrape.request_delay_ms, 500);
        assert_eq!(c.scrape.rate_limit_cooldown_sec, 60);
        assert_eq!(c.scrape.per_plugin_max.get("netease"), Some(&1));
        assert_eq!(c.scrape.per_plugin_max.get("qqmusic"), Some(&1));
        assert_eq!(c.scrape.per_plugin_max.get("musicbrainz"), Some(&2));
        assert_eq!(c.scrape.per_plugin_max.len(), 3);

        assert_eq!(c.plugins.dir, "plugins", "插件目录默认值必须是相对 cwd 的 plugins");
        assert_eq!(c.plugins.sandbox, "auto");
        assert_eq!(c.plugins.plugin_user, "music-plugin");

        assert_eq!(c.log.level, "info");
        assert_eq!(c.log.dir, format!("{want_data}/logs"));

        // 三条路径都落在同一个数据根下（改数据根 = 三者一起搬）
        for p in [&c.database.path, &c.audio.cache_dir, &c.log.dir] {
            assert!(p.starts_with(&want_data), "{p} 不在数据根 {want_data} 下");
        }

        // Default 与 defaults() 是同一个东西
        assert_eq!(Config::default(), c);
    }

    #[test]
    fn defaults_are_self_consistent() {
        Config::defaults()
            .validate()
            .expect("默认值必须能通过 validate");
        Config::default().validate().expect("Default 也必须自洽");
    }

    // ---- 2/3. JSON 解析 -------------------------------------------------

    #[test]
    fn full_json_overrides_every_field() {
        let json = parse(
            r#"{
              "server":   {"host":"127.0.0.1","port":9000,"jwt_secret":"file-secret","token_expiry_hours":48},
              "storage":  {"type":"local","library_roots":["/music/华语","/music/古典"]},
              "database": {"path":"/var/lib/mr/music.db"},
              "audio":    {"transcode":false,"cache_dir":"/var/cache/mr","cache_expiry_days":7,"ffmpeg_path":"/usr/bin/ffmpeg"},
              "watcher":  {"enabled":false,"debounce_ms":250,"self_write_suppress_ms":1000},
              "scrape":   {"concurrency":8,"batch_size":50,"task_timeout_sec":15,"max_retry":5,
                           "request_delay_ms":100,"rate_limit_cooldown_sec":120,"pool_idle_timeout_sec":600,
                           "per_plugin_max":{"netease":2,"qqmusic":3}},
              "plugins":  {"dir":"/opt/mr/plugins","sandbox":"bwrap","plugin_user":"plug",
                           "limits":{"memory_mb":1024,"cpu_sec":120,"procs":32,"file_mb":50}},
              "log":      {"level":"debug","dir":"/var/log/mr"}
            }"#,
        );
        let c = Config::from_value(&json).expect("完整配置应能解析");

        assert_eq!(c.server.host, "127.0.0.1");
        assert_eq!(c.server.port, 9000);
        assert_eq!(c.server.jwt_secret, "file-secret");
        assert_eq!(c.server.token_expiry_hours, 48);

        assert_eq!(c.storage.kind, "local");
        assert_eq!(c.storage.library_roots, vec!["/music/华语", "/music/古典"]);

        assert_eq!(c.database.path, "/var/lib/mr/music.db");

        assert!(!c.audio.transcode);
        assert_eq!(c.audio.cache_dir, "/var/cache/mr");
        assert_eq!(c.audio.cache_expiry_days, 7);
        assert_eq!(c.audio.ffmpeg_path, "/usr/bin/ffmpeg");

        assert!(!c.watcher.enabled);
        assert_eq!(c.watcher.debounce_ms, 250);
        assert_eq!(c.watcher.self_write_suppress_ms, 1000);

        assert_eq!(c.scrape.concurrency, 8);
        assert_eq!(c.scrape.batch_size, 50);
        assert_eq!(c.scrape.task_timeout_sec, 15);
        assert_eq!(c.scrape.max_retry, 5);
        assert_eq!(c.scrape.request_delay_ms, 100);
        assert_eq!(c.scrape.rate_limit_cooldown_sec, 120);
        assert_eq!(c.scrape.pool_idle_timeout_sec, 600);
        assert_eq!(c.scrape.per_plugin_max.get("netease"), Some(&2));
        assert_eq!(c.scrape.per_plugin_max.get("qqmusic"), Some(&3));

        assert_eq!(c.plugins.dir, "/opt/mr/plugins");
        assert_eq!(c.plugins.sandbox, "bwrap");
        assert_eq!(c.plugins.plugin_user, "plug");
        assert_eq!(c.plugins.limits.memory_mb, 1024);
        assert_eq!(c.plugins.limits.cpu_sec, 120);
        assert_eq!(c.plugins.limits.procs, 32);
        assert_eq!(c.plugins.limits.file_mb, 50);

        assert_eq!(c.log.level, "debug");
        assert_eq!(c.log.dir, "/var/log/mr");
    }

    #[test]
    fn partial_json_falls_back_to_defaults() {
        let empty = Config::from_value(&parse("{}")).expect("空对象 = 全默认");
        assert_eq!(empty, Config::defaults(), "空 JSON 必须等于内置默认值");

        let partial = parse(r#"{"server":{"port":1},"log":{"level":"warn"}}"#);
        let c = Config::from_value(&partial).expect("部分配置应能解析");
        let d = Config::defaults();

        assert_eq!(c.server.port, 1);
        assert_eq!(c.log.level, "warn");
        assert_eq!(c.server.host, d.server.host, "没写的字段回落默认");
        assert_eq!(c.storage, d.storage);
        assert_eq!(c.database, d.database);
        assert_eq!(c.audio, d.audio);
        assert_eq!(c.watcher, d.watcher);
        assert_eq!(c.scrape, d.scrape);
        assert_eq!(c.plugins, d.plugins);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let json = parse(
            r#"{
              "future_top_level": {"x": 1},
              "server":  {"port": 8081, "quantum_mode": true},
              "storage": {"library_roots": ["/m"], "extra": [1, 2, 3]},
              "plugins": {"limits": {"memory_mb": 64, "gpu_mb": 4096}}
            }"#,
        );
        let c = Config::from_value(&json).expect("未知字段不报错");
        assert_eq!(c.server.port, 8081);
        assert_eq!(c.plugins.limits.memory_mb, 64);
        assert_eq!(c.storage.library_roots, vec!["/m"]);
        assert_eq!(
            c.plugins.limits.cpu_sec,
            Config::defaults().plugins.limits.cpu_sec
        );
    }

    #[test]
    fn json_null_means_unset() {
        let c = Config::from_value(&parse(
            r#"{"server":{"port":null},"storage":{"library_roots":null},"scrape":{"per_plugin_max":null}}"#,
        ))
        .expect("null 视为没写");
        assert_eq!(c, Config::defaults());
    }

    // ---- 4/5. 环境变量 ---------------------------------------------------

    #[test]
    fn apply_env_overrides_config() {
        let mut c = Config::defaults();
        let e = env(&[
            ("MR_JWT_SECRET", "env-secret"),
            ("MR_LIBRARY_ROOTS", "/a:/b"),
            ("MR_DATABASE_PATH", "/env/db.sqlite"),
            ("MR_FFMPEG_PATH", "/opt/ffmpeg"),
            ("MR_CACHE_DIR", "/env/cache"),
            ("MR_LOG_LEVEL", "warn"),
            ("MR_SANDBOX", "systemd"),
            ("MR_PLUGIN_USER", "envplug"),
            ("MR_PLUGINS_DIR", "/env/plugins"),
        ]);
        c.apply_env(&e).expect("环境变量覆盖应成功");

        assert_eq!(c.server.jwt_secret, "env-secret");
        assert_eq!(c.storage.library_roots, vec!["/a", "/b"]);
        assert_eq!(c.database.path, "/env/db.sqlite");
        assert_eq!(c.audio.ffmpeg_path, "/opt/ffmpeg");
        assert_eq!(c.audio.cache_dir, "/env/cache", "MR_CACHE_DIR 覆盖 audio.cache_dir");
        assert_eq!(c.log.level, "warn");
        assert_eq!(c.plugins.sandbox, "systemd");
        assert_eq!(c.plugins.plugin_user, "envplug");
        assert_eq!(c.plugins.dir, "/env/plugins", "MR_PLUGINS_DIR 覆盖 plugins.dir");

        // 没设置的变量不动
        let d = Config::defaults();
        assert_eq!(c.server.port, d.server.port);
        assert_eq!(c.server.host, d.server.host);
        assert_eq!(c.scrape, d.scrape);
        assert_eq!(c.watcher, d.watcher);
    }

    #[test]
    fn library_roots_accept_colon_and_comma() {
        let mut c = Config::defaults();
        c.apply_env(&env(&[("MR_LIBRARY_ROOTS", "/a:/b,/c")]))
            .expect("混合分隔符");
        assert_eq!(c.storage.library_roots, vec!["/a", "/b", "/c"]);

        let mut c = Config::defaults();
        c.apply_env(&env(&[("MR_LIBRARY_ROOTS", "/only,  with space ,,")]))
            .expect("逗号 + 空白");
        assert_eq!(c.storage.library_roots, vec!["/only", "with space"]);

        let mut c = Config::defaults();
        let err = c
            .apply_env(&env(&[("MR_LIBRARY_ROOTS", ":,")]))
            .expect_err("全是空段应报错");
        assert!(
            matches!(
                err,
                ConfigError::Empty {
                    field: "storage.library_roots"
                }
            ),
            "实际 {err:?}"
        );
    }

    #[test]
    fn data_dir_moves_all_three_paths_together() {
        let mut c = Config::defaults();
        c.apply_env(&env(&[("HOME", "/home/u"), ("MR_DATA_DIR", "~/mr")]))
            .expect("MR_DATA_DIR 应成功");
        assert_eq!(c.database.path, "/home/u/mr/music.db");
        assert_eq!(c.audio.cache_dir, "/home/u/mr/transcode");
        assert_eq!(c.log.dir, "/home/u/mr/logs");
    }

    #[test]
    fn specific_path_vars_still_override_data_dir() {
        let mut c = Config::defaults();
        c.apply_env(&env(&[
            ("MR_DATA_DIR", "/base"),
            ("MR_CACHE_DIR", "/elsewhere"),
        ]))
        .expect("应成功");
        assert_eq!(c.database.path, "/base/music.db");
        assert_eq!(c.log.dir, "/base/logs");
        assert_eq!(
            c.audio.cache_dir, "/elsewhere",
            "更具体的 MR_CACHE_DIR 应压过 MR_DATA_DIR"
        );
    }

    #[test]
    fn apply_env_rejects_empty_value() {
        let mut c = Config::defaults();
        let err = c
            .apply_env(&env(&[("MR_LOG_LEVEL", "   ")]))
            .expect_err("空白值应报错");
        assert!(
            matches!(err, ConfigError::Empty { field: "log.level" }),
            "实际 {err:?}"
        );
        assert_eq!(
            c.log.level,
            Config::defaults().log.level,
            "报错不应半途改配置"
        );
    }

    // ---- 6. ~ 展开 -------------------------------------------------------

    #[test]
    fn tilde_expansion_and_missing_home() {
        assert_eq!(expand_tilde_with("~/a/b", Some("/home/u")), "/home/u/a/b");
        assert_eq!(expand_tilde_with("~", Some("/home/u")), "/home/u");
        assert_eq!(
            expand_tilde_with("~other/x", Some("/home/u")),
            "~other/x",
            "不支持 ~user"
        );
        assert_eq!(expand_tilde_with("/abs/x", Some("/home/u")), "/abs/x");
        assert_eq!(expand_tilde_with("~/a", None), "~/a", "HOME 缺失时保留原样");
        assert_eq!(
            expand_tilde_with("~/a", Some("")),
            "~/a",
            "HOME 为空串同样保留原样"
        );

        // apply_env 的 HOME 也走注入闭包 → 不碰进程环境
        let mut c = Config::defaults();
        c.apply_env(&env(&[
            ("HOME", "/home/tester"),
            ("MR_DATABASE_PATH", "~/db.sqlite"),
            ("MR_CACHE_DIR", "~/cache"),
            ("MR_PLUGINS_DIR", "~/plugins"),
        ]))
        .expect("带 HOME 的覆盖");
        assert_eq!(c.database.path, "/home/tester/db.sqlite");
        assert_eq!(c.audio.cache_dir, "/home/tester/cache");
        assert_eq!(c.plugins.dir, "/home/tester/plugins");

        let mut c = Config::defaults();
        c.apply_env(&env(&[
            ("MR_DATABASE_PATH", "~/db.sqlite"),
            ("MR_CACHE_DIR", "~/cache"),
            ("MR_PLUGINS_DIR", "~/plugins"),
        ]))
        .expect("HOME 缺失也要成功");
        assert_eq!(c.database.path, "~/db.sqlite", "HOME 缺失时保留原样");
        assert_eq!(c.audio.cache_dir, "~/cache", "HOME 缺失时保留原样");
        assert_eq!(c.plugins.dir, "~/plugins", "HOME 缺失时保留原样");

        // from_value（文件层）用进程 HOME：只断言与 HOME 一致，不硬编码路径
        let json = parse(
            r#"{"database":{"path":"~/x.db"},"audio":{"cache_dir":"~/c"},"plugins":{"dir":"~/p"},"log":{"dir":"~/l"}}"#,
        );
        let c = Config::from_value(&json).expect("解析 ~ 路径");
        match real_home() {
            Some(h) => {
                assert_eq!(c.database.path, format!("{h}/x.db"));
                assert_eq!(c.audio.cache_dir, format!("{h}/c"));
                assert_eq!(c.plugins.dir, format!("{h}/p"));
                assert_eq!(c.log.dir, format!("{h}/l"));
            }
            None => {
                assert_eq!(c.database.path, "~/x.db");
                assert_eq!(c.audio.cache_dir, "~/c");
                assert_eq!(c.plugins.dir, "~/p");
                assert_eq!(c.log.dir, "~/l");
            }
        }
    }

    // ---- 7/8/9 + load 成功路径 ------------------------------------------

    #[test]
    fn load_missing_file_returns_defaults() {
        let path =
            std::env::temp_dir().join(format!("mr-config-missing-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let c = Config::load(&path).expect("文件不存在不是错误");
        assert_eq!(c, Config::defaults());
    }

    #[test]
    fn load_reads_real_file() {
        let path = temp_file(
            "mr-config-ok-",
            r#"{"server":{"port":1234},"storage":{"library_roots":["/lib"]}}"#,
        );
        let c = Config::load(&path).expect("合法配置文件");
        std::fs::remove_file(&path).ok();

        assert_eq!(c.server.port, 1234);
        assert_eq!(c.storage.library_roots, vec!["/lib"]);
        assert_eq!(c.server.host, "0.0.0.0", "没写的段落回落默认值");
    }

    #[test]
    fn load_invalid_json_reports_bad_json() {
        let path = temp_file("mr-config-bad-", "{ 这不是 JSON ");
        let err = Config::load(&path).expect_err("非法 JSON 必须报错");
        std::fs::remove_file(&path).ok();

        match err {
            ConfigError::BadJson { path: p, source } => {
                assert_eq!(p.as_deref(), Some(path.as_path()), "BadJson 要带上文件路径");
                assert!(source.line() >= 1, "要带上 serde_json 的行号信息");
            }
            other => panic!("期望 BadJson，实际 {other:?}"),
        }
    }

    #[test]
    fn load_unreadable_path_reports_io() {
        // 目录不是配置文件 → IO 错误（而不是「不存在」→ 默认值）
        let dir = std::env::temp_dir();
        match Config::load(&dir) {
            Err(ConfigError::Io { path, .. }) => assert_eq!(path, dir),
            other => panic!("期望 Io 错误，实际 {other:?}"),
        }
    }

    #[test]
    fn wrong_types_report_field_name() {
        let cases: [(&str, &str); 6] = [
            (r#"{"server":{"port":"8080"}}"#, "server.port"),
            (
                r#"{"server":{"token_expiry_hours":true}}"#,
                "server.token_expiry_hours",
            ),
            (
                r#"{"storage":{"library_roots":"/music"}}"#,
                "storage.library_roots",
            ),
            (r#"{"storage":{"library_roots":["/a",7]}}"#, "storage.library_roots"),
            (r#"{"watcher":{"enabled":"yes"}}"#, "watcher.enabled"),
            (
                r#"{"scrape":{"per_plugin_max":{"netease":"1"}}}"#,
                "scrape.per_plugin_max",
            ),
        ];
        for (json, field) in cases {
            let err = Config::from_value(&parse(json)).expect_err(json);
            assert!(
                matches!(err, ConfigError::BadType { field: f, .. } if f == field),
                "{json} 期望 BadType({field})，实际 {err:?}"
            );
        }

        let err = Config::from_value(&parse("[]")).expect_err("数组不是合法配置根");
        assert!(
            matches!(err, ConfigError::BadType { field: "config", .. }),
            "实际 {err:?}"
        );

        let err = Config::from_value(&parse(r#"{"server":1}"#)).expect_err("server 必须是对象");
        assert!(
            matches!(err, ConfigError::BadType { field: "server", .. }),
            "实际 {err:?}"
        );

        let err = Config::from_value(&parse(r#"{"plugins":{"limits":[]}}"#))
            .expect_err("limits 必须是对象");
        assert!(
            matches!(err, ConfigError::BadType { field: "limits", .. }),
            "实际 {err:?}"
        );
    }

    #[test]
    fn out_of_range_numbers_are_bad_value() {
        let err =
            Config::from_value(&parse(r#"{"server":{"port":70000}}"#)).expect_err("端口超出 u16");
        match err {
            ConfigError::BadValue {
                field,
                value,
                allowed,
            } => {
                assert_eq!(field, "server.port");
                assert_eq!(value, "70000");
                assert!(!allowed.is_empty());
            }
            other => panic!("期望 BadValue，实际 {other:?}"),
        }

        let err = Config::from_value(&parse(r#"{"scrape":{"concurrency":4294967296}}"#))
            .expect_err("并发数超出 u32");
        assert!(
            matches!(
                err,
                ConfigError::BadValue {
                    field: "scrape.concurrency",
                    ..
                }
            ),
            "实际 {err:?}"
        );
    }

    #[test]
    fn per_plugin_max_parses_map() {
        let c = Config::from_value(&parse(
            r#"{"scrape":{"per_plugin_max":{"netease":4,"musicbrainz":2}}}"#,
        ))
        .expect("解析 per_plugin_max");
        assert_eq!(c.scrape.per_plugin_max.get("netease"), Some(&4));
        assert_eq!(c.scrape.per_plugin_max.get("musicbrainz"), Some(&2));
        assert_eq!(c.scrape.per_plugin_max.len(), 2);
    }

    // ---- 10/11/12/13. 校验、未知字段、错误文案 --------------------------

    #[test]
    fn validate_rejects_each_bad_field() {
        let base = Config::defaults();

        let mut c = base.clone();
        c.storage.library_roots = Vec::new();
        assert!(
            matches!(
                c.validate(),
                Err(ConfigError::Empty {
                    field: "storage.library_roots"
                })
            ),
            "空曲库根必须报 Empty"
        );

        let mut c = base.clone();
        c.storage.library_roots = vec!["   ".to_string()];
        assert!(
            matches!(
                c.validate(),
                Err(ConfigError::Empty {
                    field: "storage.library_roots"
                })
            ),
            "全空白曲库根也算空"
        );

        let mut c = base.clone();
        c.server.port = 0;
        assert_bad_value(&c, "server.port", "0");

        let mut c = base.clone();
        c.server.token_expiry_hours = 0;
        assert_bad_value(&c, "server.token_expiry_hours", "0");

        let mut c = base.clone();
        c.scrape.concurrency = 0;
        assert_bad_value(&c, "scrape.concurrency", "0");

        let mut c = base.clone();
        c.scrape.batch_size = 0;
        assert_bad_value(&c, "scrape.batch_size", "0");

        for mode in SANDBOX_MODES {
            let mut c = base.clone();
            c.plugins.sandbox = mode.to_string();
            c.validate()
                .unwrap_or_else(|e| panic!("{mode} 应被接受：{e}"));
        }
        let mut c = base.clone();
        c.plugins.sandbox = "chroot".to_string();
        assert_bad_value(&c, "plugins.sandbox", "chroot");
    }

    #[test]
    fn error_display_is_chinese_with_field() {
        let bad_json = match serde_json::from_str::<Value>("{") {
            Ok(_) => panic!("这个测试依赖非法 JSON"),
            Err(e) => e,
        };
        let cases: [(ConfigError, &str); 5] = [
            (
                ConfigError::Io {
                    path: PathBuf::from("/x/config.json"),
                    source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
                },
                "/x/config.json",
            ),
            (
                ConfigError::BadJson {
                    path: Some(PathBuf::from("/x/config.json")),
                    source: bad_json,
                },
                "/x/config.json",
            ),
            (
                ConfigError::BadType {
                    field: "server.port",
                    expected: "非负整数",
                },
                "server.port",
            ),
            (
                ConfigError::BadValue {
                    field: "plugins.sandbox",
                    value: "chroot".to_string(),
                    allowed: SANDBOX_MODES.join(" / "),
                },
                "plugins.sandbox",
            ),
            (
                ConfigError::Empty {
                    field: "storage.library_roots",
                },
                "storage.library_roots",
            ),
        ];

        for (err, field) in cases {
            let msg = err.to_string();
            assert!(msg.contains(field), "「{msg}」应含字段名 {field}");
            assert!(
                msg.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "「{msg}」应是中文"
            );
        }

        let err = ConfigError::BadValue {
            field: "plugins.sandbox",
            value: "chroot".to_string(),
            allowed: "auto / none".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("chroot"), "BadValue 文案要带上非法值：{msg}");
        assert!(msg.contains("auto"), "BadValue 文案要带上合法取值：{msg}");

        let io = ConfigError::Io {
            path: PathBuf::from("/x"),
            source: std::io::Error::new(std::io::ErrorKind::Other, "boom"),
        };
        assert!(
            std::error::Error::source(&io).is_some(),
            "Io 要透出底层错误"
        );
        let empty = ConfigError::Empty { field: "log.dir" };
        assert!(std::error::Error::source(&empty).is_none());
    }
}
