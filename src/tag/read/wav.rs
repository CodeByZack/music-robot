//! RIFF/WAV chunk 树解析（LIST INFO + 内嵌 ID3）—— 移植自 src/tag/read/wav.ts
use super::id3v2::{latin1_decode, le_u32_at, parse_id3v2, utf8_lossy, Id3v2Header};

#[derive(Debug, Clone)]
pub struct RiffChunk { pub id: String, pub size: u32, pub data: Vec<u8>, pub offset: usize }

/// 遍历一层子 chunk。⚠️ size 为奇数时实占 size+1 字节（RIFF 偶对齐填充），
/// 漏掉这个 pad 会让后续所有 chunk 的 id 错位一格 —— 整棵树读崩。
pub fn parse_riff_chunks(buf: &[u8], start: usize, end: usize) -> Vec<RiffChunk> {
    let mut out = Vec::new();
    let mut p = start;
    while p + 8 <= end {
        if p + 4 > buf.len() { break }
        let id = latin1_decode(&buf[p..p + 4]);
        let size = le_u32_at(buf, p + 4);
        let size_us = size as usize;
        if p + 8 + size_us > end || p + 8 + size_us > buf.len() { break }
        out.push(RiffChunk { id, size, data: buf[p + 8..p + 8 + size_us].to_vec(), offset: p });
        p += 8 + size_us + (size_us & 1);
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct WavInfo {
    pub chunks: Vec<RiffChunk>,
    /// LIST INFO 子项（INAM/IART/IPRD/…）
    pub info: Vec<(String, String)>,
    pub id3_frames: Option<Id3v2Header>,
    pub audio_data_offset: usize,
    pub data_len: u32,
    pub sample_rate: Option<u32>,
    pub bits_per_sample: Option<u32>,
    pub channels: Option<u32>,
    pub byte_rate: Option<u32>,
}
impl WavInfo { pub fn get(&self, k: &str) -> Option<&str> { self.info.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str()) } }

/// 解析 WAV：顶层 RIFF/WAVE + fmt / data / LIST INFO / id3
pub fn parse_wav(buf: &[u8]) -> Option<WavInfo> {
    if buf.len() < 12 { return None }
    if &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" { return None }
    let riff_size = le_u32_at(buf, 4) as usize;
    let end = (8 + riff_size).min(buf.len());
    let chunks = parse_riff_chunks(buf, 12, end);
    let mut w = WavInfo { chunks: chunks.clone(), ..Default::default() };

    for c in &chunks {
        match c.id.as_str() {
            "fmt " => if c.data.len() >= 16 {
                w.bits_per_sample = Some(u16::from_le_bytes([c.data[14], c.data[15]]) as u32);
                w.sample_rate = Some(le_u32_at(&c.data, 4));
                w.channels = Some(u16::from_le_bytes([c.data[2], c.data[3]]) as u32);
                w.byte_rate = Some(le_u32_at(&c.data, 8));
            },
            "data" => { w.audio_data_offset = c.offset + 8; w.data_len = c.size }
            "LIST" => if c.data.len() >= 4 && &c.data[0..4] == b"INFO" {
                for sub in parse_riff_chunks(&c.data, 4, c.data.len()) {
                    let text = utf8_lossy(&sub.data).trim_end_matches('\u{0}').to_string();
                    if !text.is_empty() { w.info.push((sub.id, text)) }
                }
            },
            "id3 " => { w.id3_frames = parse_id3v2(&c.data, 0) }
            _ => {}
        }
    }
    Some(w)
}
