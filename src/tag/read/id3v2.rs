//! ID3v2.2/2.3/2.4 帧解析 —— 移植自 src/tag/read/id3v2.ts（零依赖）
//!
//! 迁移纪律：**逐分支照搬 TS 的宽容行为**，不做"顺手修正"。
//! 已知偏差必须保留到差分基准建立之后再议（详见 docs/MIGRATION.md 陷阱清单）。
use super::metadata::{Picture, RawFrame};

/// 7-bit syncsafe 读取。TS 版无边界检查（越界读到 undefined→0），此处等价地用 get_or(0)。
pub fn read_sync_safe(buf: &[u8], pos: usize, n: usize) -> i64 {
    let mut v: i64 = 0;
    for i in 0..n { v = (v << 7) | (buf.get(pos + i).copied().unwrap_or(0) & 0x7f) as i64; }
    v
}

/// 7-bit syncsafe 写入（固定 4 字节）
pub fn write_sync_safe(v: i64) -> [u8; 4] {
    let mut b = [0u8; 4];
    let mut val = v as u32;
    for i in (0..4).rev() { b[i] = (val & 0x7f) as u8; val >>= 7; }
    b
}

const V22_MAP: &[(&str, &str)] = &[
    ("TT2", "TIT2"), ("TP1", "TPE1"), ("TP2", "TPE2"), ("TP3", "TPE3"), ("TP4", "TPE4"),
    ("TAL", "TALB"), ("TCO", "TCON"), ("TRK", "TRCK"), ("TPA", "TPOS"), ("TYE", "TYER"),
    ("TCM", "TCOM"), ("TXT", "TEXT"), ("TLA", "TLAN"), ("TBP", "TBPM"), ("TCP", "TCMP"),
    ("TOR", "TDOR"), ("PIC", "APIC"), ("ULT", "USLT"), ("COM", "COMM"), ("TXX", "TXXX"),
    ("WXX", "WXXX"), ("TDA", "TDRC"),
];
fn map_v22(id: &str) -> String {
    V22_MAP.iter().find(|(k, _)| *k == id).map(|(_, v)| v.to_string()).unwrap_or_else(|| id.to_string())
}

/// 去除反同步字节。
/// ⚠️ 与 TS `unsync()`（id3v2.ts:28-40）保持**逐字节一致**，包括审计发现的那处与规范不符的
/// 跳过逻辑（FF 后跟 E0-FF 时多跳一字节）。修它会改变既有文件的解析结果 → 属破坏性变更，
/// 必须先有差分基准。见 docs/MIGRATION.md「陷阱 1」。
pub fn unsync(buf: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(buf.len());
    let mut i = 0usize;
    while i < buf.len() {
        out.push(buf[i]);
        if buf[i] == 0xff && i + 1 < buf.len() && (buf[i + 1] & 0xe0) == 0xe0 {
            i += 1;
            if i < buf.len() && buf[i] == 0x00 { i += 1; }
            if i < buf.len() && buf[i] == 0xff { i += 1; }
        }
        i += 1;
    }
    out
}

/// UTF-16 解码（带 BOM 或指定字节序）。
/// 奇数长度截尾为偶数——**绝不 panic**（round3 §2.1 的核心要求：畸形帧不得炸掉整文件读取）。
pub fn decode_utf16(buf: &[u8], big_endian: bool) -> String {
    let even_len = buf.len() & !1;
    let s = &buf[..even_len];
    // BE：把字节对调成 LE 形态，后续统一按 LE 处理
    let b: Vec<u8> = if big_endian {
        s.chunks_exact(2).flat_map(|c| [c[1], c[0]]).collect()
    } else { s.to_vec() };

    let mut start = 0usize;
    while start + 1 < b.len() && b[start] == 0x00 && b[start + 1] == 0x00 { start += 2; }
    let mut le_from = start;
    if start + 1 < b.len() {
        if b[start] == 0xff && b[start + 1] == 0xfe { le_from = start + 2; }        // LE BOM
        else if b[start] == 0xfe && b[start + 1] == 0xff {                          // BE BOM
            // TS: Buffer.from(b).swap16().toString('utf16le', start) —— 再次整体换序后从 start 起
            let swapped: Vec<u8> = b.chunks_exact(2).flat_map(|c| [c[1], c[0]]).collect();
            return utf16le_from(&swapped, start);
        }
    }
    utf16le_from(&b, le_from)
}

