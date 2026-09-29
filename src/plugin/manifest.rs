//! S9 · 插件清单解析 —— 从脚本文件头注释里读出插件元数据。
//!
//! 清单形如（JS；Python / Shell 把 // 换成 #）：
//!
//!     #!/usr/bin/env node
//!     // @music-robot
//!     // {
//!     //   "name": "netease",
//!     //   "kind": "scraper",
//!     //   "protocol": 1
//!     // }
//!     // @end
//!
//! 硬约束：**绝不把整个插件读进内存**——插件可能是几百 MB 的资源包。
//! 这里只读文件头，且同时受「32 行」与「64 KiB」两个上限约束。
//!
//! 与规格的一处实现选择：清单各行拼接时用换行符分隔（而不是零分隔符）。
//! 对示例这种每行都是完整 JSON 片段的写法两者等价，但换行分隔能让 serde_json
//! 报出准确的相对行号，从而换算成源文件里的绝对行号。

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::error::ManifestError;
use super::protocol::PROTOCOL_VERSION;

/// 清单开始标记
pub const MARKER: &str = "@music-robot";
/// 清单结束标记
pub const END_MARKER: &str = "@end";
/// 只在前 32 行里找标记
pub const MAX_HEAD_LINES: usize = 32;
/// 头部读取的字节上限（防止「32 行但每行 100 MB」）
pub const MAX_HEAD_BYTES: u64 = 64 * 1024;
/// timeout_ms 缺省值
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// max_concurrency 缺省值
pub const DEFAULT_MAX_CONCURRENCY: u32 = 1;

/// 允许出现在行首的注释符。// 与 /* 是两字符前缀，其余单字符。
const COMMENT_PREFIXES: [&str; 6] = ["//", "/*", "#", "*", "--", ";"];

/// 插件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    /// 抓取元数据 / 封面 / 歌词
    Scraper,
    /// 提供音源（下载音频）
    Provider,
    /// 提供 MV 源
    MvProvider,
}

impl PluginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginKind::Scraper => "scraper",
            PluginKind::Provider => "provider",
            PluginKind::MvProvider => "mv_provider",
        }
    }

    /// 严格按规格的三个字面量解析（大小写敏感），其余一律 None
    pub fn parse(s: &str) -> Option<PluginKind> {
        match s {
            "scraper" => Some(PluginKind::Scraper),
            "provider" => Some(PluginKind::Provider),
            "mv_provider" => Some(PluginKind::MvProvider),
            _ => None,
        }
    }
}

/// 解析后的插件元数据
#[derive(Debug, Clone, PartialEq)]
pub struct PluginMeta {
    pub name: String,
    pub kind: PluginKind,
    pub protocol: u32,
    /// 缺省空数组
    pub capabilities: Vec<String>,
    /// 显式 command 优先；否则按扩展名推断
    pub command: Vec<String>,
    /// 缺省 30000
    pub timeout_ms: u64,
    /// 缺省 1（0 会被兜底成 1，否则信号量死锁）
    pub max_concurrency: u32,
    /// 插件文件路径（原样保留，不做 canonicalize）
    pub path: PathBuf,
}

/// 读文件并解析清单。文件不存在 / 不可读 / 头部非 UTF-8 → Io。
pub fn parse_manifest(path: &Path) -> Result<PluginMeta, ManifestError> {
    let head = read_head_lines(path)?;
    parse_lines(&head, path)
}

/// 直接从文本解析（便于单测，不碰文件系统）。同样只取前 32 行。
pub fn parse_manifest_str(text: &str, path: &Path) -> Result<PluginMeta, ManifestError> {
    let head: Vec<String> = text.lines().take(MAX_HEAD_LINES).map(str::to_string).collect();
    parse_lines(&head, path)
}

// ─────────────────────── 内部实现 ───────────────────────

