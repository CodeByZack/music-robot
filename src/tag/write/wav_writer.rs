//! WAV 局部编辑（LIST INFO 子项 / 内嵌 ID3v2）—— 移植自 src/tag/write/wav-writer.ts
//!
//! 语义：未点名的 INFO 子项与 id3 帧原样保留；点名键替换/删除；blank_all 全清。
use std::path::Path;

use super::super::read::wav::{parse_riff_chunks, parse_wav, RiffChunk};
use super::super::read::{latin1_encode, parse_id3v2, write_sync_safe};
use super::atomic::atomic_replace;
use super::id3v2_editor::{edit_id3v2_frames, Id3EditMeta};
use super::mp3_writer::WriteError;

fn u32le(v: u32) -> [u8; 4] { v.to_le_bytes() }

/// chunk 包装：4B id + size + data +（奇数长度补 1 字节 pad）
fn chunk(id: &str, data: &[u8]) -> Vec<u8> {
    let mut out = latin1_encode(id);
    out.extend_from_slice(&u32le(data.len() as u32));
    out.extend_from_slice(data);
    if data.len() & 1 == 1 { out.push(0) }
    out
}

fn info_field_of(id: &str) -> Option<&'static str> {
    Some(match id {
        "INAM" => "title", "IART" => "artists", "IPRD" => "albums", "ICRD" => "year",
        "ITRK" => "track", "IGNR" => "genres", "ICMT" => "comment",
        _ => return None, // 未知子项：原样保留
    })
}

fn is_touched(m: &Id3EditMeta, field: &str) -> bool {
    if m.unset_fields.iter().any(|x| x == field) { return true }
    match field {
        "title" => m.title.is_some(), "artists" => m.artists.is_some(), "albums" => m.albums.is_some(),
        "year" => m.year.is_some(), "track" => m.track.is_some(), "genres" => m.genres.is_some(),
        "comment" => m.comment.is_some(), _ => false,
    }
}

/// INFO 子项局部编辑：未点名子项保留（含未知），点名键替换/删除
fn edit_info_list(existing: &[RiffChunk], m: &Id3EditMeta) -> Option<Vec<u8>> {
    if m.blank_all { return None }
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for s in existing {
        if let Some(f) = info_field_of(&s.id) { if is_touched(m, f) { continue } }
        out.push((s.id.clone(), s.data.clone()));
    }
    // ⚠️ 闭包捕获 &mut out 会与后面的直接 push 冲突（E0499）；用局部函数 + 显式传参。
    fn push(out: &mut Vec<(String, Vec<u8>)>, id: &str, v: Option<&String>) {
        if let Some(x) = v { if !x.is_empty() { out.push((id.to_string(), x.as_bytes().to_vec())) } }
    }
    push(&mut out, "INAM", m.title.as_ref());
    push(&mut out, "IART", m.artists.as_ref().and_then(|a| a.first()));
    push(&mut out, "IPRD", m.albums.as_ref().and_then(|a| a.first()));
    push(&mut out, "ICRD", m.year.as_ref());
    if let Some(t) = m.track { out.push(("ITRK".into(), t.to_string().into_bytes())) }
    push(&mut out, "IGNR", m.genres.as_ref().and_then(|g| g.first()));
    push(&mut out, "ICMT", m.comment.as_ref());
    if out.is_empty() { return None }
    let mut body = b"INFO".to_vec();
    for (id, data) in &out { body.extend_from_slice(&chunk(id, data)) }
    Some(chunk("LIST", &body))
}