fn utf16le_from(b: &[u8], from: usize) -> String {
    let tail = &b[from.min(b.len())..];
    let units: Vec<u16> = tail.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

/// Latin1（ISO-8859-1）解码：逐字节无损映射到 U+0000..U+00FF，等价 Buffer.toString('latin1')
pub fn latin1_decode(b: &[u8]) -> String { b.iter().map(|&c| c as u16).collect::<Vec<u16>>().iter().map(|&u| char::from_u32(u as u32).unwrap_or('\u{fffd}')).collect() }

/// Latin1 编码；超出 0..=255 的字符按 JS Buffer 行为静默丢高位（& 0xff）
pub fn latin1_encode(s: &str) -> Vec<u8> { s.chars().map(|c| (c as u32 & 0xff) as u8).collect() }

/// UTF-8 lossy 解码，等价 Node Buffer.toString('utf8')（非法序列 → U+FFFD）
pub fn utf8_lossy(b: &[u8]) -> String { String::from_utf8_lossy(b).into_owned() }

/// 文本帧解码（首字节是编码标志）。未知 enc 回落 latin1（TS 的 default 分支）。
pub fn decode_text(data: &[u8]) -> String {
    if data.is_empty() { return String::new() }
    let enc = data[0];
    let body = &data[1..];
    let s = match enc {
        0 => latin1_decode(body),
        1 => decode_utf16(body, false),
        2 => decode_utf16(body, true),
        3 => utf8_lossy(body),
        _ => latin1_decode(body),
    };
    trim_trailing_nuls(&s)
}

pub fn trim_trailing_nuls(s: &str) -> String { s.trim_end_matches(['\u{0}']).to_string() }

/// APIC 帧解码（结构见 TS :76-105，含 UTF-16 描述的双终止符处理）
pub fn decode_apic(data: &[u8]) -> Option<Picture> {
    if data.len() < 5 { return None }
    let enc = data[0];
    let p = 1usize;
    let mut mime_end = p;
    while mime_end < data.len() && data[mime_end] != 0 { mime_end += 1 }
    if mime_end >= data.len() { return None }
    let mime_type = latin1_decode(&data[p..mime_end]);
    let pic_type = data.get(mime_end + 1).copied().unwrap_or(0);
    let p = mime_end + 2;
    let utf16 = enc == 1 || enc == 2;

    let mut desc_end = p;
    if utf16 {
        while desc_end + 1 < data.len() && !(data[desc_end] == 0 && data[desc_end + 1] == 0) { desc_end += 2 }
        while desc_end < data.len() && matches!(data[desc_end], 0x00 | 0xff | 0xfe) { desc_end += 1 }
        desc_end = desc_end.min(data.len());
    } else {
        while desc_end < data.len() && data[desc_end] != 0 { desc_end += 1 }
    }
    let description = if utf16 { decode_utf16(&data[p..desc_end], enc == 2) } else { latin1_decode(&data[p..desc_end]) };

    let mut image_start = desc_end;
    if utf16 {
        while image_start + 1 < data.len() && data[image_start] == 0 && data[image_start + 1] == 0 { image_start += 2 }
    } else if data.get(image_start).copied() == Some(0) { image_start += 1 }
    image_start = image_start.min(data.len());
    let image_data = data[image_start..].to_vec();
    Some(Picture {
        mime_type: if mime_type.is_empty() { "image/unknown".into() } else { mime_type },
        pic_type, description, data: image_data,
    })
}

#[derive(Debug, Clone)]
pub struct LyricsFrame { pub language: String, pub description: String, pub text: String }

/// USLT / COMM 通用解码：enc(1) + lang(3) + desc(NUL 结尾) + text
pub fn decode_uslt(data: &[u8]) -> Option<LyricsFrame> {
    if data.len() < 5 { return None }
    let enc = data[0];
    let language = latin1_decode(&data[1..4]);
    let utf16 = enc == 1 || enc == 2;
    let p = 4usize;
    let mut desc_end = p;
    if utf16 { while desc_end + 1 < data.len() && !(data[desc_end] == 0 && data[desc_end + 1] == 0) { desc_end += 2 } }
    else { while desc_end < data.len() && data[desc_end] != 0 { desc_end += 1 } }
    let description = if utf16 { decode_utf16(&data[p..desc_end], enc == 2) } else { latin1_decode(&data[p..desc_end]) };
    let mut text_start = desc_end;
    if utf16 { while text_start + 1 < data.len() && data[text_start] == 0 && data[text_start + 1] == 0 { text_start += 2 } }
    else if data.get(text_start).copied() == Some(0) { text_start += 1 }
    let text_buf = &data[text_start.min(data.len())..];
    let text = if utf16 { decode_utf16(text_buf, enc == 2) } else if enc == 3 { utf8_lossy(text_buf) } else { latin1_decode(text_buf) };
    Some(LyricsFrame { language, description, text })
}

#[derive(Debug, Clone)]
pub struct TxxxFrame { pub key: String, pub value: String }

/// TXXX 键值解码：enc(1) + key(NUL 结尾) + value
pub fn decode_txxx(data: &[u8]) -> Option<TxxxFrame> {
    if data.len() < 2 { return None }
    let enc = data[0];
    let utf16 = enc == 1 || enc == 2;
    let p = 1usize;
    let mut key_end = p;
    if utf16 { while key_end + 1 < data.len() && !(data[key_end] == 0 && data[key_end + 1] == 0) { key_end += 2 } }
    else { while key_end < data.len() && data[key_end] != 0 { key_end += 1 } }
    let key = if utf16 { decode_utf16(&data[p..key_end], enc == 2) } else { latin1_decode(&data[p..key_end]) };
    let mut v_start = key_end;
    if utf16 { while v_start + 1 < data.len() && data[v_start] == 0 && data[v_start + 1] == 0 { v_start += 2 } }
    else if data.get(v_start).copied() == Some(0) { v_start += 1 }
    let v_buf = &data[v_start.min(data.len())..];
    let value = if utf16 { decode_utf16(v_buf, enc == 2) } else if enc == 3 { utf8_lossy(v_buf) } else { latin1_decode(v_buf) };
    Some(TxxxFrame { key, value })
}

#[derive(Debug, Clone)]
pub struct Id3v2Header {
    pub version: u8, pub revision: u8, pub flags: u8,
    pub tag_size: i64, pub frames: Vec<RawFrame>, pub padding: usize,
}

pub fn be_u32_at(buf: &[u8], at: usize) -> u32 {
    buf.get(at..at + 4).and_then(|s| TryInto::<[u8; 4]>::try_into(s).ok()).map(u32::from_be_bytes).unwrap_or(0)
}

/// 小端 u32（RIFF / APE 大量使用）
pub fn le_u32_at(buf: &[u8], at: usize) -> u32 {
    buf.get(at..at + 4).and_then(|s| TryInto::<[u8; 4]>::try_into(s).ok()).map(u32::from_le_bytes).unwrap_or(0)
}

#[allow(dead_code)]
fn be_u32(buf: &[u8], at: usize) -> u32 {
    buf.get(at..at + 4).and_then(|s| TryInto::<[u8; 4]>::try_into(s).ok()).map(u32::from_be_bytes).unwrap_or(0)
}
fn is_frame_id(s: &str) -> bool { !s.is_empty() && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) }

