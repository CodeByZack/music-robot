//! 写入层能力模块（对应 src/tag/write/**）——纯库，Web 直接 import。
pub mod intent;
pub mod atomic;
pub mod id3v2_editor;
pub mod mp3_writer;
pub mod flac_writer;
pub mod wav_writer;

pub use atomic::{atomic_replace, AtomicError};
pub use intent::{merge_fields, diff_fields, format_diff, format_json_diff, format_json_diff_full, preview_view, blank_view, sniff_image_mime, AfterView, DiffLine, WriteMeta, WritableFields, UNSET_KEYS};
pub use id3v2_editor::Id3EditMeta;
pub use mp3_writer::{audio_hash, build_id3v1, build_id3v2_frames, mp3_audio_region, write_mp3_tags, write_mp3_tags_fs, WriteError};
pub use flac_writer::{audio_hash_flac, build_picture_block, build_vorbis_comment_block, write_flac_tags, write_flac_tags_fs};
pub use wav_writer::{audio_hash_wav, write_wav_tags, write_wav_tags_fs};

use std::path::Path;

/// 统一写入口：按 magic 分派（TS writeTags :15-30）。
///
/// ⚠️ TS 里那段「运行时检测 intent 专属字段误传」（round6 P2-3）**不需要移植**：
/// Rust 的 `WritableFields` 与 `WriteMeta` 是两个不同类型，编译器已经保证不会混。
pub fn write_tags(path: &Path, meta: &Id3EditMeta) -> Result<(), WriteError> {
    use crate::tag::read::{probe_format, Format, Probe};
    // ⚠️ 格式守卫（review §2.1）：未知容器**必须在任何写入动作之前**明确拒绝。
    //   TS 曾经按扩展名盲写，实测把 4KB 假 .m4a 塞成 ID3 头、容器直接损坏。
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    match probe_format(path) {
        Probe::Id3PrefixedReal(_) | Probe::Id3ChainUnresolved => {
            Err(WriteError::BadFormat("无法识别：ID3v2 头之后的内容不是已知音频容器".into()))
        }
        Probe::Plain(Format::Mp3) => write_mp3_tags(path, meta),
        Probe::Plain(Format::Flac) => write_flac_tags(path, meta),
        Probe::Plain(Format::Wav) => write_wav_tags(path, meta),
        Probe::Unknown => {
            // magic 无信时按扩展名兜底（与读侧对称），但未知扩展一律拒绝
            match ext.as_str() {
                "mp3" => write_mp3_tags(path, meta),
                "flac" => write_flac_tags(path, meta),
                "wav" => write_wav_tags(path, meta),
                _ => Err(WriteError::BadFormat("无法识别的文件格式".into())),
            }
        }
    }
}

/// 走路径沙箱的统一写入口（服务端专用）。
/// resolve 在边界做一次，之后内部所有 std::fs 操作都命中已验证的规范路径。
pub fn write_tags_fs(fs: &crate::fs::PathSandbox, path: &Path, meta: &Id3EditMeta) -> Result<(), WriteError> {
    let target = fs.resolve(path)?;
    write_tags(&target, meta)
}
