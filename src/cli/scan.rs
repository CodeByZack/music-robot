//! scan 子命令 —— 批量扫描分级（移植自 src/cli/scan.ts）
use std::collections::BTreeMap;

use crate::cli::args::{parse_args, wants_help, UsageError};
use crate::cli::io::CommandIO;
use crate::scanner::{rel_path, scan_dir, ScanEntry};

pub const SCAN_USAGE: &str = r#"用法: music-tag scan <dir> [--json]

递归扫描目录下的 .mp3/.flac/.wav，逐文件只读体检并分级：
  ok        无警告、格式识别正常
  warn      有警告（广告词 / GBK 乱码 / ID3v1 垃圾 / 非白名单帧）
  rejected  格式明确拒绝（ID3 前缀的 FLAC/WAV、未知容器伪装等）——须先转码
  broken    解析异常 / 文件损坏——人工处置

选项:
  --json   机器可读输出（含全部分级与警告）
  -h       帮助"#;

const LEVEL_PAD: usize = 9;

fn pad(s: &str, n: usize) -> String {
    if s.chars().count() >= n { s.to_string() } else { format!("{:width$}", " ", width = n) + s }
}

fn entry_line(e: &ScanEntry, dir: &str) -> String {
    let mut bits: Vec<String> = vec![rel_path(std::path::Path::new(&e.path), std::path::Path::new(dir))];
    if let Some(t) = &e.title { bits.push(t.clone()) }
    if let Some(a) = &e.artists_summary { bits.push(a.clone()) }
    let mut line = format!("[{}] {}", pad(e.level.as_str(), LEVEL_PAD), bits.join(" — "));
    if let Some(err) = &e.error { line.push_str(&format!(" — {err}")) }
    if !e.warnings.is_empty() {
        let mut by: BTreeMap<&str, usize> = BTreeMap::new();
        for w in &e.warnings { *by.entry(w.code.as_str()).or_insert(0) += 1 }
        let tags: Vec<String> = by.iter().map(|(c, n)| if *n > 1 { format!("{c}x{n}") } else { c.to_string() }).collect();
        line.push_str(&format!("  ⚠ {}", tags.join(" ")));
    }
    line
}

fn summary(report: &crate::scanner::ScanReport) -> String {
    let c = report.counts;
    format!("\n汇总: ok {} / warn {} / rejected {} / broken {}（共 {}）", c.ok, c.warn, c.rejected, c.broken, c.total)
}

pub fn run_scan(argv: &[&str], io: &dyn CommandIO) -> Result<i32, UsageError> {
    if wants_help(argv) { io.log(SCAN_USAGE); return Ok(0) }
    let parsed = parse_args(argv)?;
    parsed.reject_unknown(&["json"], "scan 支持: --json")?;
    parsed.require(1, SCAN_USAGE)?;
    let dir = std::path::Path::new(&parsed.positionals[0]);

    if !dir.is_dir() { io.error(&format!("不是目录: {}", dir.display())); return Ok(1) }
    let report = match scan_dir(dir) {
        Ok(r) => r,
        Err(e) => { io.error(&format!("扫描失败: {e}")); return Ok(1) }
    };
    if report.entries.is_empty() {
        io.error(&format!("目录中没有 .mp3/.flac/.wav 文件: {}", dir.display()));
        return Ok(1);
    }

    let json = parsed.flags.get("json").map(|f| f.as_bool()).unwrap_or(false);
    if json {
        let entries: Vec<serde_json::Value> = report.entries.iter().map(|e| {
            serde_json::json!({
                "path": rel_path(std::path::Path::new(&e.path), dir),
                "level": e.level.as_str(), "format": e.format,
                "title": e.title, "artists": e.artists_summary,
                "warnings": e.warnings.iter().map(|w| serde_json::json!({ "code": w.code, "message": w.message })).collect::<Vec<_>>(),
                "error": e.error, "frameCount": e.frame_count,
                "view": e.view,
            })
        }).collect();
        io.log(&serde_json::json!({
            "dir": report.dir,
            "counts": { "ok": report.counts.ok, "warn": report.counts.warn,
                        "rejected": report.counts.rejected, "broken": report.counts.broken,
                        "total": report.counts.total },
            "entries": entries,
        }).to_string());
        return Ok(0);
    }
    for e in &report.entries { io.log(&entry_line(e, &report.dir)) }
    io.log(&summary(&report));
    Ok(0)
}
