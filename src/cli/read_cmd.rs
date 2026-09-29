//! read 子命令 —— 单文件标签查看（移植自 src/cli/read.ts）
use std::collections::HashMap;
use crate::cli::args::{parse_args, wants_help, Flag, UsageError};

type Flags = HashMap<String, Flag>;

fn flag_single(flags: &Flags, key: &str) -> Result<Option<String>, UsageError> {
    let Some(f) = flags.get(key) else { return Ok(None) };
    Ok(f.single(key)?)
}
fn flag_bool(flags: &Flags, key: &str) -> bool {
    flags.get(key).map(|f| f.as_bool()).unwrap_or(false)
}
use crate::cli::io::CommandIO;
use crate::tag::read::{read_json, read_json_value, read_tags, warnings_for, AudioMetadata, Warning};

pub const READ_USAGE: &str = r#"用法: music-robot read <file> [--json] [--extract-cover <out>] [--verbose]

查看单个音乐文件的标签（读取层 src/tag/read 同一通道）。

选项:
  --json              输出机器可读 JSON（含 warnings）
  --extract-cover <f> 把第一张封面导出为文件
  --verbose           输出调试信息
  -h, --help          显示本帮助"#;

fn truncate(s: &str, n: usize) -> String {
    let cs: Vec<char> = s.chars().collect();
    if cs.len() <= n { s.to_string() } else { cs[..n].iter().collect::<String>() + "…" }
}

/// 人读表格（TS formatRead，逐字段照搬）
pub fn format_read(meta: &AudioMetadata, file: &str, warnings: &[Warning]) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("文件: {file}"));
    let dur = if meta.duration_ms > 0 { format!("{:.1}s", meta.duration_ms as f64 / 1000.0) } else { "?".into() };
    let bps = if meta.sample_rate > 0 { format!("{}Hz", meta.sample_rate) } else { "?".into() };
    let depth = meta.bits_per_sample.map(|b| format!("{b}bit")).unwrap_or_else(|| "?".into());
    let br = meta.bitrate_bps.map(|b| format!("{}kbps", (b as f64 / 1000.0).round() as i64)).unwrap_or_else(|| "?".into());
    lines.push(format!("source: {}    时长: {dur}    采样率: {bps}    位深: {depth}    码率: {br}", meta.source));
    if let Some(t) = &meta.title { lines.push(format!("标题: {}", truncate(t, 120))) }
    if !meta.artists.is_empty() { lines.push(format!("歌手: {}", meta.artists.iter().map(|a| truncate(a, 60)).collect::<Vec<_>>().join(" / "))) }
    if !meta.albums.is_empty() { lines.push(format!("专辑: {}", meta.albums.join(" / "))) }
    if let Some(a) = &meta.album_artist { lines.push(format!("专辑艺术家: {a}")) }
    let track = match meta.track {
        Some(t) => {
            let mut s = t.to_string();
            if let Some(tt) = meta.track_total { s.push_str("/"); s.push_str(&tt.to_string()) }
            s
        }
        None => "无".into(),
    };
    lines.push(format!("轨道: {track}"));
    if let Some(d) = meta.disc {
        let mut s = d.to_string();
        if let Some(dt) = meta.disc_total { s.push_str("/"); s.push_str(&dt.to_string()) }
        lines.push(format!("碟号: {s}"));
    }
    if let Some(y) = &meta.year { lines.push(format!("年份: {y}")) }
    if !meta.genres.is_empty() { lines.push(format!("流派: {}", meta.genres.join(" / "))) }
    if !meta.composers.is_empty() { lines.push(format!("作曲: {}", meta.composers.join(" / "))) }
    if let Some(c) = &meta.comment { lines.push(format!("注释: {}", truncate(c, 200))) }
    if let Some(l) = meta.lyrics_timed.clone().or_else(|| meta.lyrics.clone()) {
        let nl = l.lines().count();
        lines.push(format!("歌词: {nl} 行，{}", truncate(&l.replace('\n', " ⏎ "), 240)));
    }
    if let Some(p) = meta.pictures.first() {
        lines.push(format!("封面: {} {}B", p.mime_type, p.data.len()));
    } else {
        lines.push("封面: 无".into());
    }
    lines.push(format!("帧: {}（{}）", meta.raw_frames.len(), meta.raw_frames.iter().map(|f| f.frame_id.as_str()).collect::<Vec<_>>().join(", ")));
    if !warnings.is_empty() {
        lines.push(String::new());
        lines.push("警告:".into());
        for w in warnings { lines.push(format!("  ⚠ {}: {}", w.code, w.message)) }
    }
    lines.join("\n") + "\n"
}

pub fn run_read(argv: &[&str], io: &dyn CommandIO) -> Result<i32, UsageError> {
    if wants_help(argv) { io.log(READ_USAGE); return Ok(0) }
    let parsed = parse_args(argv)?;
    parsed.reject_unknown(&["json", "extract-cover", "verbose"], "read 支持: --json --extract-cover --verbose")?;
    parsed.require(1, READ_USAGE)?;
    let file = parsed.positionals[0].clone();

    let meta = match read_tags(std::path::Path::new(&file)) {
        Ok(m) => m,
        Err(e) => { io.error(&format!("读取失败: {e}")); return Ok(1) }
    };
    let buf = std::fs::read(&file).unwrap_or_default();
    let warnings = warnings_for(&meta, &buf);

    let extract = flag_single(&parsed.flags, "extract-cover")?;
    match extract {
        Some(out) => {
            let Some(pic) = meta.pictures.first() else { io.error("该文件没有封面"); return Ok(1) };
            if std::fs::write(&out, &pic.data).is_err() { io.error(&format!("封面导出失败: {out}")); return Ok(1) }
            io.log(&format!("封面已导出: {out}（{} {}B）", pic.mime_type, pic.data.len()));
        }
        None => {
            let json_out = flag_bool(&parsed.flags, "json");
            if json_out {
                io.log(&read_json_value(&meta, &file, &warnings).to_string());
            } else {
                io.log(&format_read(&meta, &file, &warnings));
                if flag_bool(&parsed.flags, "verbose") {
                    io.error(&format!("[verbose] {}", read_json(&meta, &file, &warnings)));
                }
            }
        }
    }
    Ok(0)
}
