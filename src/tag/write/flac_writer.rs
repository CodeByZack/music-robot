//! FLAC Vorbis Comment 键级局部编辑 —— 移植自 src/tag/write/flac-writer.ts
//!
//! 语义：只动点名字段对应的键组；未点名键（LANGUAGE/LYRICIST/ENCODER/…）原样保留；
//! 其它元数据块（SEEKTABLE/APPLICATION/CUESHEET/PADDING）原样保留；STREAMINFO 不动。
//! ⚠️ 最后这条正是 lofty 做不到的地方（它的 FlacFile 结构体没有存放"其它块"的字段）。
use std::path::Path;

use super::super::read::flac::{extract_stream_info_md5, flac_audio_start, parse_flac_metadata};
use super::super::read::{le_u32_at, Picture};
use super::atomic::atomic_replace;
use super::id3v2_editor::Id3EditMeta;
use super::mp3_writer::WriteError;

fn u32le(v: u32) -> [u8; 4] { v.to_le_bytes() }
fn u32be(v: u32) -> [u8; 4] { v.to_be_bytes() }

#[derive(Debug, Clone)]
struct VorbisPair { key: String, value: Vec<u8> }

/// 解析 VORBIS_COMMENT payload。⚠️ 守卫用 `p + 4 > len` 而非 `p > len`
/// （round7 P2-3：空 comment 块再 blank 时曾在此越界）。
fn parse_vorbis_pairs(payload: &[u8]) -> Vec<VorbisPair> {
    let mut out = Vec::new();
    let mut p = 0usize;
    if p + 4 > payload.len() { return out }
    let vendor_len = le_u32_at(payload, p) as usize; p += 4;
    p += vendor_len + 4; // vendor 跳过 + count 的 4B
    if p + 4 > payload.len() { return out }
    let count = le_u32_at(payload, p - 4) as usize;
    for _ in 0..count {
        if p + 4 > payload.len() { break }
        let len = le_u32_at(payload, p) as usize; p += 4;
        if p + len > payload.len() { break }
        let raw = &payload[p..p + len]; p += len;
        let eq = match raw.iter().position(|&b| b == b'=') { Some(x) if x > 0 => x, _ => continue };
        out.push(VorbisPair {
            key: super::super::read::latin1_decode(&raw[..eq]).to_uppercase(),
            value: raw[eq + 1..].to_vec(),
        });
    }
    out
}

/// 字段 → vorbis 键组（大写）
fn keys_of(field: &str) -> &'static [&'static str] {
    match field {
        "title" => &["TITLE"], "artists" => &["ARTIST"], "albums" => &["ALBUM"],
        "albumArtist" => &["ALBUMARTIST"], "track" => &["TRACKNUMBER"], "trackTotal" => &["TRACKTOTAL"],
        "disc" => &["DISCNUMBER"], "discTotal" => &["DISCTOTAL"], "year" => &["DATE"],
        "genres" => &["GENRE"], "composers" => &["COMPOSER"], "comment" => &["COMMENT"],
        "lyrics" => &["LYRICS"], _ => &[],
    }
}

fn vendor_of(payload: &[u8]) -> String {
    if payload.len() < 4 { return "tagwash".into() }
    let len = le_u32_at(payload, 0) as usize;
    if 4 + len > payload.len() { return "tagwash".into() }
    let v: String = super::super::read::utf8_lossy_local(&payload[4..4 + len])
        .chars().filter(|c| !('\u{0}'..='\u{1f}').contains(c)).collect();
    if v.is_empty() { "tagwash".into() } else { v }
}

fn payload_from(pairs: &[VorbisPair], vendor: &str) -> Vec<u8> {
    let mut parts: Vec<u8> = Vec::new();
    let vb = vendor.as_bytes();
    parts.extend_from_slice(&u32le(vb.len() as u32));
    parts.extend_from_slice(vb);
    parts.extend_from_slice(&u32le(pairs.len() as u32));
    for p in pairs {
        let mut item = super::super::read::latin1_encode(&p.key);
        item.push(b'=');
        item.extend_from_slice(&p.value);
        parts.extend_from_slice(&u32le(item.len() as u32));
        parts.extend_from_slice(&item);
    }
    parts
}

fn push_str(out: &mut Vec<VorbisPair>, key: &str, v: Option<&String>) {
    if let Some(s) = v { if !s.is_empty() { out.push(VorbisPair { key: key.into(), value: s.as_bytes().to_vec() }) } }
}
fn push_num(out: &mut Vec<VorbisPair>, key: &str, v: Option<i64>) {
    if let Some(n) = v { out.push(VorbisPair { key: key.into(), value: n.to_string().into_bytes() }) }
}