/// id3 chunk 帧局部编辑（内嵌 ID3v2）
fn edit_id3_chunk(existing: Option<&[u8]>, m: &Id3EditMeta) -> Option<Vec<u8>> {
    if m.blank_all { return None }
    let tag = existing.and_then(|b| parse_id3v2(b, 0));
    let has_touch = tag.is_some() || m.title.is_some() || m.artists.is_some() || m.albums.is_some()
        || m.album_artist.is_some() || m.track.is_some() || m.disc.is_some() || m.year.is_some()
        || m.genres.is_some() || m.composers.is_some() || m.comment.is_some() || m.lyrics.is_some()
        || m.lyrics_timed.is_some() || m.pictures.is_some() || !m.unset_fields.is_empty();
    if !has_touch { return existing.map(|b| b.to_vec()) }
    let frames = edit_id3v2_frames(tag.as_ref().map(|t| t.frames.as_slice()), m).concat();
    if frames.is_empty() { return None }
    let mut head = vec![0u8; 10];
    head[0..3].copy_from_slice(b"ID3");
    head[3] = 4; head[4] = 0; head[5] = 0;
    head[6..10].copy_from_slice(&write_sync_safe(frames.len() as i64));
    let mut inner = head; inner.extend_from_slice(&frames);
    Some(chunk("id3 ", &inner))
}

/// WAV 裸音频 sha256（data chunk 逐位一致才算完好）
pub fn audio_hash_wav(buf: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let data = wav_data(buf);
    let mut h = Sha256::new();
    h.update(&data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn wav_data(buf: &[u8]) -> Vec<u8> {
    if buf.len() < 12 { return Vec::new() }
    let riff = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    let end = (8 + riff).min(buf.len());
    parse_riff_chunks(buf, 12, end).into_iter().find(|c| c.id == "data").map(|c| c.data).unwrap_or_default()
}

/// WAV 写入：局部编辑 LIST INFO 子项 + 内嵌 ID3v2；其余 chunk 原样；RIFF size 重算
pub fn write_wav_tags(path: &Path, meta: &Id3EditMeta) -> Result<(), WriteError> {
    let orig = std::fs::read(path).map_err(WriteError::Read)?;
    if orig.len() < 12 || &orig[0..4] != b"RIFF" || &orig[8..12] != b"WAVE" {
        return Err(WriteError::BadFormat(format!("不是合法 WAV: {}", path.display())))
    }
    let _ = parse_wav(&orig).ok_or_else(|| WriteError::BadFormat("WAV chunk 树解析失败".into()))?;
    let riff_size = u32::from_le_bytes([orig[4], orig[5], orig[6], orig[7]]) as usize;
    let end = (8 + riff_size).min(orig.len());
    let chunks = parse_riff_chunks(&orig, 12, end);

    let mut info_chunk: Option<Vec<u8>> = None;
    let mut id3_chunk: Option<Vec<u8>> = None;
    let mut saw_info = false;
    let mut kept: Vec<u8> = Vec::new();
    for c in &chunks {
        if c.id == "LIST" && c.data.len() >= 4 && &c.data[0..4] == b"INFO" {
            saw_info = true;
            let subs = parse_riff_chunks(&c.data, 4, c.data.len());
            info_chunk = edit_info_list(&subs, meta);
            continue
        }
        if c.id == "id3 " { id3_chunk = edit_id3_chunk(Some(&c.data), meta); continue }
        // 原样搬运整个 chunk（含奇数长度的 pad 字节）
        let span = 8 + c.data.len() + (c.data.len() & 1);
        if c.offset + span <= orig.len() { kept.extend_from_slice(&orig[c.offset..c.offset + span]) }
    }
    if !saw_info { info_chunk = edit_info_list(&[], meta) }
    if let Some(x) = info_chunk { kept.extend_from_slice(&x) }
    if let Some(x) = id3_chunk { kept.extend_from_slice(&x) }

    let mut next = b"RIFF".to_vec();
    next.extend_from_slice(&u32le((kept.len() + 4) as u32));
    next.extend_from_slice(b"WAVE");
    next.extend_from_slice(&kept);

    atomic_replace(path, next, Some(&|o: &[u8], n: &[u8]| audio_hash_wav(o) == audio_hash_wav(n)))
        .map_err(WriteError::Atomic)
}

/// 走路径沙箱的 WAV 写入（服务端专用）
pub fn write_wav_tags_fs(fs: &crate::fs::PathSandbox, path: &Path, meta: &Id3EditMeta) -> Result<(), WriteError> {
    let target = fs.resolve(path)?;
    write_wav_tags(&target, meta)
}