/// 只读文件头：最多 MAX_HEAD_LINES 行，且总字节数不超过 MAX_HEAD_BYTES。
///
/// 用 read_line 逐行读（而不是 read_to_string 整读），保证大插件不会撑爆内存；
/// 非 UTF-8 字节由 read_line 报 InvalidData，经 From 变成 ManifestError::Io。
fn read_head_lines(path: &Path) -> Result<Vec<String>, ManifestError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file.take(MAX_HEAD_BYTES));
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    while out.len() < MAX_HEAD_LINES {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        out.push(buf.trim_end_matches(|c| c == '\n' || c == '\r').to_string());
    }
    Ok(out)
}

/// 去掉行首空白 + 注释符 + 空白。可重复剥离（/* 、* 混排的块注释也能吃下）。
fn strip_comment(raw: &str) -> &str {
    let mut s = raw.trim();
    loop {
        let mut hit = false;
        for p in COMMENT_PREFIXES {
            if let Some(rest) = s.strip_prefix(p) {
                s = rest.trim_start();
                hit = true;
                break;
            }
        }
        if !hit {
            break;
        }
    }
    s
}

/// 从 serde_json 的错误文本里抠出相对行号（形如 "at line 3 column 5"）。
/// 抠不到就按第 1 行算——绝不 panic。
fn json_error_line(e: &serde_json::Error) -> usize {
    e.to_string()
        .split(" at line ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(1)
}

/// 扫描 @music-robot … @end 注释块并校验字段。lines 的下标 + 1 即源文件行号。
fn parse_lines(lines: &[String], path: &Path) -> Result<PluginMeta, ManifestError> {
    let mut marker_line: Option<usize> = None;
    let mut end_seen = false;
    let mut body: Vec<&str> = Vec::new();

    for (idx, raw) in lines.iter().enumerate() {
        let lineno = idx + 1;
        let content = strip_comment(raw);
        if marker_line.is_none() {
            if content == MARKER {
                marker_line = Some(lineno);
            }
            continue;
        }
        if content == END_MARKER {
            end_seen = true;
            break;
        }
        body.push(content);
    }

    let marker_line = marker_line.ok_or(ManifestError::NoMarker)?;
    if !end_seen {
        return Err(ManifestError::NoEnd);
    }

    let first_body_line = marker_line + 1;
    let json_text = body.join("\n");
    let root: Value = serde_json::from_str(&json_text).map_err(|e| ManifestError::BadJson {
        line: first_body_line + json_error_line(&e).saturating_sub(1),
        message: e.to_string(),
    })?;
    let obj = root.as_object().ok_or_else(|| ManifestError::BadJson {
        line: first_body_line,
        message: "清单根节点必须是 JSON 对象".to_string(),
    })?;

    // name：必须是非空字符串
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(ManifestError::MissingField { field: "name" })?
        .to_string();

    // kind：缺失 → MissingField，非法字面量 → UnsupportedKind
    let kind_raw = obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(ManifestError::MissingField { field: "kind" })?;
    let kind = PluginKind::parse(kind_raw).ok_or_else(|| ManifestError::UnsupportedKind {
        found: kind_raw.to_string(),
    })?;

    // protocol：缺失 → MissingField，版本不对 → UnsupportedProtocol
    let protocol = obj
        .get("protocol")
        .and_then(value_to_u32)
        .ok_or(ManifestError::MissingField { field: "protocol" })?;
    if protocol != PROTOCOL_VERSION {
        return Err(ManifestError::UnsupportedProtocol {
            found: protocol,
            supported: PROTOCOL_VERSION,
        });
    }

    let capabilities = match obj.get("capabilities") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => Vec::new(),
    };

    // command：显式且非空则原样用；否则按扩展名推断
    let command = match obj.get("command") {
        Some(Value::Array(a)) => {
            let list: Vec<String> = a.iter().filter_map(Value::as_str).map(str::to_string).collect();
            if list.is_empty() {
                infer_command(path)
            } else {
                list
            }
        }
        _ => infer_command(path),
    };

    let timeout_ms = obj
        .get("timeout_ms")
        .and_then(value_to_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS);

    // max_concurrency = 0 会让信号量永远拿不到许可（真死锁），兜底成 1
    let max_concurrency = obj
        .get("max_concurrency")
        .and_then(value_to_u32)
        .unwrap_or(DEFAULT_MAX_CONCURRENCY)
        .max(1);

    Ok(PluginMeta {
        name,
        kind,
        protocol,
        capabilities,
        command,
        timeout_ms,
        max_concurrency,
        path: path.to_path_buf(),
    })
}

