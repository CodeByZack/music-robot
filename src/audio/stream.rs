//! S18 · Range 流式：HTTP Range 解析（纯函数）与音频 MIME 判定。
//!
//! 本模块只做两件**不碰 IO** 的事，所以可以逐条穷举单测：
//!
//! * [`parse_range`] —— `Range: bytes=...` 请求头 → 闭区间 [`ByteRange`]；
//! * [`content_type_for`] —— 文件路径 → `audio/mpeg` / `audio/flac` / `audio/wav`
//!   / `application/octet-stream`。
//!
//! 实际的读盘与响应组装在 `crate::server::routes::stream`。
//!
//! # 内存特性（必读，一个如实写明的取舍）
//!
//! 本步的直传走 `StorageBackend::read_range(path, offset, len)`，而它返回
//! `Vec<u8>` —— **请求多大就把多大读进内存**：
//!
//! * 播放器常见的分块请求（几十 KB ~ 几 MB）完全没问题；
//! * 但客户端发 `Range: bytes=0-`（「整首歌给我」）时，会把**整个文件**读进内存
//!   —— 一首无损 FLAC 可能几十 MB；并发几个这样的请求就是几百 MB 的瞬时占用。
//!
//! 这是 S1 已验收接口的形状决定的，S18 **不动** `src/storage.rs`（超出本步范围）。
//! **要真正零缓冲，需要给 `StorageBackend` 加一个返回 `impl Read` 的流式接口**，
//! 再把 `ReaderStream`（或等价物）接到响应体上，让内核缓冲按块推给客户端。
//!
//! 在那之前，这里**不会**为了掩盖这个问题去偷偷截断客户端请求的范围：截断会让响应体
//! 短于 `Content-Range` 声明的长度，破坏 HTTP 语义，播放器反而会当成传输中断。
//! 宁可如实把请求的区间读进内存。
//!
//! # 支持的 Range 形态（RFC 9110 §14.1.2）
//!
//! | 请求 | 含义 | 结果（file_len = 1000） |
//! |------|------|--------------------------|
//! | `bytes=0-499` | first-byte-pos / last-byte-pos | 0..=499 |
//! | `bytes=500-`  | 开放终点（到文件尾） | 500..=999 |
//! | `bytes=-500`  | 后缀长度（最后 500 字节） | 500..=999 |
//!
//! 终点超过文件尾按 RFC **截断到最后一字节**（不是错误）；起点 >= 文件长度则是
//! unsatisfiable，对应 416。`bytes=-0` 明确无效 —— RFC 9110 规定 suffix-length 为 0
//! 不满足任何字节（zero 长度的 suffix-range 是 unsatisfiable，不是「空区间」）。
//!
//! # 为什么不实现多段 Range（multipart/byteranges）
//!
//! `bytes=0-99,200-299` 这种多段请求本步**明确拒绝（416）**。理由：
//!
//! * 代价：要为每一段各生成一个 part（各自的 `Content-Type` 与 `Content-Range`），
//!   拼 multipart 边界，并保证 `Content-Length` 与各部分字节数严格相符；解析端与
//!   测试端都要跟着写一遍多段语义。
//! * 收益：音视频播放器 seek 时只请求**一个**连续区间，几乎不会发多段请求；
//!   多段主要出现在「一次抓取文件里几处零散片段」的场景（如 PDF 阅读器）。
//! * 结论：复杂度 vs 收益不划算，本步拒绝。拒绝时给出 416 + `Content-Range: bytes */<len>`，
//!   而不是假装成功返回 200 全量 —— 后者会让客户端拿到与请求不符的响应却不自知。

/// 一个**可满足**的字节闭区间（含首含尾）。
///
/// 不变式：`start <= end` 且 `end < file_len`。所以 [`ByteRange::byte_len`] 不会下溢，
/// 也不会为 0。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// 首个字节的下标（0 起）
    pub start: u64,
    /// 末个字节的下标（含）
    pub end: u64,
}

impl ByteRange {
    /// 区间覆盖的字节数（含首含尾，所以是 end - start + 1）。
    ///
    /// `end >= start` 由构造方保证，且 `end <= file_len - 1 <= u64::MAX - 1`，
    /// 因此 `+ 1` 不会溢出。
    pub fn byte_len(&self) -> u64 {
        self.end - self.start + 1
    }
}

