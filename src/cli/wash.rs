//! wash 子命令 —— 批量清洗（两段式写安全，移植自 src/cli/wash.ts）
use std::path::Path;

use crate::cli::args::{parse_args, wants_help, UsageError};
use crate::cli::io::CommandIO;
use crate::logger::ndjson_sink;
use crate::scanner::{scan_dir, rel_path, Level};
use crate::tag::read::read_tags;
use crate::tag::write::intent::{is_unset_key, merge_fields, WriteMeta, WritableFields};
use crate::tag::write::write_tags;

pub const WASH_USAGE: &str = r#"用法: music-tag wash <dir> [--blank | --unset <f1,f2>] [--apply] [--bak] [--ffprobe-check] [--events <file>] [--json]

批量清洗（两段式写安全，ARCHITECTURE §11.4）：
  默认 --preview：逐文件输出「将执行的剧本」状态行 + 汇总，不落盘
  --apply        真实写：原子写 → 读回复核；退出码 0=全部 applied、1=有 failed

规则（二选一，互斥）:
  --blank        全清剧本：重建成标准空标签，音频字节不动
  --unset <f..>  字段级移除（逗号分隔，如 --unset title,artist）

文件分流: 可处理 → applied/failed（preview 时为 preview）；rejected/broken → 跳过并给原因
选项:
  --bak              apply 写前备份 <file>.bak（重跑不覆盖已有首备份）
  --ffprobe-check    apply 后用 ffprobe 逐文件独立复核（15s 超时；缺失时跳过并说明）
  --events <f>       事件流追加写入 NDJSON 文件
  --json             机器可读输出
  -h                 帮助"#;

#[derive(Debug, Clone, PartialEq)]
pub enum WashRule { Blank, Unset(Vec<String>) }

/// 规则解析：--blank / --unset 二选一（互斥），unset 键走白名单
pub fn wash_rule_of(flags: &std::collections::HashMap<String, crate::cli::args::Flag>) -> Result<Option<WashRule>, UsageError> {
    let bl = flags.get("blank").map(|f| f.as_bool()).unwrap_or(false);
    let unset = flags.get("unset").and_then(|f| f.values.first()).cloned();
    if bl && unset.is_some() { return Err(UsageError::new("--blank 与 --unset 互斥（一次一个规则）")) }
    if bl { return Ok(Some(WashRule::Blank)) }
    if let Some(u) = unset {
        let keys: Vec<String> = u.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if keys.is_empty() { return Err(UsageError::new("--unset 需要至少一个字段名（如 --unset title,artist）")) }
        let bad: Vec<String> = keys.iter().filter(|k| !is_unset_key(k)).cloned().collect();
        if !bad.is_empty() { return Err(UsageError::new(format!("未知字段: {}（见 --help）", bad.join(",")))) }
        Ok(Some(WashRule::Unset(keys)))
    } else { Ok(None) }
}

pub struct ApplyOutcome { pub state: &'static str, pub detail: String }

/// blank 后读回的残留字段清单（全格式通用；空 = 复核干净）
pub fn blank_leftovers(m: &crate::tag::read::AudioMetadata) -> Vec<&'static str> {
    let mut out = Vec::new();
    if m.title.is_some() { out.push("title") }
    if !m.artists.is_empty() { out.push("artists") }
    if !m.albums.is_empty() { out.push("albums") }
    if m.album_artist.is_some() { out.push("albumArtist") }
    if m.track.is_some() || m.track_total.is_some() || m.disc.is_some() || m.disc_total.is_some() { out.push("track/disc") }
    if m.year.is_some() { out.push("year") }
    if !m.genres.is_empty() || !m.composers.is_empty() { out.push("genres/composers") }
    if m.lyrics.is_some() || m.lyrics_timed.is_some() { out.push("lyrics") }
    if m.comment.is_some() { out.push("comment") }
    if !m.pictures.is_empty() { out.push("pictures") }
    if !m.raw_frames.is_empty() { out.push("rawFrames") }
    out
}

fn build_intent(rule: &WashRule) -> WriteMeta {
    match rule {
        WashRule::Blank => WriteMeta { blank_all: true, ..Default::default() },
        WashRule::Unset(keys) => merge_fields(WritableFields { unset: keys.clone(), ..Default::default() }),
    }
}

/// 单文件 apply（错误隔离单元）：--bak → 原子写 → 读回复核 → 可选 ffprobe 复核。
/// 任何异常 → failed + 原因，绝不抛出。
pub fn apply_one(path: &Path, rule: &WashRule, bak: bool, ffprobe_check: bool) -> ApplyOutcome {
    let intent = build_intent(rule);
    let mut bak_note = String::new();
    if bak {
        let bk = std::path::PathBuf::from(format!("{}.bak", path.display()));
        if bk.exists() {
            bak_note = "；.bak 已存在（保留首备份）".into();
        } else if std::fs::copy(path, &bk).is_ok() {
            bak_note = "；已备份 .bak".into();
        } else {
            return ApplyOutcome { state: "failed", detail: format!("备份失败: {}", bk.display()) };
        }
    }
    if let Err(e) = write_tags(path, &intent) {
        return ApplyOutcome { state: "failed", detail: format!("{e}{bak_note}") };
    }
    let back = match read_tags(path) {
        Ok(m) => m,
        Err(e) => return ApplyOutcome { state: "failed", detail: format!("读回复核失败: {e}{bak_note}") },
    };
    if let WashRule::Blank = rule {
        let left = blank_leftovers(&back);
        if !left.is_empty() {
            return ApplyOutcome { state: "failed", detail: format!("读回复核失败：blank 后仍残留 {}{bak_note}", left.join(", ")) };
        }
    }
    if ffprobe_check {
        if !crate::cli::doctor::ffprobe_available() {
            return ApplyOutcome { state: "applied", detail: format!("ffprobe 不可用，已跳过独立复核（标签写入与读回复核已完成）{bak_note}") };
        }
        let cmd = std::process::Command::new("ffprobe").args(["-v", "error", "-i"]).arg(path).output();
        match cmd {
            Ok(o) if !o.status.success() => {
                let msg = String::from_utf8_lossy(&o.stderr).chars().take(60).collect::<String>();
                return ApplyOutcome { state: "failed", detail: format!("ffprobe 独立复核失败（标签已改写；音频可能损坏）{msg}{bak_note}") }
            }
            Ok(_) => {}
            Err(e) => return ApplyOutcome { state: "failed", detail: format!("ffprobe 执行失败: {e}{bak_note}") },
        }
    }
    let detail = match rule {
        WashRule::Blank => format!("blank 已落地（读回复核无残留；音频 hash 校验通过）{bak_note}"),
        WashRule::Unset(k) => format!("unset 已落地（{}；音频 hash 校验通过）{bak_note}", k.join(", ")),
    };
    ApplyOutcome { state: "applied", detail }
}

