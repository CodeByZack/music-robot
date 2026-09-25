//! 读取层能力模块（对应 src/tag/read/**）——纯库，Web/CLI 直接 import。
pub mod metadata;
pub mod id3v2;
pub mod id3v1;
pub mod gbk_sniff;
pub mod probe;
pub mod flac;
pub mod apev2;
pub mod wav;
pub mod native_probe;
mod dispatch;

pub use metadata::{AudioMetadata, Picture, RawFrame, ReadError, Result};
pub use gbk_sniff::{gbk_sniff, decode_maybe_gbk, GbkSniffResult};
pub use id3v1::{id3v1_parse, id3v1_genres, parse_track_byte};
pub use dispatch::{read_tags, read_mp3, read_flac, FramesMeta};
pub use probe::{probe_format, Format, Probe};
pub mod apev2_pub { pub use super::apev2::{parse_ape_tag, find_ape_footer, ApeItem, ApeTag}; }