/// `Range` 请求头解析失败的原因。
///
/// 分成两类，路由层据此选状态码：
/// * **不可满足**（[Unsatisfiable] / [ZeroSuffix] / [MultipleRanges]）→ 416；
/// * **语法非法**（其余）→ 400。
///
/// [Unsatisfiable]: RangeError::Unsatisfiable
/// [ZeroSuffix]: RangeError::ZeroSuffix
/// [MultipleRanges]: RangeError::MultipleRanges
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    /// 请求头为空串
    Empty,
    /// 既没有 `=` 也没有单位（例如 `bytes`）
    MissingUnit,
    /// 单位不是 bytes（例如 `items=0-1`）
    UnknownUnit,
    /// `bytes=` 后面什么都没有
    EmptySpec,
    /// 语法错误：非数字、多余的分隔符、数值溢出等
    Syntax,
    /// 多段区间（`bytes=0-99,200-299`）—— 本步明确拒绝，理由见模块文档
    MultipleRanges,
    /// 起点大于终点（`bytes=5-3`）
    StartGreaterThanEnd,
    /// 后缀长度为 0（`bytes=-0`）—— RFC 9110 规定不满足任何字节
    ZeroSuffix,
    /// 区间起点已 >= 文件长度，完全无法满足
    Unsatisfiable,
}

impl RangeError {
    /// 是否应映射为 **416 Range Not Satisfiable**（而不是 400）。
    pub fn is_unsatisfiable(self) -> bool {
        matches!(
            self,
            RangeError::Unsatisfiable | RangeError::ZeroSuffix | RangeError::MultipleRanges
        )
    }
}

impl std::fmt::Display for RangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RangeError::Empty => write!(f, "Range 请求头为空"),
            RangeError::MissingUnit => write!(f, "缺少范围单位，应为 bytes="),
            RangeError::UnknownUnit => write!(f, "不支持的范围单位，本接口只支持 bytes"),
            RangeError::EmptySpec => write!(f, "bytes= 后面没有区间定义"),
            RangeError::Syntax => write!(f, "区间语法不正确"),
            RangeError::MultipleRanges => {
                write!(f, "不支持多段区间（未实现 multipart/byteranges）")
            }
            RangeError::StartGreaterThanEnd => write!(f, "区间起点大于终点"),
            RangeError::ZeroSuffix => write!(f, "后缀长度为 0，不满足任何字节"),
            RangeError::Unsatisfiable => write!(f, "区间起点已超出文件长度"),
        }
    }
}

impl std::error::Error for RangeError {}

/// 解析 `Range` 请求头，返回**闭区间**。
///
/// `file_len` 是文件总字节数，用于解释后缀形态、截断超出文件尾的终点、判断
/// unsatisfiable。纯函数：不碰 IO、不 panic、任何输入都返回 `Result`。
///
/// 调用方只在**存在** Range 请求头时调用它；「没有 Range」= 200 全量，由调用方判断。
///
/// 单位 `bytes` 按 RFC 9110 大小写不敏感（`BYTES=` 也接受），首尾空白会被裁掉；
/// 但数字部分必须是纯 ASCII 数字（1*DIGIT），`+1` / ` 1` 一律算语法错误。
pub fn parse_range(header: &str, file_len: u64) -> Result<ByteRange, RangeError> {
    let text = header.trim();
    if text.is_empty() {
        return Err(RangeError::Empty);
    }

    let (unit, spec) = match text.split_once('=') {
        Some(parts) => parts,
        None => return Err(RangeError::MissingUnit),
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return Err(RangeError::UnknownUnit);
    }
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(RangeError::EmptySpec);
    }

    // 多段列表一律拒绝（逗号出现即判定），理由见模块文档。
    if spec.contains(',') {
        return Err(RangeError::MultipleRanges);
    }

    let (first, last) = match spec.split_once('-') {
        Some(parts) => parts,
        // 没有 '-'：bytes=abc
        None => return Err(RangeError::Syntax),
    };

    // 形态三：bytes=-N —— 后缀长度，取最后 N 个字节。
    // 注意 first 为空不代表「负数」：HTTP 里这就是后缀形态。
    if first.is_empty() {
        if last.is_empty() {
            // bytes=- 既没有起点也没有终点
            return Err(RangeError::Syntax);
        }
        let suffix = parse_digits(last)?;
        if suffix == 0 {
            // RFC 9110：suffix-length 为 0 不满足任何字节
            return Err(RangeError::ZeroSuffix);
        }
        if file_len == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        // 后缀长度大于等于文件长度时取整个文件（RFC 语义），用 saturating_sub 保证不溢出
        return Ok(ByteRange {
            start: file_len.saturating_sub(suffix),
            end: file_len - 1,
        });
    }

    let start = parse_digits(first)?;

    // 形态二：bytes=N- —— 从 N 一直读到文件尾。
    if last.is_empty() {
        if start >= file_len {
            return Err(RangeError::Unsatisfiable);
        }
        return Ok(ByteRange {
            start,
            end: file_len - 1,
        });
    }

    // 形态一：bytes=N-M。
    let end = parse_digits(last)?;
    if start > end {
        return Err(RangeError::StartGreaterThanEnd);
    }
    if start >= file_len {
        return Err(RangeError::Unsatisfiable);
    }
    // 终点超出文件尾 → 截断到最后一字节（RFC：不是错误）。
    Ok(ByteRange {
        start,
        end: end.min(file_len - 1),
    })
}