/// 按扩展名推断启动命令（小写比较）
fn infer_command(path: &Path) -> Vec<String> {
    let p = path.to_string_lossy().into_owned();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "js" => vec!["node".to_string(), p],
        "py" => vec!["python3".to_string(), p],
        "sh" => vec!["sh".to_string(), p],
        _ => vec![p],
    }
}

/// 数字或数字字符串 → u32（清单里写 "1" 也认）
fn value_to_u32(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|x| u32::try_from(x).ok()),
        Value::String(s) => s.trim().parse::<u32>().ok(),
        _ => None,
    }
}

fn value_to_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JS_MANIFEST: &str = r##"#!/usr/bin/env node
// @music-robot
// {
//   "name": "netease",
//   "kind": "scraper",
//   "protocol": 1,
//   "capabilities": ["metadata", "cover", "lyrics"],
//   "timeout_ms": 30000,
//   "max_concurrency": 1
// }
// @end
console.log('scan');
"##;

    const PY_MANIFEST: &str = r##"#!/usr/bin/env python3
# @music-robot
# {
#   "name": "musicbrainz",
#   "kind": "provider",
#   "protocol": 1,
#   "timeout_ms": 15000,
#   "max_concurrency": 4
# }
# @end
print("hi")
"##;

    const SH_MANIFEST: &str = r##"#!/bin/sh
