//! doctor 子命令 —— 环境 + 配置 + 全库体检（移植自 src/cli/doctor.ts）
use std::sync::OnceLock;
use crate::cli::args::{parse_args, wants_help, UsageError};
use crate::cli::io::CommandIO;
use crate::scanner::{scan_dir, ScanReport};

pub const DOCTOR_USAGE: &str = r#"用法: music-robot doctor [dir] [--json]

库健康度体检（只读，不改任何文件）：
  1. 环境自检：ffprobe 可用性（缺失时自动降级内置估算）
  2. 配置自检：配置模块（内置默认值 + MR_* 环境变量）
  3. 全库逐文件体检（与 scan 同一通道）：ok / warn / rejected / broken 分级汇总

退出码: 0 = 全部 ok/warn（warn 可修）；1 = 有 rejected/broken 或环境异常；2 = 用法错误
选项:
  --json   机器可读输出"#;

/// ffprobe 是否可用（PATH 里找，结果缓存）
pub fn ffprobe_available() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        std::env::var_os("PATH").map_or(false, |paths| {
            for d in std::env::split_paths(&paths) {
                if d.join("ffprobe").is_file() { return true }
            }
            false
        })
    })
}

pub fn run_doctor(argv: &[&str], io: &dyn CommandIO) -> Result<i32, UsageError> {
    if wants_help(argv) { io.log(DOCTOR_USAGE); return Ok(0) }
    let parsed = parse_args(argv)?;
    parsed.reject_unknown(&["json"], "doctor 支持: --json")?;
    if parsed.positionals.len() > 1 { return Err(UsageError::new(DOCTOR_USAGE.to_string())) }
    let dir = parsed.positionals.first().map(|s| s.as_str()).unwrap_or(".");
    let json = parsed.flags.get("json").map(|f| f.as_bool()).unwrap_or(false);

    let (probe_ok, probe_detail) = if ffprobe_available() {
        (true, "可用")
    } else {
        (false, "缺失 → 内置估算（MR_NO_FFPROBE=1 可强制本地算法）")
    };

    // 配置自检：config 模块（src/config.rs）已就绪，但 doctor 还没接线去读配置文件，
    // 所以这里只报告「用的是内置默认值 + MR_* 环境变量」，而不是谎报「配置已校验通过」。
    let (config_ok, config_detail) =
        (true, "未读取配置文件：使用内置默认值 + MR_* 环境变量（src/config.rs 已就绪，doctor 尚未接线）");

    // 单次扫描，结果同时供摘要字符串和 JSON 输出使用
    struct ScanOutcome { ok: bool, detail: String, report: Option<ScanReport> }
    let so = match std::path::Path::new(dir).is_dir() {
        false => ScanOutcome { ok: false, detail: format!("目录不可达: {dir}"), report: None },
        true => match scan_dir(std::path::Path::new(dir)) {
            Err(e) => ScanOutcome { ok: false, detail: e.to_string(), report: None },
            Ok(r) => {
                let empty = r.entries.is_empty();
                let ok = !empty && r.counts.rejected == 0 && r.counts.broken == 0;
                let detail = if empty {
                    format!("目录中没有 .mp3/.flac/.wav 文件: {dir}")
                } else if ok {
                    format!("{} 个文件：ok {} / warn {} / rejected 0 / broken 0",
                        r.counts.total, r.counts.ok, r.counts.warn)
                } else {
                    format!("{} 个文件：ok {} / warn {} / rejected {} / broken {}",
                        r.counts.total, r.counts.ok, r.counts.warn, r.counts.rejected, r.counts.broken)
                };
                ScanOutcome { ok, detail, report: Some(r) }
            },
        },
    };
    let (scan_ok, scan_detail) = (so.ok, so.detail.clone());

    let has_problem = !probe_ok || !scan_ok || !config_ok;
    let code = if has_problem { 1 } else { 0 };

    if json {
        let counts = so.report.as_ref().map(|r| serde_json::json!({
            "ok": r.counts.ok, "warn": r.counts.warn,
            "rejected": r.counts.rejected, "broken": r.counts.broken,
            "total": r.counts.total
        }));
        io.log(&serde_json::json!({
            "probe": { "ok": probe_ok, "detail": probe_detail },
            "config": { "ok": config_ok, "detail": config_detail },
            "scan": { "ok": scan_ok, "detail": scan_detail, "counts": counts },
            "exitCode": code,
        }).to_string());
        return Ok(code);
    }

    io.log(&format!("ffprobe: {} {}", probe_detail, if probe_ok { "✓" } else { "✗" }));
    io.log(&format!("配置: {config_detail} {}", if config_ok { "✓" } else { "✗" }));
    io.log(&format!("全库: {scan_detail} {}", if scan_ok { "✓" } else { "✗" }));
    if has_problem { io.error("体检发现问题（退出码 1）"); }
    else { io.log("全部健康 ✓"); }
    Ok(code)
}
