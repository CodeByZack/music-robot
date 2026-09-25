//! MP3 ID3v2 局部编辑写入 —— 移植自 src/tag/write/mp3-writer.ts
//!
//! 语义：点名字段替换/删除，其余帧（含未知帧）payload 原样保留；APE tag 原样保留；
//! ID3v1 仅在原文件存在时同步重建（点名覆盖 + 原值继承）。**音频字节不动**。
use std::path::Path;

use super::super::read::{find_ape_footer, latin1_encode, id3v1_parse, parse_id3v2, read_sync_safe, write_sync_safe, ReadError};
use super::atomic::{atomic_replace, AtomicError};
use super::id3v2_editor::{edit_id3v2_frames, Id3EditMeta};

/// MP3 写入契约 = 帧编辑元数据 + blank 标记（等价 TS `WriteMeta extends Id3EditMeta`）。
pub type Mp3WriteMeta = Id3EditMeta;

/// 文本帧（统一 UTF-8, encoding=3）——供空 meta 全量重建复用
pub fn text_frame(id: &str, text: &str) -> Vec<u8> {
    let mut data = vec![3u8];
    data.extend_from_slice(text.trim_end_matches('\u{0}').as_bytes());
    let mut out: Vec<u8> = Vec::new();
    let mut b = latin1_encode(id); b.resize(4, b' '); b.truncate(4);
    out.extend_from_slice(&b);
    out.extend_from_slice(&write_sync_safe(data.len() as i64));
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&data);
    out
}

/// 构建 ID3v2.4 帧区（白名单重建，供 blank 全清 / 无原 tag 时使用）
pub fn build_id3v2_frames(meta: &Id3EditMeta) -> Vec<u8> {
    edit_id3v2_frames(None, meta).concat()
}

/// 构建 ID3v1 尾部（128B）；track 不在 1-254 视为无效写 0
pub fn build_id3v1(meta: &Id3EditMeta) -> Vec<u8> {
    let mut b = vec![0u8; 128];
    b[0..3].copy_from_slice(b"TAG");
    let put = |b: &mut Vec<u8>, off: usize, len: usize, s: Option<&String>| {
        if let Some(s) = s {
            if !s.is_empty() {
                let raw = latin1_encode(s);
                let n = raw.len().min(len);
                b[off..off + n].copy_from_slice(&raw[..n]);
            }
        }
    };
    put(&mut b, 3, 30, meta.title.as_ref());
    put(&mut b, 33, 30, meta.artists.as_ref().and_then(|a| a.first()).map(|x| x.to_string()).as_ref());
    put(&mut b, 63, 30, meta.albums.as_ref().and_then(|a| a.first()).map(|x| x.to_string()).as_ref());
    put(&mut b, 93, 4, meta.year.as_ref());
    put(&mut b, 97, 28, meta.comment.as_ref());
    let track = meta.track.unwrap_or(0);
    b[125] = 0;
    b[126] = if (1..=254).contains(&track) { track as u8 } else { 0 };
    b[127] = 255; // genre = Unknown(255)；0 是 Blues 会误显示
    b
}

/// 提取裸音频区域：ID3v2 之后、ID3v1/APEv2/垃圾尾部之前
pub fn mp3_audio_region(buf: &[u8]) -> (usize, usize) {
    let raw_start = match parse_id3v2(buf, 0) { Some(t) => 10 + t.tag_size as usize, None => 0 };
    let mut end = buf.len();
    if end >= 128 && &buf[end - 128..end - 125] == b"TAG" { return (clamp_start(raw_start, end - 128), end - 128) }
    if let Some(ape) = find_ape_footer(buf) {
        let size = read_sync_safe(buf, ape + 12, 0) as usize; // 占位防误用；下方按 LE 取真值
        let _ = size;
        let size = u32::from_le_bytes([buf[ape + 12], buf[ape + 13], buf[ape + 14], buf[ape + 15]]) as usize;
        let e = ape.saturating_sub(size);
        return (clamp_start(raw_start, e), e)
    }
    // 垃圾尾部（如 0x55×N）可能多层，循环剥净：128 窗口内 ≥120 个相同字节即视为垃圾
    while end >= 128 {
        let tail = &buf[end - 128..end];
        let same = tail[1..].iter().filter(|&&x| x == tail[0]).count();
        if same < 120 { break }
        end -= 128;
    }
    (clamp_start(raw_start, end), end)
}

/// 保证 start <= end：畸形文件的 ID3v2 声明长度可能越过音频终点，
/// TS 的 Buffer.slice 对反向区间容忍（返回空），Rust 切片会 panic —— 必须显式夹住。
fn clamp_start(start: usize, end: usize) -> usize { start.min(end) }