/// 解析整个 ID3v2 tag（含扩展头跳过）。返回 None = 该位置没有合法 ID3v2。
pub fn parse_id3v2(buf: &[u8], start: usize) -> Option<Id3v2Header> {
    if buf.len() < start + 10 { return None }
    if &buf[start..start + 3] != b"ID3" { return None }
    let (version, revision, flags) = (buf[start + 3], buf[start + 4], buf[start + 5]);
    let tag_size = read_sync_safe(buf, start + 6, 4);
    if tag_size < 0 || start + 10 + tag_size as usize > buf.len() { return None }

    let mut p = start + 10;
    let end = start + 10 + tag_size as usize;

    if flags & 0x40 != 0 {
        if version == 4 && p + 4 <= end {
            let ext = read_sync_safe(buf, p, 4);
            if ext > 0 && p + ext as usize <= end { p += ext as usize }
        } else if version == 3 && p + 4 <= end {
            let ext = be_u32(buf, p);
            if ext > 0 && p + 4 + ext as usize <= end { p += 4 + ext as usize }
        } else if version == 2 && p + 4 <= end {
            let ext = be_u32(buf, p);
            if ext > 0 && p + 6 + ext as usize <= end { p += 6 + ext as usize }
        } else {
            return Some(Id3v2Header { version, revision, flags, tag_size, frames: vec![], padding: 0 });
        }
    }

    let mut frames: Vec<RawFrame> = Vec::new();
    let header_size = if version == 2 { 6 } else { 10 };
    let must_unsync = version == 3 && (flags & 0x80) != 0;

    while p + header_size <= end {
        if buf[p] == 0 { break }
        let raw_id = latin1_decode(&buf[p..p + if version == 2 { 3 } else { 4 }]);
        let frame_id = if version == 2 { map_v22(&raw_id) } else { raw_id.clone() };
        if !is_frame_id(&frame_id) { break }

        let size: i64;
        let mut frame_flags: u16 = 0;
        if version == 2 { size = read_sync_safe(buf, p + 3, 3) }
        else if version == 4 { size = read_sync_safe(buf, p + 4, 4); frame_flags = ((buf[p + 8] as u16) << 8) | buf[p + 9] as u16 }
        else { size = be_u32(buf, p + 4) as i64; frame_flags = ((buf[p + 8] as u16) << 8) | buf[p + 9] as u16 }

        if size < 0 || p + header_size + size as usize > end { p += header_size; continue }
        let mut data = buf[p + header_size..p + header_size + size as usize].to_vec();
        let fu23 = version == 3 && (frame_flags & 0x0008) != 0;
        let fu24 = version == 4 && (frame_flags & 0x0002) != 0;
        if must_unsync || fu23 || fu24 {
            data = unsync(&data);
            if version == 4 && (frame_flags & 0x0001) != 0 && data.len() >= 4 { data = data[4..].to_vec() }
        }
        frames.push(RawFrame { frame_id, size: size as usize, data });
        p += header_size + size as usize;
    }

    let mut padding = 0usize;
    while p < end && buf[p] == 0 { padding += 1; p += 1 }
    Some(Id3v2Header { version, revision, flags, tag_size, frames, padding })
}

