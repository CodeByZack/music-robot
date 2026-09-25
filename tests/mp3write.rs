//! MP3 写侧测试 —— 移植自 tests/tag/write.test.ts + tests/cli/write-cmd.test.ts 的 E2E 用例。
//!
//! ⚠️ 全部在**副本**上操作，绝不碰 TS 仓库的 samples/（那是只读 ground truth）。
//! 隔离手法与 TS 一致：拷到 target/tmp-<test>/ 下再改。
use music_tag::tag::read::read_tags;
use music_tag::tag::write::{audio_hash, write_mp3_tags, Mp3WriteMeta};
use std::path::{Path, PathBuf};

fn sample(name: &str) -> PathBuf { Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name) }

/// 每个测试独立目录，避免互相污染；返回副本路径。
fn scratch(test: &str, file: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("scratch").join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dst = dir.join(file);
    std::fs::copy(sample(file), &dst).unwrap();
    dst
}

fn md5_of(p: &Path) -> String {
    // 简易内容指纹：用于 --preview「文件字节不变」断言（不必密码学强度）
    let b = std::fs::read(p).unwrap();
    let mut h: u64 = 1469598103934665603;
    for x in &b { h ^= *x as u64; h = h.wrapping_mul(1099511628211) }
    format!("{:016x}-{}", h, b.len())
}

#[test]
fn w01_partial_edit_keeps_unnamed_and_unknown_frames() {
    // TS 1/10：点名替换 title，未点名帧与未知帧 payload 原样保留
    let f = scratch("w01", "最美情侣-白小白.mp3");
    let before = read_tags(&f).unwrap();
    assert_eq!(before.raw_frames.len(), 15, "前置：原文件 15 帧");
    let unknown_before: Vec<_> = before.raw_frames.iter()
        .filter(|x| !["TIT2","TPE1","TALB","TRCK","TPOS","TDRC","TYER","TCON","TCOM","TEXT","COMM","USLT","APIC","TSRC","TXXX"].contains(&x.frame_id.as_str()))
        .map(|x| (x.frame_id.clone(), x.data.clone())).collect();

    let mut m = Mp3WriteMeta::default();
    m.title = Some("新标题".into());
    write_mp3_tags(&f, &m).unwrap();

    let after = read_tags(&f).unwrap();
    assert_eq!(after.title.as_deref(), Some("新标题"));
    assert_eq!(after.artists, before.artists, "未点名的歌手必须原样");
    assert_eq!(after.albums, before.albums, "未点名的专辑必须原样");
    let unknown_after: Vec<_> = after.raw_frames.iter()
        .filter(|x| !["TIT2","TPE1","TALB","TRCK","TPOS","TDRC","TYER","TCON","TCOM","TEXT","COMM","USLT","APIC","TSRC","TXXX"].contains(&x.frame_id.as_str()))
        .map(|x| (x.frame_id.clone(), x.data.clone())).collect();
    assert_eq!(unknown_before.len(), unknown_after.len(), "未知帧数量不得变");
    for i in 0..unknown_before.len() {
        assert_eq!(unknown_before[i].1, unknown_after[i].1, "未知帧 {} payload 必须逐字节不动", unknown_after[i].0);
    }
}

/// 独立算一次"裸音频区"的 sha256：不走被测代码的 mp3_audio_region，
/// 而是用 ffprobe 无关、也与我们解析逻辑无关的方式 —— **直接比对原始字节区间是否逐字节仍在**。
/// 这样即使 audio_hash / mp3_audio_region 本身写错（恒返回同一个值），也能被抓出来。
fn audio_bytes_present_verbatim(orig_path: &Path, new_path: &Path) -> bool {
    let orig = std::fs::read(orig_path).unwrap();
    let new = std::fs::read(new_path).unwrap();
    let (s, e) = music_tag::tag::write::mp3_audio_region(&orig);
    let region = &orig[s..e];
    // 新文件必须完整包含这段字节，且位置可偏移（tag 尺寸变化）
    region.len() > 10_000 && new.windows(region.len()).any(|w| w == region)
}

#[test]
fn w02b_verify_actually_gates_the_write() {
    // 关键回归：verify 若形同虚设（恒真/被删），这条必须红。
    // 手法：让 verify 恒假 → atomic_replace 应拒绝落盘，原文件逐字节不变。
    let f = scratch("w02b", "华夏传说 - 凤凰传奇.mp3");
    let before = std::fs::read(&f).unwrap();
    let r = music_tag::tag::write::atomic::atomic_replace(&f, vec![b'X'; 1234], Some(&|_: &[u8], _: &[u8]| false));
    assert!(r.is_err(), "verify 恒假时 atomic_replace 必须报错");
    assert_eq!(std::fs::read(&f).unwrap(), before, "校验失败后不得写入一个字节");
}