/// 键级局部编辑：删点名字段的键组 + 追加点名新值
fn edit_vorbis_pairs(existing: &[VorbisPair], m: &Id3EditMeta) -> Vec<VorbisPair> {
    let mut out = existing.to_vec();
    let is_unset = |f: &str| m.unset_fields.iter().any(|x| x == f);
    let touched = |f: &str| -> bool {
        if is_unset(f) { return true }
        match f {
            "title" => m.title.is_some(), "artists" => m.artists.is_some(), "albums" => m.albums.is_some(),
            "albumArtist" => m.album_artist.is_some(), "track" => m.track.is_some(), "trackTotal" => m.track_total.is_some(),
            "disc" => m.disc.is_some(), "discTotal" => m.disc_total.is_some(), "year" => m.year.is_some(),
            "genres" => m.genres.is_some(), "composers" => m.composers.is_some(), "comment" => m.comment.is_some(),
            _ => false,
        }
    };
    for field in ["title","artists","albums","albumArtist","track","trackTotal","disc","discTotal","year","genres","composers","comment"] {
        if !touched(field) { continue }
        let keys = keys_of(field);
        out.retain(|p| !keys.contains(&p.key.as_str()));
    }
    // lyrics 与 lyricsTimed 共用一个 LYRICS 键，任一被点名都整体替换
    let lyrics_touched = m.lyrics.is_some() || m.lyrics_timed.is_some() || is_unset("lyrics") || is_unset("lyricsTimed");
    if lyrics_touched { out.retain(|p| p.key != "LYRICS") }

    push_str(&mut out, "TITLE", m.title.as_ref());
    for a in m.artists.clone().unwrap_or_default() { if !a.is_empty() { out.push(VorbisPair { key: "ARTIST".into(), value: a.into_bytes() }) } }
    push_str(&mut out, "ALBUMARTIST", m.album_artist.as_ref());
    for a in m.albums.clone().unwrap_or_default() { if !a.is_empty() { out.push(VorbisPair { key: "ALBUM".into(), value: a.into_bytes() }) } }
    push_num(&mut out, "TRACKNUMBER", m.track);
    push_num(&mut out, "TRACKTOTAL", m.track_total);
    push_num(&mut out, "DISCNUMBER", m.disc);
    push_num(&mut out, "DISCTOTAL", m.disc_total);
    push_str(&mut out, "DATE", m.year.as_ref());
    for g in m.genres.clone().unwrap_or_default() { if !g.is_empty() { out.push(VorbisPair { key: "GENRE".into(), value: g.into_bytes() }) } }
    for c in m.composers.clone().unwrap_or_default() { if !c.is_empty() { out.push(VorbisPair { key: "COMPOSER".into(), value: c.into_bytes() }) } }
    push_str(&mut out, "COMMENT", m.comment.as_ref());
    // 与 TS 一致：lyricsTimed 优先，非空则用它；否则退回 lyrics
    let timed_ok = m.lyrics_timed.as_ref().map(|s| !s.is_empty()).unwrap_or(false);
    if timed_ok {
        out.push(VorbisPair { key: "LYRICS".into(), value: m.lyrics_timed.clone().unwrap().into_bytes() });
    } else if let Some(l) = &m.lyrics {
        if !l.is_empty() { out.push(VorbisPair { key: "LYRICS".into(), value: l.clone().into_bytes() }) }
    }
    out
}

fn block_with(ty: u8, payload: &[u8], last: bool) -> Vec<u8> {
    let mut h = vec![(ty & 0x7f) | if last { 0x80 } else { 0 }];
    h.push(((payload.len() >> 16) & 0xff) as u8);
    h.push(((payload.len() >> 8) & 0xff) as u8);
    h.push((payload.len() & 0xff) as u8);
    let mut out = h;
    out.extend_from_slice(payload);
    out
}

/// FLAC PICTURE 块的 **payload**（字段大端；type 在前）。
/// ⚠️ 刻意不含 4B 块头：本模块内部统一「Piece 只存 payload，写出时由 block_with 加头」，
///   避免双重头（我第一版把带头的完整块塞进 pieces，末尾又加一次头 → type 读成 0x06000031）。
fn picture_payload(pic: &Picture) -> Vec<u8> {
    let mime = super::super::read::latin1_encode(if pic.mime_type.is_empty() { "image/jpeg" } else { &pic.mime_type });
    let desc = pic.description.trim_end_matches('\u{0}').as_bytes().to_vec();
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(&u32be(pic.pic_type as u32));
    payload.extend_from_slice(&u32be(mime.len() as u32)); payload.extend_from_slice(&mime);
    payload.extend_from_slice(&u32be(desc.len() as u32)); payload.extend_from_slice(&desc);
    for _ in 0..4 { payload.extend_from_slice(&u32be(0)) } // w/h/depth/colors
    payload.extend_from_slice(&pic.data);
    payload
}

/// 完整 FLAC PICTURE 块（含块头），供外部单独使用
pub fn build_picture_block(pic: &Picture) -> Vec<u8> { block_with(6, &picture_payload(pic), false) }

