//! 参数解析 —— 移植自 src/cli/args.ts（零依赖手写解析，不引 clap）
//!
//! 规则（与 TS 完全一致）：
//!   --key=value  → 内联值
//!   --key value  → 下一参数为值（除非它本身以 -- 开头）
//!   --key        → 布尔
//!   重复 --key   → 收集为数组
//!   --           → UsageError（空 flag）
//!
//! ⚠️ `-h` 不以 `--` 开头，会落进位置参数。各子命令必须在 parse 前自行识别，
//! 否则 `--help` 会被当成文件路径（TS 也这么处理，见各 run* 的第一个 if）。
use std::collections::HashMap;

/// 用法错误分类——结构化枚举，不靠字符串匹配
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageErrorKind {
    UnknownFlag,
    DuplicateFlag,
    NotANumber,
    Usage,
    Other,
}

#[derive(Debug)]
pub struct UsageError {
    pub kind: UsageErrorKind,
    pub message: String,
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for UsageError {}

impl UsageError {
    /// 便捷构造（自动分类）——向后兼容现有调用点
    pub fn new(msg: impl Into<String>) -> Self {
        let message = msg.into();
        let kind = Self::classify(&message);
        UsageError { kind, message }
    }

    fn classify(msg: &str) -> UsageErrorKind {
        if msg.contains("未知") {
            UsageErrorKind::UnknownFlag
        } else if msg.contains("只能出现一次") {
            UsageErrorKind::DuplicateFlag
        } else if msg.contains("必须是数字") {
            UsageErrorKind::NotANumber
        } else if msg.starts_with("用法") || msg.starts_with("music-robot") {
            UsageErrorKind::Usage
        } else {
            UsageErrorKind::Other
        }
    }
}

/// 单个 flag 的值。`bare` = 出现过「不带值」的形态。
#[derive(Debug, Clone, Default)]
pub struct Flag {
    pub bare: bool,
    pub values: Vec<String>,
}
impl Flag {
    pub fn is_set(&self) -> bool { self.bare || !self.values.is_empty() }
    /// 布尔语义（--key / --key=true / --key=false）
    pub fn as_bool(&self) -> bool {
        if self.bare { return true }
        matches!(self.values.first().map(|s| s.as_str()), Some("true"))
    }
    /// 单值：重复出现视为用法错误（TS singleFlag）
    pub fn single(&self, key: &str) -> Result<Option<String>, UsageError> {
        if self.bare && self.values.is_empty() { return Ok(None) }
        if self.values.len() > 1 { return Err(UsageError::new(format!("--{key} 只能出现一次"))) }
        Ok(self.values.first().cloned())
    }
    /// 多值：重复收集（TS multiFlag）。bare 且无值 → None
    pub fn multi(&self) -> Option<Vec<String>> {
        if self.values.is_empty() { return None }
        Some(self.values.clone())
    }
}

#[derive(Debug, Default)]
pub struct ParsedArgs {
    pub flags: HashMap<String, Flag>,
    pub positionals: Vec<String>,
}
impl ParsedArgs {
    pub fn reject_unknown(&self, allowed: &[&str], hint: &str) -> Result<(), UsageError> {
        let mut bad: Vec<String> = self.flags.keys().filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
        if bad.is_empty() { return Ok(()) }
        bad.sort();
        Err(UsageError::new(format!("未知选项: --{}（{hint}）", bad.join(" --"))))
    }
    pub fn require(&self, n: usize, usage: &str) -> Result<(), UsageError> {
        if self.positionals.len() != n { Err(UsageError::new(usage.to_string())) } else { Ok(()) }
    }
}

/// 解析 argv（不含程序名本身）
pub fn parse_args(argv: &[&str]) -> Result<ParsedArgs, UsageError> {
    let mut flags: HashMap<String, Flag> = HashMap::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let token = argv[i];
        if let Some(rest) = token.strip_prefix("--") {
            let (key, inline) = match rest.find('=') {
                Some(p) => (rest[..p].to_string(), Some(rest[p + 1..].to_string())),
                None => (rest.to_string(), None),
            };
            if key.is_empty() { return Err(UsageError::new(format!("空 flag: {token}"))) }
            match inline {
                Some(v) => flags.entry(key).or_default().values.push(v),
                None => {
                    let f = flags.entry(key).or_default();
                    if i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
                        i += 1;
                        f.values.push(argv[i].to_string());
                    } else {
                        f.bare = true;
                    }
                }
            }
        } else {
            positionals.push(token.to_string());
        }
        i += 1;
    }
    Ok(ParsedArgs { flags, positionals })
}

/// JS 语义的 parseInt：取前缀数字，`parseInt("5/12") == 5`；无数字 → NaN（用法错误）
fn js_parse_int(s: &str) -> Option<i64> {
    let t = s.trim_start();
    let (neg, rest) = if let Some(r) = t.strip_prefix('-') { (true, r) }
                       else if let Some(r) = t.strip_prefix('+') { (false, r) }
                       else { (false, t) };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() { return None }
    digits.parse::<i64>().ok().map(|n| if neg { -n } else { n })
}

/// 数字 flag（TS parseIntFlag）。取值失败 → UsageError
pub fn int_flag(flag: Option<&Flag>, key: &str) -> Result<Option<i64>, UsageError> {
    let Some(v) = flag.map(|f| f.values.first()).flatten() else { return Ok(None) };
    if v.is_empty() { return Ok(None) }
    match js_parse_int(v) {
        Some(n) => Ok(Some(n)),
        None => Err(UsageError::new(format!("--{key} 必须是数字（收到 {v:?}）"))),
    }
}

/// 前置识别 -h/--help：parse 只认 `--` 前缀，`-h` 会落进位置参数
pub fn wants_help(argv: &[&str]) -> bool {
    argv.iter().any(|a| *a == "-h" || *a == "--help")
}