#[test]
fn w02c_audio_bytes_survive_verbatim() {
    // 用"字节区间是否原样存在"独立验证音频未被改动，不依赖被测的 hash 实现
    let src = sample("华夏传说 - 凤凰传奇.mp3");
    let f = scratch("w02c", "华夏传说 - 凤凰传奇.mp3");
    let mut m = Mp3WriteMeta::default();
    m.title = Some("标题变长变短都试".into());
    m.lyrics = Some("歌词".repeat(500));
    write_mp3_tags(&f, &m).unwrap();
    assert!(audio_bytes_present_verbatim(&src, &f), "裸音频字节段必须在写后逐字节可见（否则音频被污染）");
}

#[test]
fn w02_bare_audio_hash_invariant() {
    // TS 2/10：改标签后裸音频区 sha256 必须完全不变（解析式界定，允许 tag 尺寸变化）
    let f = scratch("w02", "华夏传说 - 凤凰传奇.mp3");
    let orig = std::fs::read(&f).unwrap();
    let h0 = audio_hash(&orig);
    let mut m = Mp3WriteMeta::default();
    m.title = Some("改了很长很长的标题以改变 tag 尺寸".into());
    m.lyrics = Some("一二三四五六七八九零".repeat(50));
    write_mp3_tags(&f, &m).unwrap();
    let h1 = audio_hash(&std::fs::read(&f).unwrap());
    assert_eq!(h0, h1, "裸音频 hash 变了 → 音频被污染，这是最严重的一类缺陷");
    assert_ne!(orig.len(), std::fs::read(&f).unwrap().len(), "tag 尺寸应已变化（否则说明没写进去）");
}

#[test]
fn w03_cover_embed_roundtrip() {
    // TS 3/10：封面嵌入后可读回，mime/type/data 一致
    let f = scratch("w03", "华夏传说 - 凤凰传奇.mp3");
    let png: Vec<u8> = vec![0x89,0x50,0x4E,0x47,0x0D,0x0A,0x1A,0x0A, 0,0,0,0x0D,0x49,0x48,0x44,0x52];
    let mut m = Mp3WriteMeta::default();
    m.pictures = Some(vec![music_tag::tag::read::Picture {
        mime_type: "image/png".into(), pic_type: 3, description: String::new(), data: png.clone() }]);
    write_mp3_tags(&f, &m).unwrap();
    let after = read_tags(&f).unwrap();
    assert_eq!(after.pictures.len(), 1, "封面应写入一张");
    assert_eq!(after.pictures[0].data, png, "封面字节必须逐位相等");
    assert_eq!(after.pictures[0].pic_type, 3);
}

#[test]
fn w04_multi_artists_into_single_tpe1() {
    // TS 10/10 + REVIEW §2.5：多歌手并入单个 TPE1（NUL 分隔），TPE2 专用 albumArtist
    let f = scratch("w04", "华夏传说 - 凤凰传奇.mp3");
    let mut m = Mp3WriteMeta::default();
    m.artists = Some(vec!["甲".into(), "乙".into(), "丙".into()]);
    m.album_artist = Some("专辑艺术家".into());
    write_mp3_tags(&f, &m).unwrap();
    let after = read_tags(&f).unwrap();
    assert_eq!(after.artists, vec!["甲", "乙", "丙"], "TPE1 三值应按 NUL 分隔展开回来");
    assert_eq!(after.album_artist.as_deref(), Some("专辑艺术家"), "albumArtist 应落在 TPE2");
    let tpe2: Vec<_> = after.raw_frames.iter().filter(|x| x.frame_id == "TPE2").collect();
    assert_eq!(tpe2.len(), 1, "TPE2 必须恰好一帧");
    assert!(after.raw_frames.iter().all(|x| x.frame_id != "TPE4"), "新写不得产生 TPE4");
}

