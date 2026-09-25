//! 读后体检（warnings）与结构化 JSON 视图 —— 移植自 src/tag/read/warnings.ts
//! 归属：能力层（Web 直接 import）。
use crate::tag::read::gbk_sniff::gbk_sniff;
use crate::tag::read::id3v2::{decode_txxx, decode_uslt};
use crate::tag::read::metadata::AudioMetadata;

const AD_PATTERNS: &[&str] = &["音乐下载网站", "公众号", "yym4", "www.", "关注下载", "无损下载", "资源下载"];

#[derive(Debug, Clone)]
pub struct Warning { pub code: String, pub message: String }

/// 读取侧白名单（≈写入侧 buildId3v2Frames + 常见标准帧）
pub const READ_WHITELIST: &[&str] = &[
    "TIT2", "TPE1", "TPE2", "TPE3", "TPE4", "TALB", "TRCK", "TPOS", "TDRC", "TYER", "TDOR",
    "TCON", "TCOM", "TEXT", "TPUB", "COMM", "USLT", "SYLT", "APIC", "TXXX", "TSRC",
    "PCST", "TCMP", "WOAR", "WXXX", "MVNM", "MVIN", "GRP1", "IPLS", "TMCL", "TIPL",
];

fn latin1_bytes(s: &str) -> Vec<u8> { s.chars().map(|c| c as u8).collect() }

/// 体检：广告词 / GBK 乱码 / ID3v1 垃圾尾 / 非白名单帧
pub fn warnings_for(meta: &AudioMetadata, buf: &[u8]) -> Vec<Warning> {
    let mut warnings = Vec::new();

    // 1. 广告词
    let texts: [(&str, &str); 4] = [
        ("artist", &meta.artists.join(" / ")),
        ("album", &meta.albums.join(" / ")),
        ("albumArtist", meta.album_artist.as_deref().unwrap_or("")),
        ("comment", meta.comment.as_deref().unwrap_or("")),
    ];
    for (field, text) in texts.iter() {
        if text.is_empty() { continue }
        if let Some(hit) = AD_PATTERNS.iter().find(|p| text.contains(**p)) {
            warnings.push(Warning { code: "ad-words".into(), message: format!("{field} 疑似广告文案（含\"{hit}\"）") });
        }
    }

    // 2. GBK 乱码（COMM/TXXX/USLT 文本高字节占比 >30%）
    // ⚠️ 判据：只有 text 全部落在 latin1 范围（enc=0 的 latin1 展开形态）才嗅探字节形态。
    //    正常 UTF-8/UTF-16 中文的字符码位 >0xFF，绝不误报（勿改成字符码位占比判据）。
    for f in &meta.raw_frames {
        let text = if f.frame_id == "COMM" || f.frame_id == "USLT" {
            decode_uslt(&f.data).map(|l| l.text)
        } else if f.frame_id == "TXXX" {
            decode_txxx(&f.data).map(|t| t.value)
        } else { None };
        let Some(text) = text.filter(|t| !t.is_empty()) else { continue };
        let all_latin1 = text.chars().all(|c| (c as u32) <= 0xff);
        if all_latin1 {
            let sniff = gbk_sniff(&latin1_bytes(&text));
            if sniff.looks_gbk {
                warnings.push(Warning {
                    code: "gbk-mojibake".into(),
                    message: format!("{} 疑似 GBK 乱码（高字节占比 {:.0}%）", f.frame_id, sniff.high_byte_ratio * 100.0),
                });
            }
        }
    }

    // 3. ID3v1 垃圾尾（0x55/0xAA 均匀填充，非 TAG magic）
    if meta.source == "id3v2" || meta.source == "id3v1" {
        if buf.len() >= 128 {
            let tail = &buf[buf.len() - 128..];
            if &tail[..4] != b"TAG" && tail.iter().any(|&b| b != 0) {
                let z55 = tail.iter().filter(|&&b| b == 0x55).count();
                let zaa = tail.iter().filter(|&&b| b == 0xAA).count();
                if z55 as f64 / 128.0 >= 0.9 || zaa as f64 / 128.0 >= 0.9 {
                    warnings.push(Warning {
                        code: "id3v1-garbage".into(),
                        message: "尾部 128B 是 0x55/0xAA 垃圾（非标准 ID3v1，track 等字段不可信）".into(),
                    });
                }
            }
        }
    }

    // 4. 非白名单帧
    let unknown: Vec<&str> = meta.raw_frames.iter().map(|f| f.frame_id.as_str()).filter(|f| !READ_WHITELIST.contains(f)).collect();
    if !unknown.is_empty() {
        warnings.push(Warning {
            code: "unknown-frames".into(),
            message: format!("{} 个非白名单帧（write 保留，仅 blank 会移除）：{}", unknown.len(), unknown.join(", ")),
        });
    }
    warnings
}

/// read 的 JSON 视图（sanitize：不带 Buffer 原样倾倒）
pub fn read_json_value(meta: &AudioMetadata, file: &str, warnings: &[Warning]) -> serde_json::Value {
    use serde_json::{json, Value};
    let mbids: Vec<(String, Option<String>)> = vec![
        ("artistId".into(), meta.musicbrainz_artist_id.clone()),
        ("releaseId".into(), meta.musicbrainz_release_id.clone()),
        ("trackId".into(), meta.musicbrainz_track_id.clone()),
        ("releaseGroupId".into(), meta.musicbrainz_release_group_id.clone()),
        ("discId".into(), meta.musicbrainz_disc_id.clone()),
    ];
    let has_mbid = mbids.iter().any(|(_, v)| v.is_some());
    let mbid = if has_mbid {
        Value::Object(mbids.into_iter().filter(|(_, v)| v.is_some()).map(|(k, v)| (k, v.into())).collect())
    } else { Value::Null };

    json!({
        "file": file,
        "source": meta.source,
        "mbid": mbid,
        "title": meta.title,
        "artists": meta.artists,
        "albums": meta.albums,
        "albumArtist": meta.album_artist,
        "track": meta.track,
        "trackTotal": meta.track_total,
        "disc": meta.disc,
        "discTotal": meta.disc_total,
        "year": meta.year,
        "genres": meta.genres,
        "composers": meta.composers,
        "durationMs": meta.duration_ms,
        "sampleRate": meta.sample_rate,
        "bitsPerSample": meta.bits_per_sample,
        "bitrateBps": meta.bitrate_bps,
        "lyrics": meta.lyrics,
        "lyricsTimed": meta.lyrics_timed,
        "comment": meta.comment,
        "pictures": meta.pictures.iter().map(|p| json!({
            "mimeType": p.mime_type, "type": p.pic_type,
            "description": p.description, "dataLength": p.data.len()
        })).collect::<Vec<_>>(),
        "rawFrames": meta.raw_frames.iter().map(|f| json!({ "frameId": f.frame_id, "size": f.size })).collect::<Vec<_>>(),
        "warnings": warnings.iter().map(|w| json!({ "code": w.code, "message": w.message })).collect::<Vec<_>>(),
    })
}

pub fn read_json(meta: &AudioMetadata, file: &str, warnings: &[Warning]) -> String {
    read_json_value(meta, file, warnings).to_string()
}
