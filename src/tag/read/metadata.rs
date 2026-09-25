//! 统一元数据模型 —— 与 TS AudioMetadata 逐字段对齐（src/tag/read/index.ts:24-50）
//!
//! 语义映射约定（重要，避免两侧漂移）：
//!   TS `string | undefined`（可选、缺失即无此字段）→ Rust `Option<String>`
//!   TS `number | null`（显式"无值"）              → Rust `Option<i64>`
//!   TS `Buffer`                                    → Rust `Vec<u8>`
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Picture { pub mime_type: String, pub pic_type: u8, pub description: String, pub data: Vec<u8> }

#[derive(Debug, Clone)]
pub struct RawFrame { pub frame_id: String, pub size: usize, pub data: Vec<u8> }

/// 读取层的错误。TS 侧只有 3 个抛出点（read/index.ts:340,341,350），
/// 全部是「明确拒绝」语义——不是解析失败（parser 一律返回空值继续）。
#[derive(Debug)]
pub enum ReadError {
    Io(std::io::Error),
    /// ID3v2 前缀 + 真实容器是 FLAC/WAV（yt-dlp --add-metadata 产物）→ 必须拒绝，否则 blank 会假清空
    Id3PrefixedReal(&'static str),
    /// ID3v2 前缀 + 未知容器（OggS/ftyp…）→ 拒绝
    Id3PrefixedUnknown,
    /// 完全无法识别的格式
    Unrecognized,
}
impl ReadError {
    pub fn message(&self) -> String { self.to_string() }
}
impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "IO 错误：{e}"),
            ReadError::Id3PrefixedReal(fmt) => write!(f, "无法识别：文件以 ID3v2 头开始但内部为 {fmt}（疑似 yt-dlp --add-metadata 产物）。请用 ffmpeg 重封装修复：ffmpeg -i <file> -c copy <out>.{fmt}"),
            ReadError::Id3PrefixedUnknown => write!(f, "无法识别：ID3v2 头之后的内容不是已知音频容器，拒绝按 MP3 解析"),
            ReadError::Unrecognized => write!(f, "无法识别：不支持的文件格式或文件无有效标签区"),
        }
    }
}
impl std::error::Error for ReadError {}
impl From<std::io::Error> for ReadError { fn from(e: std::io::Error) -> Self { ReadError::Io(e) } }

#[derive(Debug, Clone, Default)]
pub struct AudioMetadata {
    pub title: Option<String>,
    pub artists: Vec<String>,
    pub albums: Vec<String>,
    pub album_artist: Option<String>,
    pub track: Option<i64>,
    pub track_total: Option<i64>,
    pub disc: Option<i64>,
    pub disc_total: Option<i64>,
    pub year: Option<String>,          // 任意字符串："2019/05"、"circa 199?" 都要能存
    pub genres: Vec<String>,
    pub composers: Vec<String>,
    pub duration_ms: i64,
    pub sample_rate: u32,
    pub bits_per_sample: Option<u32>,
    pub bitrate_bps: Option<i64>,
    pub lyrics: Option<String>,
    pub lyrics_timed: Option<String>,
    pub comment: Option<String>,
    pub pictures: Vec<Picture>,
    pub raw_frames: Vec<RawFrame>,
    pub source: String,                // 'id3v2' | 'id3v1' | 'apev2' | 'vorbis' | 'riff'
    pub musicbrainz_artist_id: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_track_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub musicbrainz_disc_id: Option<String>,
    pub isrc: Option<String>,
    /// 诊断用：真实格式（probe 结果），TS 侧无此字段（review round8 P2-1 曾因此踩死分支）
    pub detected_format: Option<String>,
    pub path: Option<PathBuf>,
}
pub type Result<T> = std::result::Result<T, ReadError>;