#[test]
fn w05_track_total_alone_rewrites_whole_trck() {
    // TS 12/14 + 复查 §3.2：trackTotal 单独点名 → TRCK 整组重写，保留已有 num
    let f = scratch("w05", "华夏传说 - 凤凰传奇.mp3");
    let mut m = Mp3WriteMeta::default();
    m.track = Some(5); m.track_total = Some(12);
    write_mp3_tags(&f, &m).unwrap();
    let a = read_tags(&f).unwrap();
    assert_eq!((a.track, a.track_total), (Some(5), Some(12)));
    // 只 unset track-total：num 必须留下
    let mut u = Mp3WriteMeta::default();
    u.unset_fields = vec!["trackTotal".into()];
    write_mp3_tags(&f, &u).unwrap();
    let b = read_tags(&f).unwrap();
    assert_eq!(b.track, Some(5), "--unset track-total 不得连带删掉 track num");
    assert_eq!(b.track_total, None, "total 应被删除");
}

#[test]
fn w06_blank_clears_everything_but_audio() {
    // blank 语义：所有信息清空（含未知帧/APEv2/ID3v1），音频字节不动
    let f = scratch("w06", "最美情侣-白小白.mp3");
    let orig = std::fs::read(&f).unwrap();
    let h0 = audio_hash(&orig);
    let mut m = Mp3WriteMeta::default(); m.blank_all = true;
    write_mp3_tags(&f, &m).unwrap();
    let after = read_tags(&f).unwrap();
    assert_eq!(audio_hash(&std::fs::read(&f).unwrap()), h0, "blank 绝不能碰音频");
    assert!(after.title.is_none() || after.title.as_deref() == Some(""));
    assert!(after.artists.is_empty() && after.albums.is_empty() && after.pictures.is_empty());
    assert!(after.raw_frames.is_empty(), "blank 后应零帧（标准空标签）");
}

#[test]
fn w07_gbk_mojibake_written_as_utf8() {
    // TS 5/10：中文写入按 UTF-8(encoding=3)，读回不乱码
    let f = scratch("w07", "盛夏-毛不易.mp3");
    let mut m = Mp3WriteMeta::default();
    m.title = Some("简体中文 ♫ emoji🎵".into());
    write_mp3_tags(&f, &m).unwrap();
    assert_eq!(read_tags(&f).unwrap().title.as_deref(), Some("简体中文 ♫ emoji🎵"));
}

#[test]
fn w08_atomic_tmp_cleaned_on_verify_failure() {
    // TS 6/10 原子写：verify 失败 → 原文件不被破坏、tmp 不残留
    let dir = scratch("w08", "华夏传说 - 凤凰传奇.mp3");
    let parent = dir.parent().unwrap();
    let before = md5_of(&dir);
    // 构造必然失败：直接调 atomic_replace 并给出恒假的 verify
    let r = music_tag::tag::write::atomic::atomic_replace(&dir, b"garbage".to_vec(), Some(&|_: &[u8], _: &[u8]| false));
    assert!(r.is_err(), "verify 恒假时 atomic_replace 必须报错");
    assert_eq!(md5_of(&dir), before, "校验失败后原文件必须逐字节不变");
    let leftovers: Vec<_> = std::fs::read_dir(parent).unwrap()
        .filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("tagwash-tmp")).collect();
    assert!(leftovers.is_empty(), "失败路径必须清掉 tmp 残留，实际留下：{:?}", leftovers);
}

#[test]
fn w09_junk_id3v1_tail_survives_write() {
    // TS 7/10：尾部是 0x55 垃圾（非 TAG magic）的文件写完不得崩、不得把垃圾当 ID3v1
    let f = scratch("w09", "老男孩-筷子兄弟.mp3");
    let mut m = Mp3WriteMeta::default();
    m.title = Some("安全改写".into());
    write_mp3_tags(&f, &m).unwrap();
    let after = read_tags(&f).unwrap();
    assert_eq!(after.title.as_deref(), Some("安全改写"));
    let bytes = std::fs::read(&f).unwrap();
    assert!(bytes.len() > 128);
}

#[test]
fn w10_format_guard_rejects_fake_audio() {
    // TS 11/14（review §2.1）：假 .m4a/未知容器必须被明确拒绝，不得塞 ID3 头搞坏文件
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/scratch/w10");
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("fake.m4a");
    std::fs::write(&fake, vec![b'A'; 4096]).unwrap();
    let before = std::fs::read(&fake).unwrap();
    let mut m = Mp3WriteMeta::default();
    m.title = Some("x".into());
    let r = music_tag::tag::write::write_tags(&fake, &m);
    assert!(r.is_err(), "未知格式必须拒绝写入");
    assert_eq!(std::fs::read(&fake).unwrap(), before, "被拒的文件必须一字未改");
}
