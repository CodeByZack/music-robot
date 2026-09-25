//! FLAC 元数据块 / Vorbis Comment / STREAMINFO —— 移植自 src/tag/read/flac.ts
use super::id3v2::utf8_lossy;

#[derive(Debug, Clone)]
pub struct FlacBlock { pub ty: u8, pub is_last: bool, pub payload: Vec<u8> }

/// 遍历 METADATA_BLOCK（type+last-flag 4 字节头 + 24bit 长度）
pub fn parse_flac_metadata(buf: &[u8]) -> Option<Vec<FlacBlock>> {
    if buf.len() < 4 || &buf[0..4] != b"fLaC" { return None }
    let mut p = 4usize;
    let mut out = Vec::new();
    loop {
        if p + 4 > buf.len() { return None }
        let b0 = buf[p];
        let is_last = b0 & 0x80 != 0;
        let ty = b0 & 0x7f;
        let len = ((buf[p + 1] as usize) << 16) | ((buf[p + 2] as usize) << 8) | buf[p + 3] as usize;
        p += 4;
        if p + len > buf.len() { return None }
        out.push(FlacBlock { ty, is_last, payload: buf[p..p + len].to_vec() });
        p += len;
        if is_last { break }
    }
    Some(out)
}

/// STREAMINFO 位域（跨字节，**大端**）。逐行对照 TS parseStreamInfo 移植：
///   sr = (b10<<12)|(b11<<4)|(b12>>4)                      // 20 bit
///   channels = ((b12>>1)&7)+1                              // 3 bit
///   bits = (((b12&1)<<4)|(b13>>4))+1                        // 5 bit ← b12 的最低位是进位！
///   totalSamples = ((b13&0x0f)<<32)|b14<<24|b15<<16|b16<<8|b17
pub fn parse_stream_info(payload: &[u8]) -> (u32, u32, u64) {
    if payload.len() < 18 { return (0, 0, 0) }
    // md5 在 payload[18..34]，由 extract_stream_info_md5 单独提供
    let b = |i: usize| payload[i] as u32;
    let sample_rate = (b(10) << 12) | (b(11) << 4) | (b(12) >> 4);
    let _channels = ((b(12) >> 1) & 0x07) + 1;
    let bits = (((b(12) & 1) << 4) | (b(13) >> 4)) + 1;
    let total = (((payload[13] & 0x0f) as u64) << 32)
        | ((payload[14] as u64) << 24) | ((payload[15] as u64) << 16)
        | ((payload[16] as u64) << 8) | payload[17] as u64;
    (sample_rate, bits, total)
}

#[derive(Debug, Clone)]
pub struct VorbisPair { pub key: String, pub value: String }

/// Vendor + comments；key/value 均为 UTF-8。守卫与 TS 一致（p+4 > length 而非 p > length）。
pub fn parse_vorbis_comment(payload: &[u8]) -> (Vec<VorbisPair>, String) {
    let mut pairs = Vec::new();
    if payload.len() < 4 { return (pairs, String::new()) }
    let vend_len = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
    if 4 + vend_len > payload.len() { return (pairs, String::new()) }
    let vendor = utf8_lossy(&payload[4..4 + vend_len]);
    let mut p = 4 + vend_len;
    if p + 4 > payload.len() { return (pairs, vendor) }
    let count = u32::from_le_bytes(payload[p..p + 4].try_into().unwrap()) as usize;
    p += 4;
    for _ in 0..count {
        if p + 4 > payload.len() { break }
        let len = u32::from_le_bytes(payload[p..p + 4].try_into().unwrap()) as usize;
        p += 4;
        if p + len > payload.len() { break }
        let raw = utf8_lossy(&payload[p..p + len]);
        p += len;
        if let Some(eq) = raw.find('=') {
            pairs.push(VorbisPair { key: raw[..eq].to_string(), value: raw[eq + 1..].to_string() });
        }
    }
    (pairs, vendor)
}

/// FLAC PICTURE 块解码（type 6）。字段全部 **大端 u32**（FLAC 规范），
/// mime/description 是变长，data 取剩余全部字节。
pub fn flac_picture(payload: &[u8]) -> Option<super::metadata::Picture> {
    let be32 = |q: usize| -> u64 { (payload[q] as u64) << 24 | (payload[q + 1] as u64) << 16 | (payload[q + 2] as u64) << 8 | payload[q + 3] as u64 };
    if payload.len() < 8 { return None }
    let mut q = 0usize;
    let pic_type = be32(q) as u8; q += 4;
    let mime_len = be32(q) as usize; q += 4;
    if q + mime_len > payload.len() { return None }
    let mime_type = super::id3v2::latin1_decode(&payload[q..q + mime_len]); q += mime_len;
    if q + 4 > payload.len() { return None }
    let desc_len = be32(q) as usize; q += 4;
    if q + desc_len > payload.len() { return None }
    let description = utf8_lossy(&payload[q..q + desc_len]); q += desc_len;
    // width(4) height(4) depth(4) colors(4)，本层不使用
    if q + 16 > payload.len() { return None }
    q += 16;
    Some(super::metadata::Picture {
        mime_type: if mime_type.is_empty() { "image/unknown".into() } else { mime_type },
        pic_type, description, data: payload[q..].to_vec(),
    })
}

/// 音频区起点：最后一个 metadata block 之后
pub fn flac_audio_start(buf: &[u8]) -> usize {
    let mut p = 4usize;
    while p + 4 <= buf.len() {
        let b0 = buf[p];
        let len = ((buf[p + 1] as usize) << 16) | ((buf[p + 2] as usize) << 8) | buf[p + 3] as usize;
        let last = b0 & 0x80 != 0;
        p += 4 + len;
        if last { break }
    }
    p.min(buf.len())
}

/// STREAMINFO 里存的原音频 MD5（hex）。FLAC 校验用：它描述的是**音频**，
/// 因此不随元数据块变化而变 —— 两次读出来不一致就说明音频被动过。
pub fn extract_stream_info_md5(buf: &[u8]) -> String {
    let blocks = match parse_flac_metadata(buf) { Some(b) => b, None => return String::new() };
    match blocks.iter().find(|b| b.ty == 0) {
        Some(si) if si.payload.len() >= 34 => si.payload[18..34].iter().map(|x| format!("{x:02x}")).collect(),
        _ => String::new(),
    }
}