/// 按 RFC 的 `1*DIGIT` 严格解析非负整数：只认 ASCII 数字。
///
/// 不用 `str::parse` 直接收下 `+1`（Rust 接受前导 `+`，但 RFC 的 DIGIT 不接受），
/// 顺便把空串与溢出都折叠成 [`RangeError::Syntax`]。
fn parse_digits(text: &str) -> Result<u64, RangeError> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(RangeError::Syntax);
    }
    text.parse::<u64>().map_err(|_| RangeError::Syntax)
}

/// 按扩展名给出音频 MIME；扩展名大小写不敏感，认不出时用 `application/octet-stream`。
///
/// 只认画布点名的三种（mp3 / flac / wav）。其余容器（m4a / ogg 等）眼下没有解码 /
/// 播放需求，宁可给通用的 octet-stream，也不在这里猜一个可能不准的类型。
pub fn content_type_for(file_path: &str) -> &'static str {
    let extension = std::path::Path::new(file_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    if extension.eq_ignore_ascii_case("mp3") {
        "audio/mpeg"
    } else if extension.eq_ignore_ascii_case("flac") {
        "audio/flac"
    } else if extension.eq_ignore_ascii_case("wav") {
        "audio/wav"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─────────────────────── 合法形态 ───────────────────────

    /// bytes=N-M：首尾都在文件内 / 终点超出按 RFC 截断 / 正好整个文件。
    #[test]
    fn parses_first_and_last_byte_pos_form() {
        assert_eq!(
            parse_range("bytes=0-499", 1000).unwrap(),
            ByteRange { start: 0, end: 499 }
        );
        assert_eq!(
            parse_range("bytes=500-999", 1000).unwrap(),
            ByteRange { start: 500, end: 999 }
        );
        // 终点超出文件尾：截断到最后一字节，不是错误
        assert_eq!(
            parse_range("bytes=990-99999", 1000).unwrap(),
            ByteRange { start: 990, end: 999 }
        );
        // 正好整个文件（边界：必须可满足，路由层该给 206 而不是 416）
        assert_eq!(
            parse_range("bytes=0-999", 1000).unwrap(),
            ByteRange { start: 0, end: 999 }
        );
        // 单字节文件
        assert_eq!(
            parse_range("bytes=0-0", 1).unwrap(),
            ByteRange { start: 0, end: 0 }
        );
        // 单位大小写不敏感 + 首尾空白可裁
        assert_eq!(
            parse_range("  BYTES=1-2  ", 10).unwrap(),
            ByteRange { start: 1, end: 2 }
        );
    }

    /// bytes=N-：开放终点，读到文件尾。
    #[test]
    fn parses_open_ended_form() {
        assert_eq!(
            parse_range("bytes=500-", 1000).unwrap(),
            ByteRange { start: 500, end: 999 }
        );
        assert_eq!(
            parse_range("bytes=999-", 1000).unwrap(),
            ByteRange { start: 999, end: 999 }
        );
        // 起点 0 的开放终点 = 整个文件
        assert_eq!(
            parse_range("bytes=0-", 1000).unwrap(),
            ByteRange { start: 0, end: 999 }
        );
    }

    /// bytes=-N：后缀长度，取最后 N 个字节；N 超过文件长度时取整个文件。
    #[test]
    fn parses_suffix_length_form() {
        assert_eq!(
            parse_range("bytes=-500", 1000).unwrap(),
            ByteRange { start: 500, end: 999 }
        );
        assert_eq!(
            parse_range("bytes=-2", 16).unwrap(),
            ByteRange { start: 14, end: 15 }
        );
        // 后缀长度 == 文件长度 → 整个文件
        assert_eq!(
            parse_range("bytes=-1000", 1000).unwrap(),
            ByteRange { start: 0, end: 999 }
        );
        // 后缀长度 > 文件长度 → 整个文件（RFC 语义）
        assert_eq!(
            parse_range("bytes=-99999", 1000).unwrap(),
            ByteRange { start: 0, end: 999 }
        );
    }

    #[test]
    fn byte_len_is_inclusive() {
        assert_eq!(ByteRange { start: 0, end: 0 }.byte_len(), 1);
        assert_eq!(ByteRange { start: 2, end: 5 }.byte_len(), 4);
        assert_eq!(ByteRange { start: 0, end: 999 }.byte_len(), 1000);
    }

    // ─────────────────────── 非法输入穷举 ───────────────────────

    /// 纯函数穷举：所有非法输入都明确报错，绝不 panic。
    #[test]
    fn rejects_invalid_inputs_without_panicking() {
        let cases = [
            "",                       // 空
            "   ",                    // 全空白
            "bytes",                  // 没有 '='
            "bytes=",                 // 没有区间
            "bytes=abc",              // 非数字
            "bytes=5-3",              // 起点大于终点
            "bytes=-0",               // 后缀长度为 0
            "bytes=--5",              // 看起来像负数
            "bytes=-5-3",             // 负数式起点 + 多余分隔符
            "bytes=1--5",             // 负数式终点
            "bytes=-",                // 两端都空
            "bytes=1-2-3",            // 多余 '-'
            "bytes=0-1,2-3",          // 多段（合法语法，但本步拒绝）
            "bytes=,",                // 只有逗号
            "items=0-1",              // 不支持的单位
            "bytes=+1-2",             // RFC 的 DIGIT 不接受前导 '+'
            "bytes=0 - 5",            // 数字里混空白
            "bytes=1- 2",             // 终点前有空白
            "bytes=99999999999999999999-", // u64 溢出
            "bytes=-99999999999999999999", // 后缀溢出
        ];
        for case in cases {
            let result = parse_range(case, 1000);
            assert!(
                result.is_err(),
                "{case:?} 必须明确报错，实际得到 {result:?}"
            );
        }
    }

    /// 非法输入的错误分类：哪一类该 400，哪一类该 416。
    #[test]
    fn classifies_errors_for_status_mapping() {
        assert_eq!(parse_range("", 1000), Err(RangeError::Empty));
        assert_eq!(parse_range("bytes", 1000), Err(RangeError::MissingUnit));
        assert_eq!(parse_range("bytes=", 1000), Err(RangeError::EmptySpec));
        assert_eq!(parse_range("items=0-1", 1000), Err(RangeError::UnknownUnit));
        assert_eq!(parse_range("bytes=abc", 1000), Err(RangeError::Syntax));
        assert_eq!(
            parse_range("bytes=5-3", 1000),
            Err(RangeError::StartGreaterThanEnd)
        );
        assert_eq!(parse_range("bytes=-0", 1000), Err(RangeError::ZeroSuffix));
        assert_eq!(
            parse_range("bytes=0-1,2-3", 1000),
            Err(RangeError::MultipleRanges)
        );

        assert!(RangeError::Unsatisfiable.is_unsatisfiable());
        assert!(RangeError::ZeroSuffix.is_unsatisfiable());
        assert!(RangeError::MultipleRanges.is_unsatisfiable());
        assert!(!RangeError::Syntax.is_unsatisfiable());
        assert!(!RangeError::StartGreaterThanEnd.is_unsatisfiable());
        assert!(!RangeError::Empty.is_unsatisfiable());
    }

    /// 越界（起点 >= 文件长度）→ Unsatisfiable；空文件上任何区间都不可满足。
    #[test]
    fn rejects_unsatisfiable_ranges() {
        for case in ["bytes=1000-", "bytes=1000-2000", "bytes=5000-6000"] {
            assert_eq!(
                parse_range(case, 1000),
                Err(RangeError::Unsatisfiable),
                "{case}"
            );
        }
        for case in ["bytes=0-", "bytes=0-0", "bytes=-1"] {
            assert_eq!(
                parse_range(case, 0),
                Err(RangeError::Unsatisfiable),
                "空文件上的 {case} 必须不可满足"
            );
        }
    }

    /// 每个错误都有一句中文说明，且不 panic。
    #[test]
    fn every_error_has_a_chinese_message() {
        let all = [
            RangeError::Empty,
            RangeError::MissingUnit,
            RangeError::UnknownUnit,
            RangeError::EmptySpec,
            RangeError::Syntax,
            RangeError::MultipleRanges,
            RangeError::StartGreaterThanEnd,
            RangeError::ZeroSuffix,
            RangeError::Unsatisfiable,
        ];
        for error in all {
            let text = error.to_string();
            assert!(
                text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{error:?} 的文案必须是中文：{text}"
            );
        }
    }

    // ─────────────────────── Content-Type ───────────────────────

    #[test]
    fn content_type_follows_extension_case_insensitively() {
        assert_eq!(content_type_for("/music/a.mp3"), "audio/mpeg");
        assert_eq!(content_type_for("/music/a.MP3"), "audio/mpeg");
        assert_eq!(content_type_for("/music/a.flac"), "audio/flac");
        assert_eq!(content_type_for("/music/a.FLAC"), "audio/flac");
        assert_eq!(content_type_for("/music/a.wav"), "audio/wav");
        assert_eq!(content_type_for("/music/a.Wav"), "audio/wav");
        // 认不出的扩展名 / 没有扩展名 → 通用二进制
        assert_eq!(content_type_for("/music/a.ogg"), "application/octet-stream");
        assert_eq!(content_type_for("/music/a.m4a"), "application/octet-stream");
        assert_eq!(content_type_for("/music/no-extension"), "application/octet-stream");
    }
}
