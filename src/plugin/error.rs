//! 插件子系统的结构化错误 —— S9 清单解析 / S10 协议解析。
//!
//! 风格与 `tag::read::ReadError` 一致：手写枚举 + `Display`（中文消息），
//! 不引 thiserror，也不靠字符串匹配分类。

/// 清单解析错误（S9）。
#[derive(Debug)]
pub enum ManifestError {
    /// 文件打开/读取失败。非 UTF-8 字节也走这里（`BufRead::read_line` 报 InvalidData）
    Io(std::io::Error),
    /// 前 32 行内没有 `@music-robot`
    NoMarker,
    /// 有 `@music-robot` 但没有配对的 `@end`
    NoEnd,
    /// 清单 JSON 非法。`line` 已换算成源文件里的绝对行号
    BadJson { line: usize, message: String },
    /// 缺必填字段（name / kind / protocol）
    MissingField { field: &'static str },
    /// 协议版本不支持
    UnsupportedProtocol { found: u32, supported: u32 },
    /// 插件类型不支持
    UnsupportedKind { found: String },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Io(e) => write!(f, "读取插件清单失败：{e}"),
            ManifestError::NoMarker => write!(f, "插件清单缺少 @music-robot 标记（只在前 32 行内查找）"),
            ManifestError::NoEnd => write!(f, "插件清单缺少 @end 标记（@music-robot 未闭合）"),
            ManifestError::BadJson { line, message } => {
                write!(f, "插件清单第 {line} 行 JSON 非法：{message}")
            }
            ManifestError::MissingField { field } => write!(f, "插件清单缺少必填字段：{field}"),
            ManifestError::UnsupportedProtocol { found, supported } => {
                write!(f, "不支持的插件协议版本 {found}（本服务只支持 {supported}）")
            }
            ManifestError::UnsupportedKind { found } => {
                write!(f, "不支持的插件类型 {found:?}（仅支持 scraper / provider / mv_provider）")
            }
        }
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ManifestError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ManifestError {
    fn from(e: std::io::Error) -> Self { ManifestError::Io(e) }
}

/// 协议解析错误（S10）。
#[derive(Debug, PartialEq)]
pub enum ProtocolError {
    /// JSON 本身非法，或结构不符合协议（未知 action / 未知错误码 / 字段类型错）
    BadJson(String),
    /// 缺必填字段；嵌套字段用 `"error.code"` 这样的带前缀名字
    MissingField(&'static str),
    /// 协议版本不支持
    UnsupportedProtocol(u32),
    /// cover.path 不是安全的相对路径
    BadCoverPath(String),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::BadJson(m) => write!(f, "协议 JSON 非法：{m}"),
            ProtocolError::MissingField(field) => write!(f, "协议消息缺少必填字段：{field}"),
            ProtocolError::UnsupportedProtocol(v) => {
                write!(f, "不支持的协议版本 {v}（本服务只支持 1）")
            }
            ProtocolError::BadCoverPath(p) => {
                write!(f, "cover.path 必须是相对 work_dir 的安全相对路径，收到 {p:?}")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_error_display_is_chinese_and_never_panics() {
        let cases = [
            ManifestError::from(std::io::Error::new(std::io::ErrorKind::NotFound, "no file")),
            ManifestError::NoMarker,
            ManifestError::NoEnd,
            ManifestError::BadJson { line: 7, message: "expected value".into() },
            ManifestError::MissingField { field: "kind" },
            ManifestError::UnsupportedProtocol { found: 2, supported: 1 },
            ManifestError::UnsupportedKind { found: "weird".into() },
        ];
        for e in &cases {
            let text = e.to_string();
            assert!(!text.is_empty());
            assert!(text.contains("插件"), "错误消息应为中文：{text}");
        }
        // Io 变体应当把底层错误暴露为 source
        assert!(std::error::Error::source(&cases[0]).is_some());
        assert!(std::error::Error::source(&cases[1]).is_none());
        // 实现 std::error::Error，可装箱
        let _boxed: Box<dyn std::error::Error> = Box::new(ManifestError::NoEnd);
    }

    #[test]
    fn protocol_error_display_is_chinese() {
        let cases = [
            ProtocolError::BadJson("oops".into()),
            ProtocolError::MissingField("id"),
            ProtocolError::UnsupportedProtocol(9),
            ProtocolError::BadCoverPath("/etc/passwd".into()),
        ];
        for e in &cases {
            let text = e.to_string();
            assert!(!text.is_empty());
            assert!(text.contains("协议") || text.contains("cover"), "错误消息应为中文：{text}");
        }
        let _boxed: Box<dyn std::error::Error> = Box::new(ProtocolError::MissingField("id"));
    }

    #[test]
    fn protocol_error_equality_is_structural() {
        assert_eq!(ProtocolError::MissingField("id"), ProtocolError::MissingField("id"));
        assert_ne!(ProtocolError::MissingField("id"), ProtocolError::MissingField("protocol"));
        assert_ne!(ProtocolError::UnsupportedProtocol(1), ProtocolError::UnsupportedProtocol(2));
    }
}