/// 完整 VORBIS_COMMENT 块（供全量重建/测试）
pub fn build_vorbis_comment_block(m: &Id3EditMeta) -> Vec<u8> {
    block_with(4, &payload_from(&edit_vorbis_pairs(&[], m), "tagwash"), false)
}

/// FLAC 裸音频 sha256（STREAMINFO md5 之外的第二道校验）
pub fn audio_hash_flac(buf: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let start = flac_audio_start(buf);
    let mut h = Sha256::new();
    h.update(&buf[start.min(buf.len())..]);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// FLAC 写入：键级局部编辑（保留全部元数据块），音频字节不动
pub fn write_flac_tags(path: &Path, meta: &Id3EditMeta) -> Result<(), WriteError> {
    let orig = std::fs::read(path).map_err(WriteError::Read)?;
    let blocks = match parse_flac_metadata(&orig) {
        Some(b) if !b.is_empty() => b,
        _ => return Err(WriteError::BadFormat(format!("不是合法 FLAC: {}", path.display()))),
    };
    let audio_start = flac_audio_start(&orig);
    let audio = orig[audio_start.min(orig.len())..].to_vec();

    let mut existing: Vec<VorbisPair> = Vec::new();
    let mut vendor = "tagwash".to_string();
    if let Some(vc) = blocks.iter().find(|b| b.ty == 4) {
        existing = parse_vorbis_pairs(&vc.payload);
        vendor = vendor_of(&vc.payload);
    }
    if meta.blank_all { existing.clear() }

    let pic_touched = meta.pictures.is_some() || meta.blank_all;
    let new_pics: Vec<Vec<u8>> = if meta.blank_all { Vec::new() } else {
        meta.pictures.clone().unwrap_or_default().into_iter().filter(|p| !p.data.is_empty()).map(|p| picture_payload(&p)).collect()
    };

    // 按原块序重建 metadata：保留 STREAMINFO 及所有非 4/6 块
    #[derive(Clone)]
    struct Piece { ty: u8, payload: Vec<u8> }
    let mut pieces: Vec<Piece> = Vec::new();
    let (mut vc_done, mut pic_idx) = (false, 0usize);
    for b in &blocks {
        match b.ty {
            0 => pieces.push(Piece { ty: 0, payload: b.payload.clone() }),
            4 => {
                if !vc_done {
                    let payload = if !existing.is_empty() || meta.title.is_some() || meta.artists.is_some() {
                        payload_from(&edit_vorbis_pairs(&existing, meta), &vendor)
                    } else {
                        payload_from(&[], &vendor)
                    };
                    pieces.push(Piece { ty: 4, payload });
                    vc_done = true;
                }
            }
            6 => {
                if pic_touched { if pic_idx < new_pics.len() { pieces.push(Piece { ty: 6, payload: new_pics[pic_idx].clone() }); pic_idx += 1 } }
                else { pieces.push(Piece { ty: 6, payload: b.payload.clone() }) }
            }
            other => pieces.push(Piece { ty: other, payload: b.payload.clone() }),
        }
    }
    // 原文件无 vorbis 块：有点名内容或 blank 则补一个（放在 STREAMINFO 之后）
    let need_vc = meta.blank_all || !existing.is_empty() || meta.title.is_some() || meta.artists.is_some()
        || meta.albums.is_some() || meta.comment.is_some()
        || (meta.pictures.is_some() && !new_pics.is_empty());
    if !vc_done && need_vc {
        let payload = payload_from(&edit_vorbis_pairs(&existing, meta), "tagwash");
        let si_idx = pieces.iter().position(|x| x.ty == 0).map(|i| i + 1).unwrap_or(0);
        pieces.insert(si_idx, Piece { ty: 4, payload });
        vc_done = true;
    }
    let _ = vc_done;
    if pic_touched { while pic_idx < new_pics.len() { pieces.push(Piece { ty: 6, payload: new_pics[pic_idx].clone() }); pic_idx += 1 } }

    let mut next: Vec<u8> = b"fLaC".to_vec();
    for (i, p) in pieces.iter().enumerate() {
        next.extend_from_slice(&block_with(p.ty, &p.payload, i == pieces.len() - 1));
    }
    next.extend_from_slice(&audio);

    atomic_replace(path, next, Some(&|o: &[u8], n: &[u8]| {
        extract_stream_info_md5(o) == extract_stream_info_md5(n) && audio_hash_flac(o) == audio_hash_flac(n)
    })).map_err(WriteError::Atomic)
}

/// 走路径沙箱的 FLAC 写入（服务端专用）
pub fn write_flac_tags_fs(fs: &crate::fs::PathSandbox, path: &Path, meta: &Id3EditMeta) -> Result<(), WriteError> {
    let target = fs.resolve(path)?;
    write_flac_tags(&target, meta)
}