# @music-robot
# {
#   "name": "mvbox",
#   "kind": "mv_provider",
#   "protocol": 1
# }
# @end
echo hi
"##;

    fn p(name: &str) -> PathBuf { PathBuf::from(name) }

    /// 用单行 JSON 造一个清单文本
    fn manifest_with(json: &str, path: &str) -> Result<PluginMeta, ManifestError> {
        let text = format!("// @music-robot\n// {json}\n// @end\n");
        parse_manifest_str(&text, &p(path))
    }

    fn command_of(json: &str, path: &str) -> Vec<String> {
        manifest_with(json, path).expect("应当解析成功").command
    }

    /// 写一个临时插件文件（测试间用 tag 区分，避免并行互相覆盖）
    fn write_tmp(tag: &str, bytes: &[u8]) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("music-robot-plugin-{}-{}.js", std::process::id(), tag));
        std::fs::write(&path, bytes).expect("写临时文件失败");
        path
    }

    fn pad_lines(n: usize) -> String {
        let mut s = String::new();
        for i in 1..=n {
            s.push_str(&format!("// 填充 {i}\n"));
        }
        s
    }

    // ── 三种注释风格 ──

    #[test]
    fn js_manifest_parses() {
        let meta = parse_manifest_str(JS_MANIFEST, &p("/opt/plugins/netease.js")).expect("解析失败");
        assert_eq!(meta.name, "netease");
        assert_eq!(meta.kind, PluginKind::Scraper);
        assert_eq!(meta.protocol, 1);
        assert_eq!(meta.capabilities, vec!["metadata", "cover", "lyrics"]);
        assert_eq!(meta.command, vec!["node", "/opt/plugins/netease.js"]);
        assert_eq!(meta.timeout_ms, 30_000);
        assert_eq!(meta.max_concurrency, 1);
        assert_eq!(meta.path, p("/opt/plugins/netease.js"));
    }

    #[test]
    fn python_manifest_parses() {
        let meta = parse_manifest_str(PY_MANIFEST, &p("/opt/plugins/mb.py")).expect("解析失败");
        assert_eq!(meta.name, "musicbrainz");
        assert_eq!(meta.kind, PluginKind::Provider);
        assert_eq!(meta.protocol, 1);
        assert_eq!(meta.timeout_ms, 15_000);
        assert_eq!(meta.max_concurrency, 4);
        assert_eq!(meta.capabilities, Vec::<String>::new(), "capabilities 缺省应为空数组");
        assert_eq!(meta.command, vec!["python3", "/opt/plugins/mb.py"]);
    }

    #[test]
    fn shell_manifest_parses() {
        let meta = parse_manifest_str(SH_MANIFEST, &p("/opt/plugins/mv.sh")).expect("解析失败");
        assert_eq!(meta.name, "mvbox");
        assert_eq!(meta.kind, PluginKind::MvProvider);
        assert_eq!(meta.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert_eq!(meta.max_concurrency, DEFAULT_MAX_CONCURRENCY);
        assert_eq!(meta.command, vec!["sh", "/opt/plugins/mv.sh"]);
    }

    #[test]
    fn shebang_does_not_break_parsing() {
        let bodies = [
            ("#!/usr/bin/env node", "//", "a.js"),
            ("#!/usr/bin/env python3", "#", "a.py"),
            ("#!/bin/sh", "#", "a.sh"),
        ];
        for (shebang, c, file) in bodies {
            let text = format!(
                "{shebang}\n{c} @music-robot\n{c} {{\"name\":\"x\",\"kind\":\"scraper\",\"protocol\":1}}\n{c} @end\n"
            );
            let meta = parse_manifest_str(&text, &p(file)).expect("带 shebang 的清单应能解析");
            assert_eq!(meta.name, "x");
        }
    }

    #[test]
    fn c_block_comment_prefixes_parse() {
        let text = "/*\n * @music-robot\n * {\n *   \"name\": \"cstyle\",\n *   \"kind\": \"scraper\",\n *   \"protocol\": 1\n * }\n * @end\n */\n";
        let meta = parse_manifest_str(text, &p("a.js")).expect("C 风格块注释应能解析");
        assert_eq!(meta.name, "cstyle");
    }

    #[test]
    fn semicolon_and_dash_prefixes_parse() {
        for c in [";", "--"] {
            let text = format!(
                "{c} @music-robot\n{c} {{\"name\":\"x\",\"kind\":\"provider\",\"protocol\":1}}\n{c} @end\n"
            );
            let meta = parse_manifest_str(&text, &p("a.js")).expect("前缀应能解析");
            assert_eq!(meta.name, "x");
        }
    }

    // ── 错误路径 ──

    #[test]
    fn missing_end_is_no_end() {
        let text = "// @music-robot\n// {\"name\":\"x\",\"kind\":\"scraper\",\"protocol\":1}\n";
        match parse_manifest_str(text, &p("a.js")) {
            Err(ManifestError::NoEnd) => {}
            other => panic!("期望 NoEnd，实际 {other:?}"),
        }
    }

    #[test]
    fn missing_marker_is_no_marker() {
        let text = "// 只是普通注释\nconsole.log(1)\n";
        match parse_manifest_str(text, &p("a.js")) {
            Err(ManifestError::NoMarker) => {}
            other => panic!("期望 NoMarker，实际 {other:?}"),
        }
    }

    #[test]
    fn marker_must_match_exactly() {
        // 内容必须「恰好等于」标记，多一个后缀就不算
        let text = "// @music-robot-x\n// {}\n// @end\n";
        match parse_manifest_str(text, &p("a.js")) {
            Err(ManifestError::NoMarker) => {}
            other => panic!("期望 NoMarker，实际 {other:?}"),
        }
    }

    #[test]
    fn bad_json_is_bad_json_with_absolute_line() {
        // 标记在第 1 行，JSON 体从第 2 行开始 → 报错行号应为 2
        let text = "// @music-robot\n// {\"name\": \"x\", }\n// @end\n";
        match parse_manifest_str(text, &p("a.js")) {
            Err(ManifestError::BadJson { line, .. }) => assert_eq!(line, 2),
            other => panic!("期望 BadJson，实际 {other:?}"),
        }
    }

    #[test]
    fn bad_json_line_is_absolute_for_multiline_body() {
        // 标记第 1 行，JSON 体第 2..5 行，第 4 行非法 → 绝对行号 4
        let text = "// @music-robot\n// {\n//   \"name\": \"x\",\n//   oops\n// }\n// @end\n";
        match parse_manifest_str(text, &p("a.js")) {
            Err(ManifestError::BadJson { line, .. }) => assert_eq!(line, 4),
            other => panic!("期望 BadJson，实际 {other:?}"),
        }
    }

    #[test]
    fn empty_body_is_bad_json() {
        let text = "// @music-robot\n// @end\n";
        assert!(matches!(parse_manifest_str(text, &p("a.js")), Err(ManifestError::BadJson { .. })));
    }

    #[test]
    fn missing_required_fields_are_reported() {
        let cases = [
            (r#"{"kind":"scraper","protocol":1}"#, "name"),
            (r#"{"name":"x","protocol":1}"#, "kind"),
            (r#"{"name":"x","kind":"scraper"}"#, "protocol"),
            (r#"{"name":"","kind":"scraper","protocol":1}"#, "name"),
            (r#"{"name":"x","kind":7,"protocol":1}"#, "kind"),
        ];
        for (json, want) in cases {
            match manifest_with(json, "a.js") {
                Err(ManifestError::MissingField { field }) => assert_eq!(field, want, "json={json}"),
                other => panic!("期望缺字段 {want}（json={json}），实际 {other:?}"),
            }
        }
    }

    #[test]
    fn unsupported_protocol_is_reported() {
        match manifest_with(r#"{"name":"x","kind":"scraper","protocol":2}"#, "a.js") {
            Err(ManifestError::UnsupportedProtocol { found, supported }) => {
                assert_eq!(found, 2);
                assert_eq!(supported, 1);
            }
            other => panic!("期望 UnsupportedProtocol，实际 {other:?}"),
        }
    }

    #[test]
    fn unsupported_kind_is_reported() {
        match manifest_with(r#"{"name":"x","kind":"unknown","protocol":1}"#, "a.js") {
            Err(ManifestError::UnsupportedKind { found }) => assert_eq!(found, "unknown"),
            other => panic!("期望 UnsupportedKind，实际 {other:?}"),
        }
    }

    #[test]
    fn non_object_root_is_bad_json() {
        match manifest_with("[1,2,3]", "a.js") {
            Err(ManifestError::BadJson { .. }) => {}
            other => panic!("期望 BadJson，实际 {other:?}"),
        }
    }

    // ── 缺省与推断 ──

    #[test]
    fn defaults_are_applied() {
        let meta =
            manifest_with(r#"{"name":"x","kind":"scraper","protocol":1}"#, "a.js").expect("解析失败");
        assert_eq!(meta.capabilities, Vec::<String>::new());
        assert_eq!(meta.timeout_ms, 30_000);
        assert_eq!(meta.max_concurrency, 1);
    }

    #[test]
    fn command_inferred_from_extension() {
        let json = r#"{"name":"x","kind":"scraper","protocol":1}"#;
        assert_eq!(command_of(json, "/opt/a.js"), vec!["node", "/opt/a.js"]);
        assert_eq!(command_of(json, "/opt/a.py"), vec!["python3", "/opt/a.py"]);
        assert_eq!(command_of(json, "/opt/a.sh"), vec!["sh", "/opt/a.sh"]);
        assert_eq!(command_of(json, "/opt/a.rb"), vec!["/opt/a.rb"]);
        assert_eq!(command_of(json, "/opt/plugin"), vec!["/opt/plugin"]);
        // 扩展名大小写不敏感
        assert_eq!(command_of(json, "/opt/a.JS"), vec!["node", "/opt/a.JS"]);
    }

    #[test]
    fn explicit_command_wins() {
        let json =
            r#"{"name":"x","kind":"scraper","protocol":1,"command":["/usr/local/bin/node","--no-warnings"]}"#;
        assert_eq!(command_of(json, "/opt/a.js"), vec!["/usr/local/bin/node", "--no-warnings"]);
    }

    #[test]
    fn empty_command_falls_back_to_inference() {
        let json = r#"{"name":"x","kind":"scraper","protocol":1,"command":[]}"#;
        assert_eq!(command_of(json, "/opt/a.js"), vec!["node", "/opt/a.js"]);
    }

    #[test]
    fn numeric_strings_are_accepted() {
        let meta = manifest_with(
            r#"{"name":"x","kind":"scraper","protocol":"1","timeout_ms":"1500","max_concurrency":"3"}"#,
            "a.js",
        )
        .expect("数字字符串应当被接受");
        assert_eq!(meta.protocol, 1);
        assert_eq!(meta.timeout_ms, 1500);
        assert_eq!(meta.max_concurrency, 3);
    }

    #[test]
    fn zero_concurrency_is_clamped_to_one() {
        let meta =
            manifest_with(r#"{"name":"x","kind":"scraper","protocol":1,"max_concurrency":0}"#, "a.js")
                .expect("解析失败");
        assert_eq!(meta.max_concurrency, 1, "0 并发会死锁，必须兜底");
    }

    #[test]
    fn non_string_capabilities_are_skipped() {
        let meta = manifest_with(
            r#"{"name":"x","kind":"scraper","protocol":1,"capabilities":["a",1,null,"b"]}"#,
            "a.js",
        )
        .expect("解析失败");
        assert_eq!(meta.capabilities, vec!["a", "b"]);
    }

    // ── 窗口边界 ──

    #[test]
    fn marker_beyond_32_lines_is_no_marker() {
        // 39 行填充 + 标记 → 标记落在第 40 行
        let text = format!(
            "{}// @music-robot\n// {{\"name\":\"x\",\"kind\":\"scraper\",\"protocol\":1}}\n// @end\n",
            pad_lines(39)
        );
        match parse_manifest_str(&text, &p("a.js")) {
            Err(ManifestError::NoMarker) => {}
            other => panic!("期望 NoMarker，实际 {other:?}"),
        }
    }

    #[test]
    fn head_window_is_exactly_32_lines() {
        // 标记 + JSON + @end 恰好落在第 30 / 31 / 32 行 → 成功
        let ok = format!(
            "{}// @music-robot\n// {{\"name\":\"x\",\"kind\":\"scraper\",\"protocol\":1}}\n// @end\n",
            pad_lines(29)
        );
        assert_eq!(parse_manifest_str(&ok, &p("a.js")).expect("解析失败").name, "x");

        // 标记在第 32 行（窗口内）但 @end 在窗口外 → NoEnd（不是 NoMarker）
        let tail_off = format!("{}// @music-robot\n// {{}}\n// @end\n", pad_lines(31));
        match parse_manifest_str(&tail_off, &p("a.js")) {
            Err(ManifestError::NoEnd) => {}
            other => panic!("期望 NoEnd，实际 {other:?}"),
        }

        // 标记在第 33 行 → NoMarker
        let outside = format!("{}// @music-robot\n// {{}}\n// @end\n", pad_lines(32));
        match parse_manifest_str(&outside, &p("a.js")) {
            Err(ManifestError::NoMarker) => {}
            other => panic!("期望 NoMarker，实际 {other:?}"),
        }
    }

    // ── 文件系统路径 ──

    #[test]
    fn parse_manifest_reads_real_file() {
        let path = write_tmp("js-file", JS_MANIFEST.as_bytes());
        let got = parse_manifest(&path);
        let _ = std::fs::remove_file(&path);
        let meta = got.expect("解析真实文件失败");
        assert_eq!(meta.name, "netease");
        assert_eq!(meta.kind, PluginKind::Scraper);
        assert_eq!(meta.command, vec!["node".to_string(), path.to_string_lossy().into_owned()]);
        assert_eq!(meta.path, path);
    }

    #[test]
    fn parse_manifest_missing_file_is_io_error() {
        let mut path = std::env::temp_dir();
        path.push(format!("music-robot-plugin-missing-{}.js", std::process::id()));
        let _ = std::fs::remove_file(&path);
        match parse_manifest(&path) {
            Err(ManifestError::Io(_)) => {}
            other => panic!("期望 Io，实际 {other:?}"),
        }
    }

    #[test]
    fn non_utf8_head_does_not_panic() {
        let mut bytes =
            b"// @music-robot\n// {\"name\": \"x\", \"kind\": \"scraper\", \"protocol\": 1, \"j\": \""
                .to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(b"\"}\n// @end\n");
        let path = write_tmp("non-utf8", &bytes);
        let got = parse_manifest(&path);
        let _ = std::fs::remove_file(&path);
        match got {
            Err(ManifestError::Io(_)) | Err(ManifestError::BadJson { .. }) => {}
            other => panic!("非 UTF-8 头部应报 Io / BadJson，实际 {other:?}"),
        }
    }

    #[test]
    fn non_utf8_without_marker_does_not_panic() {
        let path = write_tmp("non-utf8-nomarker", &[0xff, 0xfe, 0x00, b'\n', 0xfd]);
        let got = parse_manifest(&path);
        let _ = std::fs::remove_file(&path);
        assert!(got.is_err(), "非 UTF-8 且无标记应当报错而不是 panic");
    }

    #[test]
    fn huge_file_head_is_bounded() {
        // 文件远大于 64 KiB，但标记在头部，应当照常解析（证明没有整读）
        let big = format!("{}{}", JS_MANIFEST, "x".repeat(256 * 1024));
        let path = write_tmp("huge", big.as_bytes());
        let got = parse_manifest(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(got.expect("大文件头部应能解析").name, "netease");
    }

    // ── 类型辅助 ──

    #[test]
    fn kind_parse_and_as_str() {
        assert_eq!(PluginKind::parse("scraper"), Some(PluginKind::Scraper));
        assert_eq!(PluginKind::parse("provider"), Some(PluginKind::Provider));
        assert_eq!(PluginKind::parse("mv_provider"), Some(PluginKind::MvProvider));
        assert_eq!(PluginKind::parse("Scraper"), None, "大小写敏感");
        assert_eq!(PluginKind::parse("mv-provider"), None);
        assert_eq!(PluginKind::parse("unknown"), None);
        assert_eq!(PluginKind::Scraper.as_str(), "scraper");
        assert_eq!(PluginKind::Provider.as_str(), "provider");
        assert_eq!(PluginKind::MvProvider.as_str(), "mv_provider");
    }

    #[test]
    fn strip_comment_strips_prefixes_and_whitespace() {
        assert_eq!(strip_comment("// @music-robot"), "@music-robot");
        assert_eq!(strip_comment("#@music-robot"), "@music-robot");
        assert_eq!(strip_comment("   *   @end"), "@end");
        assert_eq!(strip_comment("-- @end"), "@end");
        assert_eq!(strip_comment("; @end"), "@end");
        assert_eq!(strip_comment("/* @end"), "@end");
        assert_eq!(strip_comment("  \"name\": 1"), "\"name\": 1");
        assert_eq!(strip_comment(""), "");
    }
}
