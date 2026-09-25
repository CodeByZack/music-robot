//! 无 ffprobe 时的本地音频属性估算 —— 移植自 src/tag/read/native-probe.ts
//!
//! 优先级：ffprobe 可用 → 用它（与播放器/ffmpeg 报告一致）；
//!        不可用 → 本模块内置算法：FLAC/WAV 精确，MP3 基于 Info/Xing 头（±40ms）。
//! 这条兜底通道**必须真实可用**：否则在没有 ffmpeg 的机器上工具会对 MP3 报时长 0。
use super::flac;
use super::id3v2::be_u32_at;

#[derive(Debug, Clone, Copy, Default)]
pub struct NativeProbe { pub duration_ms: i64, pub sample_rate: u32, pub bits_per_sample: Option<u32>, pub bitrate_bps: Option<i64> }
impl NativeProbe { pub fn zeroed() -> Self { NativeProbe::default() } }

/// MPEG1 Layer I/II/III 码率表（kbps），索引取自帧头 b2 的高 4 bit
const BITRATE_TABLES: [[u32; 15]; 4] = [
    [0; 15], // layer 0 = 保留
    [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448], // L1
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],     // L2
    [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],      // L3
];
/// MPEG2 / MPEG2.5 码率表
const BITRATE_TABLES_V2: [[u32; 15]; 4] = [
    [0; 15],
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256], // L1
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],      // L2
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],      // L3
];
const SAMPLE_RATES: [u32; 4] = [44100, 48000, 32000, 0];

#[derive(Debug, Clone, Copy)]
pub struct MpegFrame {
    pub version_bits: u8, pub layer: u8, pub bitrate_kbps: u32,
    pub sample_rate: u32, pub samples_per_frame: u32, pub frame_len: usize, pub mono: bool,
}

/// 解析 MPEG 音频帧头（MPEG1/2/2.5 × Layer I/II/III）。非法组合返回 None。
pub fn parse_mpeg_frame(buf: &[u8], pos: usize) -> Option<MpegFrame> {
    if pos + 4 > buf.len() { return None }
    if buf[pos] != 0xff || (buf[pos + 1] & 0xe0) != 0xe0 { return None }
    let b1 = buf[pos + 1]; let b2 = buf[pos + 2]; let b3 = buf[pos + 3];
    let version_bits = (b1 >> 3) & 0x03;   // 3=MPEG1 2=MPEG2 0=MPEG2.5（1=保留）
    let layer_bits = (b1 >> 1) & 0x03;     // 3=L1 2=L2 1=L3（0=保留）
    if version_bits == 1 || layer_bits == 0 { return None }
    let bitrate_idx = (b2 >> 4) & 0x0f;
    let sr_idx = (b2 >> 2) & 0x03;
    let padding = (b2 >> 1) & 0x01;
    let layer = match layer_bits { 3 => 1, 2 => 2, _ => 3 };
    if bitrate_idx == 0 || bitrate_idx == 15 { return None }
    let v2 = version_bits == 2 || version_bits == 0;
    let table = if v2 { BITRATE_TABLES_V2[layer as usize] } else { BITRATE_TABLES[layer as usize] };
    let bitrate_kbps = table[bitrate_idx as usize];
    let base_sr = *SAMPLE_RATES.get(sr_idx as usize)?;
    let sample_rate = match version_bits {
        3 => base_sr,                                   // MPEG1
        2 => base_sr / 2,                               // MPEG2
        _ => base_sr / 4,                               // MPEG2.5
    };
    if sample_rate == 0 { return None }
    let samples_per_frame = match (layer, v2) {
        (1, _) => 384, (2, _) => 1152, (_, true) => 576, (_, false) => 1152,
    };
    // 与 TS 一致：Layer I 是 floor(12*br/sr + pad)*4，其余 floor(sr_per_frame/8*br/sr)+pad
    let br = bitrate_kbps as f64;
    let sr = sample_rate as f64;
    let frame_len = if layer == 1 {
        (((12.0 * br * 1000.0 / sr).floor() + padding as f64) as usize) * 4
    } else {
        (((samples_per_frame as f64 / 8.0) * br * 1000.0 / sr).floor() as usize) + padding as usize
    };
    let mono = ((b3 >> 6) & 0x03) == 3;
    Some(MpegFrame { version_bits, layer, bitrate_kbps, sample_rate, samples_per_frame, frame_len, mono })
}

/// MP3：优先 Xing/Info 头的帧数（VBR 准确），缺省时按 CBR 用字节数估算。
pub fn mp3_native_probe(buf: &[u8], audio_start: usize) -> NativeProbe {
    let f = match parse_mpeg_frame(buf, audio_start) { Some(x) => x, None => return NativeProbe::zeroed() };
    let side_info_len = match (f.mono, f.version_bits) {
        (false, 3) => 32, (false, _) => 17, (true, 3) => 17, (true, _) => 9,
    };
    let tag_pos = audio_start + 4 + side_info_len as usize;
    if tag_pos + 12 <= buf.len() {
        let id = &buf[tag_pos..tag_pos + 4];
        if id == b"Xing" || id == b"Info" {
            let flags = be_u32_at(buf, tag_pos + 4);
            let frames = if flags & 1 != 0 { be_u32_at(buf, tag_pos + 8) } else { 0 };
            let bytes = (buf.len() - audio_start) as u64;
            // 有帧数 → 精确时长；再反推平均码率
            if frames > 0 {
                let dur = frames as f64 * f.samples_per_frame as f64 / f.sample_rate as f64;
                let ms = (dur * 1000.0).round() as i64;
                let br = if ms > 0 { (bytes * 8 * 1000 / ms as u64) as i64 } else { (f.bitrate_kbps * 1000) as i64 };
                return NativeProbe { duration_ms: ms, sample_rate: f.sample_rate, bits_per_sample: None, bitrate_bps: Some(br) };
            }
        }
    }
    // CBR 估算
    let bytes = (buf.len() - audio_start) as u64;
    let br_bps = (f.bitrate_kbps * 1000) as u64;
    if br_bps == 0 { return NativeProbe::zeroed() }
    let ms = (bytes * 8 * 1000 / br_bps) as i64;
    NativeProbe { duration_ms: ms, sample_rate: f.sample_rate, bits_per_sample: None, bitrate_bps: Some(br_bps as i64) }
}

/// FLAC：从 STREAMINFO 取（精确）
pub fn flac_native_probe(buf: &[u8]) -> Option<NativeProbe> {
    let blocks = flac::parse_flac_metadata(buf)?;
    let si = blocks.iter().find(|b| b.ty == 0)?;
    let (sr, bits, total) = flac::parse_stream_info(&si.payload);
    if sr == 0 { return None }
    Some(NativeProbe {
        duration_ms: ((total as f64 / sr as f64) * 1000.0).round() as i64,
        sample_rate: sr, bits_per_sample: (bits > 0).then_some(bits), bitrate_bps: None,
    })
}

/// WAV：data chunk 字节数 ÷ byteRate（精确）
pub fn wav_native_probe(buf: &[u8]) -> NativeProbe {
    match super::wav::parse_wav(buf) {
        Some(w) => {
            let data_bytes = w.data_len as f64;
            let ms = if let Some(br) = w.byte_rate { if br > 0 { (data_bytes / br as f64 * 1000.0).round() as i64 } else { 0 } } else { 0 };
            NativeProbe { duration_ms: ms, sample_rate: w.sample_rate.unwrap_or(0),
                          bits_per_sample: w.bits_per_sample, bitrate_bps: w.byte_rate.map(|b| (b * 8) as i64) }
        }
        None => NativeProbe::zeroed(),
    }
}