/// TRCK/TPOS 的 "N" 或 "N/T"。非数字一律 null（TS :236-247 同款严格度）。
#[derive(Debug, Clone, Copy, Default)]
pub struct NumberPair { pub num: Option<i64>, pub total: Option<i64> }
pub fn parse_number_pair(text: &str) -> NumberPair {
    let raw = text.trim();
    if raw.is_empty() { return NumberPair::default() }
    let mut it = raw.split('/').map(str::trim);
    let a = it.next(); let b = it.next();
    let parse = |s: Option<&str>| -> Option<i64> {
        let s = s?;
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) { return None }
        match s.parse::<i64>() { Ok(n) if (1..=255).contains(&n) => Some(n), _ => None }
    };
    let mut total = parse(b);
    if total == Some(0) { total = None } // 0 表示未知
    NumberPair { num: parse(a), total }
}

/// 流派解析：支持纯文本与 (n) 数字代码（ID3v1 表）
pub fn parse_genre(text: &str) -> String {
    let t = text.trim();
    if t.len() > 2 && t.starts_with('(') && t.ends_with(')') {
        let inner = &t[1..t.len() - 1];
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(idx) = inner.parse::<usize>() {
                let g = super::id3v1::id3v1_genres();
                if idx < g.len() { return g[idx].to_string() }
            }
        }
    }
    t.to_string()
}
