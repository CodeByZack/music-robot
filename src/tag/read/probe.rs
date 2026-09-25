//! 共享格式探测 —— 移植自 src/probe.ts。readTags/writeTags/scan 三处同源判定。
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, Clone, PartialEq)]
pub enum Probe {
    Plain(Format),
    /// ID3v2 前缀 + 内部真实是 FLAC/WAV（yt-dlp --add-metadata 产物）→ readTags 必须拒绝
    Id3PrefixedReal(Format),
    /// ID3v2 前缀 + 之后不是已知容器 → 拒绝，不走扩展名容错
    Id3ChainUnresolved,
    /// magic 无信 → 允许按扩展名兜底
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format { Mp3, Flac, Wav }
impl Format { pub fn ext(&self) -> &'static str { match self { Format::Mp3 => "mp3", Format::Flac => "flac", Format::Wav => "wav" } } }

fn read_at(f: &mut File, pos: u64, n: usize) -> Vec<u8> {
    let _ = f.seek(SeekFrom::Start(pos));
    let mut b = vec![0u8; n];
    match f.read_exact(&mut b) { Ok(()) => b, Err(_) => { // 部分读：保留已读到的字节（等价 TS readOnce 的短读语义）
        let _ = f.rewind();
        let mut tmp = Vec::new(); let _ = f.seek(SeekFrom::Start(pos)).map(|_| f.read_to_end(&mut tmp));
        tmp.truncate(n); tmp } }
}

/// footer-only 布局探测：在 64KB 窗口内找自校验通过的 APEv2 footer / LYRICS3 尾标。
/// 自校验不过（魔数恰好出现在音频数据里）不跳——宁可拒绝不误判（round6 P2-2）。
fn find_tag_footer(f: &mut File, pos: u64) -> Option<u64> {
    const WINDOW: usize = 64 * 1024;
    let chunk = read_at(f, pos, WINDOW);
    // APETAGEX footer：size 字段须满足 size === offset + 32（即 footer 落在文件尾）
    for w in chunk.windows(8).filter(|w| *w == b"APETAGEX") {
        let off = w.as_ptr() as usize - chunk.as_ptr() as usize;
        if off + 16 > chunk.len() { continue }
        let ape_size = u32::from_le_bytes(chunk[off + 12..off + 16].try_into().unwrap()) as u64;
        if ape_size == (pos + off as u64) + 32 { return Some(pos + off as u64) }
    }
    // LYRICS3：len6 数值 === offset + 15
    for w in chunk.windows(6).filter(|w| *w == b"LYRICS") {
        let off = w.as_ptr() as usize - chunk.as_ptr() as usize;
        let len_field = &chunk[off + 6..(off + 12).min(chunk.len())];
        let digits: String = len_field.iter().take_while(|b| b.is_ascii_digit()).map(|b| *b as char).collect();
        if let Ok(n) = digits.parse::<u64>() { if n == (pos + off as u64) + 15 { return Some(pos + off as u64) } }
    }
    None
}

pub fn probe_format(path: &std::path::Path) -> Probe {
    let Ok(mut f) = File::open(path) else { return Probe::Unknown };
    let head = read_at(&mut f, 0, 12);
    let g = |i: usize| head.get(i).copied().unwrap_or(0);
    let is_id3 = head.len() >= 3 && &head[0..3] == b"ID3";
    if is_id3 {
        let tag_size = ((g(6) & 0x7f) as u64) << 21 | ((g(7) & 0x7f) as u64) << 14
            | ((g(8) & 0x7f) as u64) << 7 | (g(9) & 0x7f) as u64;
        let mut pos = 10 + tag_size;
        let mut probe = read_at(&mut f, pos, 32);
        for _hop in 0..8 {
            if probe.len() >= 8 && &probe[0..8] == b"APETAGEX" {
                if probe.len() < 32 { break }
                let ape_size = u32::from_le_bytes(probe[12..16].try_into().unwrap());
                let has_header = u32::from_le_bytes(probe[16..20].try_into().unwrap()) & 0x8000_0000 != 0;
                if ape_size < 32 || ape_size > 256 * 1024 * 1024 { break } // 异常 size：不跳
                pos += ape_size as u64 + if has_header { 32 } else { 0 };
                probe = read_at(&mut f, pos, 32);
                continue;
            }
            if probe.len() >= 3 && &probe[0..3] == b"ID3" {
                let sz = ((probe.get(6).copied().unwrap_or(0) & 0x7f) as u64) << 21
                    | ((probe.get(7).copied().unwrap_or(0) & 0x7f) as u64) << 14
                    | ((probe.get(8).copied().unwrap_or(0) & 0x7f) as u64) << 7
                    | (probe.get(9).copied().unwrap_or(0) & 0x7f) as u64;
                pos += 10 + sz;
                probe = read_at(&mut f, pos, 32);
                continue;
            }
            // 便宜前置（round7）：跳点已是容器 magic / MPEG sync 时直接终判，不付 64KB 探测
            if probe.len() >= 4 && &probe[0..4] == b"fLaC" { break }
            if probe.len() >= 12 && &probe[0..4] == b"RIFF" && &probe[8..12] == b"WAVE" { break }
            if probe.first().copied() == Some(0xff) && probe.get(1).map(|b| b & 0xe0 == 0xe0).unwrap_or(false) { break }
            match find_tag_footer(&mut f, pos) {
                Some(foot) => { pos = foot; probe = read_at(&mut f, pos, 32); continue }
                None => break,
            }
        }
        if probe.len() >= 4 && &probe[0..4] == b"fLaC" { return Probe::Id3PrefixedReal(Format::Flac) }
        if probe.len() >= 12 && &probe[0..4] == b"RIFF" && &probe[8..12] == b"WAVE" { return Probe::Id3PrefixedReal(Format::Wav) }
        if probe.first().copied() == Some(0xff) && probe.get(1).map(|b| b & 0xe0 == 0xe0).unwrap_or(false) { return Probe::Plain(Format::Mp3) }
        return Probe::Id3ChainUnresolved;
    }
    if head.len() >= 4 && &head[0..4] == b"fLaC" { return Probe::Plain(Format::Flac) }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" { return Probe::Plain(Format::Wav) }
    if g(0) == 0xff && (g(1) & 0xe0) == 0xe0 { return Probe::Plain(Format::Mp3) }
    Probe::Unknown
}
