//! src/scanner.rs — 递归枚举 + 只读体检分级（移植自 src/scanner.ts）
//!
//! 语义规格：ARCHITECTURE §11（ok/warn/rejected/broken 四分级；每文件独立 try/catch，
//! 单文件不拖垮整目录；目录不可达 / 零命中由 CLI 层报 exit 1）
use std::path::{Path, PathBuf};

use crate::tag::read::{read_tags, warnings_for, probe_format, Format, Probe, AudioMetadata, Warning};

/// 分级结果（与 TS ScanEntry.level 一致）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level { Ok, Warn, Rejected, Broken }
impl Level { pub fn as_str(&self) -> &'static str {
    match self { Level::Ok => "ok", Level::Warn => "warn", Level::Rejected => "rejected", Level::Broken => "broken" } } }

#[derive(Debug, Clone)]
pub struct ScanEntry {
    pub path: String,
    pub level: Level,
    pub format: String,
    pub title: Option<String>,
    pub artists_summary: Option<String>,
    pub warnings: Vec<Warning>,
    pub error: Option<String>,
    pub frame_count: Option<usize>,
    /// read_json 视图（sanitize 过、含 mbid）；rejected/broken 无
    pub view: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct ScanReport {
    pub dir: String,
    pub entries: Vec<ScanEntry>,
    pub counts: Counts,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct Counts { pub ok: usize, pub warn: usize, pub rejected: usize, pub broken: usize, pub total: usize }

const AUDIO_EXT: &[&str] = &["mp3", "flac", "wav"];

/// 递归枚举目录下的 .mp3/.flac/.wav（大小写不敏感；路径排序稳定）
pub fn scan_files(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    if !dir.is_dir() {
        return Err(std::io::Error::other(format!("不是目录: {}", dir.display())));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    walk(dir, &mut out);
    out.sort();
    Ok(out)
}
fn walk(d: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, out); continue }
        let Some(ext) = p.extension().and_then(|s| s.to_str()).map(|s| s.to_lowercase()) else { continue };
        if AUDIO_EXT.contains(&ext.as_str()) { out.push(p) }
    }
}

/// 单文件只读体检（绝不抛：异常归入 rejected/broken）
pub fn inspect_file(path: &Path) -> ScanEntry {
    let probe: Option<Probe> = Some(probe_format(path));
    let format = format_name(probe.as_ref().unwrap());
    // ⚠️ 分级按**错误变体**而不是错误文案：文案匹配脆弱（P2-4）。
    //   早期实现靠 `msg.contains("无法识别")` 判 rejected，但 `ReadError::Unrecognized`
    //   的文案里恰好有「无法识别」四个字，导致「FLAC 魔数成立但块序列损坏」被误判成
    //   rejected（TS 参照实现判 broken）。改为按变体映射：
    //     Id3PrefixedReal / Id3PrefixedUnknown → rejected（结构上明确拒绝的容器伪装）
    //     Unrecognized / Io / Escape           → broken（识别到格式但解析/IO 失败）
    use crate::tag::read::ReadError;
    enum Reader {
        Both(AudioMetadata, Vec<u8>),
        Rejected(String),
        Broken(String),
    }
    let reader: Reader = match read_tags(path) {
        Ok(meta) => match std::fs::read(path) {
            Ok(buf) => Reader::Both(meta, buf),
            Err(e) => Reader::Broken(format!("读取失败: {e}")),
        },
        Err(ReadError::Id3PrefixedReal(_) | ReadError::Id3PrefixedUnknown) => {
            Reader::Rejected("无法识别：ID3v2 前缀但容器不可解析（伪装文件或未知标签链）".into())
        }
        Err(e) => Reader::Broken(e.to_string()),
    };
    match reader {
        Reader::Both(meta, buf) => {
            let warnings = warnings_for(&meta, &buf);
            let view = Some(read_json_value_from_meta(&meta, path.display().to_string(), &warnings));
            ScanEntry {
                path: path.display().to_string(),
                level: if warnings.is_empty() { Level::Ok } else { Level::Warn },
                format,
                title: meta.title.clone(),
                artists_summary: (!meta.artists.is_empty()).then(|| meta.artists.join(" / ")),
                warnings,
                error: None,
                frame_count: Some(meta.raw_frames.len()),
                view,
            }
        }
        Reader::Rejected(msg) => ScanEntry {
            path: path.display().to_string(), level: Level::Rejected, format,
            title: None, artists_summary: None, warnings: vec![],
            error: Some(msg), frame_count: None, view: None,
        },
        Reader::Broken(msg) => ScanEntry {
            path: path.display().to_string(), level: Level::Broken, format,
            title: None, artists_summary: None, warnings: vec![],
            error: Some(msg), frame_count: None, view: None,
        },
    }
}

fn read_json_value_from_meta(meta: &AudioMetadata, file: String, warnings: &[Warning]) -> serde_json::Value {
    crate::tag::read::read_json_value(meta, &file, warnings)
}

fn format_name(p: &Probe) -> String {
    match p {
        Probe::Plain(Format::Mp3) => "mp3".into(),
        Probe::Plain(Format::Flac) => "flac".into(),
        Probe::Plain(Format::Wav) => "wav".into(),
        _ => "unknown".into(),
    }
}

/// 目录体检（纯函数）：逐文件 try/catch（单文件不拖垮整目录）
pub fn scan_dir(dir: &Path) -> Result<ScanReport, std::io::Error> {
    let files = scan_files(dir)?;
    let entries: Vec<ScanEntry> = files.iter().map(|p| inspect_file(p)).collect();
    let mut counts = Counts { total: entries.len(), ..Default::default() };
    for e in &entries {
        match e.level {
            Level::Ok => counts.ok += 1,
            Level::Warn => counts.warn += 1,
            Level::Rejected => counts.rejected += 1,
            Level::Broken => counts.broken += 1,
        }
    }
    Ok(ScanReport { dir: dir.display().to_string(), entries, counts })
}

/// 展示用相对路径
pub fn rel_path(p: &Path, dir: &Path) -> String {
    std::path::Path::strip_prefix(p, dir).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string())
}