/// 音频区域 sha256（完整性校验）
pub fn audio_hash(buf: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let (s, e) = mp3_audio_region(buf);
    let mut h = Sha256::new();
    h.update(&buf[s.min(buf.len())..e.min(buf.len())]);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// APEv2 原始字节区（若有）。约束：APE 区必须完全落在 audioEnd 之后——
/// 音频字节里误命中 'APETAGEX' 的假 footer（size 乱值）会被 `pos - size < audioEnd` 排除。
fn ape_bytes_of(buf: &[u8]) -> Option<Vec<u8>> {
    let (_, audio_end) = mp3_audio_region(buf);
    let pos = find_ape_footer(buf)?;
    if pos < audio_end { return None }
    let size = u32::from_le_bytes([buf[pos + 12], buf[pos + 13], buf[pos + 14], buf[pos + 15]]) as usize;
    if pos < audio_end + size { return None }
    Some(buf[pos - size..pos + 32].to_vec())
}

fn has_id3v1(buf: &[u8]) -> bool { buf.len() >= 128 && &buf[buf.len() - 128..buf.len() - 125] == b"TAG" }

/// 局部编辑：merge(原 ID3v1 字段, 点名覆盖) → 重建 ID3v1 的视图
fn v1_meta_from(orig: &[u8], meta: &Id3EditMeta) -> Id3EditMeta {
    let v1 = id3v1_parse(orig);
    let un = |f: &str| meta.unset_fields.iter().any(|x| x == f);
    Id3EditMeta {
        title: Some(match (&meta.title, un("title")) { (Some(v), _) => v.clone(), (None, true) => String::new(), (None, false) => v1.as_ref().map(|x| x.title.clone()).unwrap_or_default() }),
        artists: Some(match (&meta.artists, un("artists")) { (Some(v), _) => v.clone(), (None, true) => vec![], (None, false) => v1.as_ref().filter(|x| !x.artist.is_empty()).map(|x| vec![x.artist.clone()]).unwrap_or_default() }),
        albums: Some(match (&meta.albums, un("albums")) { (Some(v), _) => v.clone(), (None, true) => vec![], (None, false) => v1.as_ref().filter(|x| !x.album.is_empty()).map(|x| vec![x.album.clone()]).unwrap_or_default() }),
        year: Some(match (&meta.year, un("year")) { (Some(v), _) => v.clone(), (None, true) => String::new(), (None, false) => v1.as_ref().map(|x| x.year.clone()).unwrap_or_default() }),
        comment: Some(match (&meta.comment, un("comment")) { (Some(v), _) => v.clone(), (None, true) => String::new(), (None, false) => v1.as_ref().map(|x| x.comment.clone()).unwrap_or_default() }),
        track: match (&meta.track, un("track")) { (Some(v), _) => Some(*v), (None, true) => None, (None, false) => v1.as_ref().and_then(|x| x.track) },
        ..Default::default()
    }
}

#[derive(Debug)]
pub enum WriteError { Read(std::io::Error), Atomic(AtomicError) }
impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { WriteError::Read(e) => write!(f, "读取失败：{e}"), WriteError::Atomic(e) => write!(f, "{e}") }
    }
}
impl From<ReadError> for WriteError { fn from(e: ReadError) -> Self { WriteError::Read(std::io::Error::other(e.to_string())) } }

/// MP3 写入：局部编辑 ID3v2（未知帧/APE 保留），原子替换 + 裸音频 hash 校验
pub fn write_mp3_tags(path: &Path, meta: &Mp3WriteMeta) -> Result<(), WriteError> {
    let orig_buf = std::fs::read(path).map_err(WriteError::Read)?;
    let (start, end) = mp3_audio_region(&orig_buf);
    let audio = orig_buf[start.min(orig_buf.len())..end.min(orig_buf.len())].to_vec();

    let orig_tag = parse_id3v2(&orig_buf, 0);
    let mut head = vec![0u8; 10];
    head[0..3].copy_from_slice(b"ID3");
    head[3] = 4; head[4] = 0; head[5] = 0;

    let (frames_bytes, padding, ape) = if meta.blank_all {
        (Vec::new(), 8192usize, None) // blank：0 帧 + 8192 padding；APEv2 一并清除
    } else {
        let fb = edit_id3v2_frames(orig_tag.as_ref().map(|t| t.frames.as_slice()), meta).concat();
        let pad = orig_tag.as_ref().map(|t| t.padding.max(0)).unwrap_or(8192);
        (fb, pad, ape_bytes_of(&orig_buf))
    };
    head[6..10].copy_from_slice(&write_sync_safe((frames_bytes.len() + padding) as i64));
    let mut tag_bytes = head;
    tag_bytes.extend_from_slice(&frames_bytes);
    tag_bytes.extend_from_slice(&vec![0u8; padding]);

    let mut parts: Vec<u8> = Vec::with_capacity(tag_bytes.len() + audio.len());
    parts.extend_from_slice(&tag_bytes);
    parts.extend_from_slice(&audio);
    if let Some(a) = ape { parts.extend_from_slice(&a) }
    if has_id3v1(&orig_buf) {
        // ⚠️ 必须先绑定再传引用：`&build_id3v1(if … { &Default::default() } else { … })` 会让
        //   临时值在语句结束就析构（借用检查器 E0716）——TS 里这种写法完全合法。
        let v1_meta = if meta.blank_all { Id3EditMeta::default() } else { v1_meta_from(&orig_buf, meta) };
        parts.extend_from_slice(&build_id3v1(&v1_meta));
    }
    let next = parts;

    atomic_replace(path, next, Some(&|o: &[u8], n: &[u8]| audio_hash(o) == audio_hash(n)))
        .map_err(WriteError::Atomic)
}