fn one_flag(flags: &std::collections::HashMap<String, crate::cli::args::Flag>, k: &str) -> Result<Option<String>, UsageError> {
    Ok(flags.get(k).and_then(|f| f.values.first()).cloned())
}
fn boolf(flags: &std::collections::HashMap<String, crate::cli::args::Flag>, k: &str) -> bool {
    flags.get(k).map(|f| f.as_bool()).unwrap_or(false)
}

pub fn run_wash(argv: &[&str], io: &dyn CommandIO) -> Result<i32, UsageError> {
    if wants_help(argv) { io.log(WASH_USAGE); return Ok(0) }
    let parsed = parse_args(argv)?;
    parsed.reject_unknown(&["blank", "unset", "apply", "bak", "ffprobe-check", "events", "json", "preview"],
        "wash 支持: --blank --unset --apply --bak --ffprobe-check --events --json --preview")?;
    parsed.require(1, WASH_USAGE)?;
    let dir = Path::new(&parsed.positionals[0]);
    let rule = wash_rule_of(&parsed.flags)?.ok_or_else(|| UsageError::new("需要 --blank 或 --unset 指定规则（一次一个）"))?;
    let apply = boolf(&parsed.flags, "apply");
    let bak = boolf(&parsed.flags, "bak");
    let ffprobe_check = boolf(&parsed.flags, "ffprobe-check");
    let json = boolf(&parsed.flags, "json");
    let events = one_flag(&parsed.flags, "events")?;

    if !dir.is_dir() { io.error(&format!("不是目录: {}", dir.display())); return Ok(1) }

    let report = match scan_dir(dir) {
        Ok(r) => r,
        Err(e) => { io.error(&format!("扫描失败: {e}")); return Ok(1) }
    };
    if report.entries.is_empty() {
        io.error(&format!("目录中没有可处理文件: {}", dir.display()));
        return Ok(1);
    }

    // 事件流：--events 时追加写 NDJSON 文件（ARCHITECTURE §12.6，不写死 stdout）
    let file_sink = events.as_ref().map(|f| ndjson_sink(f));
    let emit = |path: &str, state: &str, detail: &str| {
        let ev = crate::logger::WashEvent {
            ts: crate::logger::now_iso(), run: "cli".into(),
            kind: crate::logger::EventKind::File, cmd: "wash".into(),
            path: Some(path.to_string()), state: Some(state.to_string()),
            detail: Some(detail.to_string()), counts: None,
        };
        if let Some(sk) = &file_sink { sk(&ev) }
    };

    let mut n_applied = 0u64;
    let mut n_failed = 0u64;
    let mut n_skipped = 0u64;
    let mut lines: Vec<String> = Vec::new();

    for e in &report.entries {
        let rel = rel_path(Path::new(&e.path), dir);
        match e.level {
            Level::Ok | Level::Warn => {
                if apply {
                    let out = apply_one(Path::new(&e.path), &rule, bak, ffprobe_check);
                    emit(&rel, out.state, &out.detail);
                    if out.state == "applied" { n_applied += 1 } else { n_failed += 1 }
                    lines.push(format!("  [{rel}] → {} — {}", out.state, out.detail));
                } else {
                    let note = match &rule { WashRule::Blank => "将重建为空标签".into(), WashRule::Unset(k) => format!("将移除 {}", k.join(", ")) };
                    lines.push(format!("  [{rel}] → preview — {note}（当前 {}）", e.level.as_str()));
                }
            }
            Level::Rejected | Level::Broken => {
                n_skipped += 1;
                lines.push(format!("  [{rel}] → skipped — {}（{}）", e.level.as_str(), e.error.as_deref().unwrap_or("未知原因")));
            }
        }
    }

    if json {
        io.log(&serde_json::json!({
            "dir": report.dir, "apply": apply, "bak": bak,
            "applied": n_applied, "failed": n_failed, "skipped": n_skipped,
            "lines": lines,
        }).to_string());
        return Ok(if n_failed > 0 { 1 } else { 0 });
    }

    io.log(&format!("{} — {}", if apply { "apply" } else { "preview" }, dir.display()));
    for l in &lines { io.log(l) }
    io.log(&format!("\n汇总: applied {} / failed {} / skipped {}（共 {}）",
        n_applied, n_failed, n_skipped, report.entries.len()));
    Ok(if n_failed > 0 { 1 } else { 0 })
}
