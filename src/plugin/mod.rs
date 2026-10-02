//! 插件子系统 —— S9 清单解析 + S10 协议类型 + S11 worker 池 + S12 沙箱
//! + 插件目录扫描（[`registry`]：把前三者接成有序的刮削插件表）。
//!
//! 插件是单文件可执行脚本（.js / .py / .sh / 无扩展名），元数据写在文件头注释里；
//! 主服务与插件进程之间用 stdin/stdout 传 JSON（daemon 模式一行一个）。
//! 插件是**不受信任的子进程**，所以起进程这件事统一交给 [`pool::WorkerPool`]：
//! 它在 spawn 时经 [`sandbox::apply`] 挂上 rlimit / no_new_privs / pdeathsig / 降权。
//!
//! 依赖只有 std + serde_json + libc（手写 Value，不引 serde derive；
//! libc 只用来做 setrlimit/prctl/setsid/killpg 这些平台调用）。
//! 全部同步实现，async 调用方的接法见 [`pool`] 的模块文档。

pub mod error;
pub mod manifest;
pub mod pool;
pub mod protocol;
pub mod registry;
pub mod sandbox;

pub use error::{ManifestError, ProtocolError};
pub use manifest::{
    parse_manifest, parse_manifest_str, PluginKind, PluginMeta, DEFAULT_MAX_CONCURRENCY,
    DEFAULT_TIMEOUT_MS, END_MARKER, MARKER, MAX_HEAD_BYTES, MAX_HEAD_LINES,
};
pub use pool::{PoolConfig, PoolError, PoolGuard, PoolStats, WorkerPool, DEFAULT_IDLE_TIMEOUT};
pub use protocol::{
    decode_request, decode_response, encode_request, encode_response, validate_cover_path, CoverRef,
    DownloadMvRequest, DownloadOk, DownloadPrefer, DownloadRequest, ErrorCode, FieldUpdate, MvPrefer,
    PluginErrorInfo, PluginErrorResponse, PluginRequest, PluginResponse, RequestOptions,
    ResponseAction, ScrapeCandidate, ScrapeOk, ScrapeRequest, SongRef, TagValue, Tags, TrackMatch, DEFAULT_CONFIDENCE,
    PROTOCOL_VERSION,
};
pub use registry::{LoadReport, PluginRegistry};
pub use sandbox::{
    can_drop_privileges, kill_process_group, SandboxConfig, SandboxError, DEFAULT_CPU_SEC,
    DEFAULT_FILE_MB, DEFAULT_MEMORY_MB, DEFAULT_NOFILE, DEFAULT_PROCS,
};
