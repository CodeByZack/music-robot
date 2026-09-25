//! Latin1 字节串 → GB18030 乱码嗅探 —— 移植自 src/tag/read/gbk-sniff.ts
//!
//! 依赖说明：TS 侧用 Node/V8 内置 `TextDecoder('gb18030')`；Rust std 只有 UTF-8/UTF-16，
//! 故此处用 encoding_rs（Firefox 维护、纯 Rust）作**平台能力补位**。标签解析逻辑仍全部自研。
use encoding_rs::GB18030;

#[derive(Debug, Clone)]
pub struct GbkSniffResult {
    pub latin1: String,
    /// GB18030 解码结果。None = 解码器明确失败（等价 TS 的 catch → null）。
    pub gbk: Option<String>,
    pub looks_gbk: bool,
    pub high_byte_ratio: f64,
    /// 解码过程是否产生过替换字符 U+FFFD。**encoding_rs 独有信号**，
    /// TS 侧拿不到——可用于加强判据（见下方 looksGbk 说明与 MIGRATION.md 陷阱2）。
    pub had_replacements: bool,
}

/// 统计高字节(>=0x80)占比 —— 等价 TS 对 latin1 字符串逐 charCode 判断
pub fn high_byte_ratio(buf: &[u8]) -> f64 {
    if buf.is_empty() { return 0.0 }
    buf.iter().filter(|b| **b >= 0x80).count() as f64 / buf.len() as f64
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{4e00}'..='\u{9fff}' | '\u{3000}'..='\u{303f}' | '\u{ff00}'..='\u{ffef}')
}

/// 嗅探：高字节占比 > 0.3 **且** GB18030 解码后含 CJK 字符 → 判定为 GBK 乱码。
/// 与 TS gbkSniff 同构（两条判据都在，不再有此前"缺解码器所以判据偏松"的偏差）。
pub fn gbk_sniff(input: &[u8]) -> GbkSniffResult {
    let latin1 = super::id3v2::latin1_decode(input);
    let ratio = high_byte_ratio(input);

    // encoding_rs 的 decode 是 lossy（fatal=false），与 TextDecoder 默认行为一致；
    // 它不会返回 Err，只会以 had_replacements=true 标记"有字节无法映射"。
    let (cow, _enc, had_replacements) = GB18030.decode(input);
    let gbk = Some(cow.into_owned());

    let mut looks_gbk = false;
    if let Some(s) = &gbk {
        if ratio > 0.3 && s.chars().any(is_cjk) { looks_gbk = true }
    }
    GbkSniffResult { latin1, gbk, looks_gbk, high_byte_ratio: ratio, had_replacements }
}

/// 若判定为 GBK 则返回解码文本，否则返回 latin1（两者都去尾部 NUL）。等价 TS decodeMaybeGbk。
pub fn decode_maybe_gbk(input: &[u8], sniff: Option<&GbkSniffResult>) -> String {
    let s = match sniff { Some(x) => x.clone(), None => gbk_sniff(input) };
    let chosen = match (&s.gbk, s.looks_gbk) { (Some(g), true) => g.clone(), _ => s.latin1.clone() };
    chosen.trim_end_matches('\u{0}').to_string()
}
