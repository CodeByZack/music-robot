//! FLAC / WAV 写侧测试 —— 移植自 write.test.ts 4/10、14/14 + write-cmd 附加 FLAC 用例。
//! ⚠️ 全在本项目自己的 fixtures/ 副本上跑，测试自己 cp 到 target/ 再折腾。
use music_robot::tag::read::{flac::parse_flac_metadata, parse_wav_chunks, read_tags};
use music_robot::tag::write::{audio_hash_flac, audio_hash_wav, write_flac_tags, write_wav_tags, Id3EditMeta};
use std::path::{Path, PathBuf};

/// 全部样本在 fixtures/：6 个音乐文件（mp3×5 + flac×1）+ 3 个小 WAV
fn sample(n: &str) -> PathBuf {
    let base = "fixtures";
    Path::new(env!("CARGO_MANIFEST_DIR")).join(base).join(n)
}
fn scratch(t: &str, file: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("scratch").join(t);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dst = dir.join(file);
    std::fs::copy(sample(file), &dst).unwrap();
    dst
}
fn vorbis_keys(buf: &[u8]) -> Vec<String> {
    let blocks = parse_flac_metadata(buf).unwrap_or_default();
    match blocks.iter().find(|b| b.ty == 4) {
        Some(vc) => music_robot::tag::read::flac::parse_vorbis_comment(&vc.payload).0.iter().map(|p| p.key.to_uppercase()).collect(),
        None => vec![],
    }
}

#[test]
fn f01_key_level_edit_keeps_unnamed_keys_and_blocks() {
    // TS 4/10 + flac-writer 头注释：只动点名键；LANGUAGE/LYRICIST/ENCODER 等未点名键保留
    let f = scratch("f01", "牵丝戏 - 白兀.flac");
    let before = std::fs::read(&f).unwrap();
    let keys_before = vorbis_keys(&before);
    assert!(keys_before.iter().any(|k| k == "ENCODER"), "前置：原文件应有未点名的 ENCODER 键，实际 {keys_before:?}");

    let mut m = Id3EditMeta::default();
    m.title = Some("新标题".into());
    write_flac_tags(&f, &m).unwrap();

    let after = std::fs::read(&f).unwrap();
    let keys_after = vorbis_keys(&after);
    for k in &keys_before {
        if k != "TITLE" { assert!(keys_after.contains(k), "未点名的 vorbis 键 {k} 必须保留") }
    }
    assert_eq!(read_tags(&f).unwrap().title.as_deref(), Some("新标题"));
    assert_eq!(read_tags(&f).unwrap().artists, vec!["白兀"], "未点名歌手应原样");
}

#[test]
fn f06_lyrics_alone_reaches_the_file_and_keeps_the_other_keys() {
    // FLAC 里歌词是**单一键**（Vorbis 没有 SYLT 的对应物），所以它一对一映射到 `lyrics`。
    // 这条用例盯两件事：① 只点名 lyrics 真的写进去了；② 未点名的键不受影响。
    let f = scratch("f06", "牵丝戏 - 白兀.flac");
    let keys_before = vorbis_keys(&std::fs::read(&f).unwrap());

    let mut m = Id3EditMeta::default();
    m.lyrics = Some("[00:01.00]第一句\n[00:03.00]第二句".into());
    write_flac_tags(&f, &m).unwrap();

    let meta = read_tags(&f).unwrap();
    assert_eq!(
        meta.lyrics.as_deref(),
        Some("[00:01.00]第一句\n[00:03.00]第二句"),
        "只点名歌词时必须真的写进文件"
    );
    assert!(meta.lyrics_timed.is_none(), "Vorbis 没有同步歌词，不该凭空多出一份");
    assert_eq!(meta.artists, vec!["白兀"], "只改歌词不该动歌手");
    for k in &keys_before {
        if k != "LYRICS" {
            assert!(vorbis_keys(&std::fs::read(&f).unwrap()).contains(k), "未点名的 vorbis 键 {k} 必须保留");
        }
    }
}

#[test]
fn f07_timed_lyrics_rejected_for_flac_without_mutation() {
    // SYLT 在 Vorbis Comment 里没有落脚点 → 必须明确拒绝，不能静默丢弃。
    let f = scratch("f07", "牵丝戏 - 白兀.flac");
    let before = std::fs::read(&f).unwrap();
    let mut m = Id3EditMeta::default();
    m.lyrics_timed = Some("[00:01.00]同步歌词".into());
    assert!(write_flac_tags(&f, &m).is_err(), "FLAC 写同步歌词必须报错");
    assert_eq!(std::fs::read(&f).unwrap(), before, "被拒绝时不得改动文件一个字节");
}

#[test]
fn f02_picture_block_rebuilt_and_audio_intact() {
    let f = scratch("f02", "牵丝戏 - 白兀.flac");
    let orig = std::fs::read(&f).unwrap();
    let h0 = audio_hash_flac(&orig);
    let png: Vec<u8> = vec![0x89,0x50,0x4E,0x47,0x0D,0x0A,0x1A,0x0A, 0,0,0,0x0D];
    let mut m = Id3EditMeta::default();
    m.pictures = Some(vec![music_robot::tag::read::Picture { mime_type: "image/png".into(), pic_type: 3, description: String::new(), data: png.clone() }]);
    write_flac_tags(&f, &m).unwrap();
    let after = std::fs::read(&f).unwrap();
    assert_eq!(audio_hash_flac(&after), h0, "FLAC 裸音频 hash 变了 → 音频被污染");
    let pics = read_tags(&f).unwrap().pictures;
    assert_eq!(pics.len(), 1, "点名 pictures 应替换为一张");
    assert_eq!(pics[0].data, png, "封面字节应逐位一致");
}

#[test]
fn f03_streaminfo_and_other_blocks_preserved_verbatim() {
    // 关键契约（也是 lofty 做不到那条）：SEEKTABLE/PADDING/APPLICATION 等非 4/6 块必须原样保留
    let f = scratch("f03", "牵丝戏 - 白兀.flac");
    let orig = std::fs::read(&f).unwrap();
    let types_before: Vec<u8> = parse_flac_metadata(&orig).unwrap().iter().map(|b| b.ty).collect();
    let si_before = parse_flac_metadata(&orig).unwrap().iter().find(|b| b.ty == 0).unwrap().payload.clone();
    let mut m = Id3EditMeta::default();
    m.comment = Some("改一下注释".into());
    write_flac_tags(&f, &m).unwrap();
    let after = std::fs::read(&f).unwrap();
    let blocks_after = parse_flac_metadata(&after).unwrap();
    assert_eq!(blocks_after.iter().map(|b| b.ty).collect::<Vec<_>>(), types_before, "元数据块类型序列不得改变");
    assert_eq!(blocks_after.iter().find(|b| b.ty == 0).unwrap().payload, si_before, "STREAMINFO 必须逐字节不动");
}

#[test]
fn f04_blank_clears_vorbis_and_pictures() {
    // blank FLAC：vorbis 键清空 + 封面清空，音频不动（TS write-cmd 附加用例）
    let f = scratch("f04", "牵丝戏 - 白兀.flac");
    let orig = std::fs::read(&f).unwrap();
    let h0 = audio_hash_flac(&orig);
    let mut m = Id3EditMeta::default(); m.blank_all = true;
    write_flac_tags(&f, &m).unwrap();
    let after = std::fs::read(&f).unwrap();
    assert_eq!(audio_hash_flac(&after), h0, "blank 不得碰音频");
    let m2 = read_tags(&f).unwrap();
    assert!(m2.title.is_none() || m2.title.as_deref() == Some(""), "blank 后标题应为空");
    assert!(m2.artists.is_empty() && m2.pictures.is_empty(), "blank 后歌手/封面都应清空");
    assert!(vorbis_keys(&after).is_empty(), "blank 后不应残留任何 vorbis 键");
}

#[test]
fn f05_not_flac_rejected_without_mutation() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/scratch/f05");
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("fake.flac");
    std::fs::write(&fake, vec![b'B'; 2048]).unwrap();
    let before = std::fs::read(&fake).unwrap();
    let mut m = Id3EditMeta::default(); m.title = Some("x".into());
    assert!(write_flac_tags(&fake, &m).is_err(), "非法 FLAC 必须报错");
    assert_eq!(std::fs::read(&fake).unwrap(), before, "被拒文件必须一字未改");
}

#[test]
fn w01_wav_list_info_partial_edit() {
    // TS write.test 14/14：WAV title 写入 + 其余 INFO 子项保留
    let f = scratch("w01", "tagged.wav");
    let before = read_tags(&f).unwrap();
    assert_eq!(before.title.as_deref(), Some("测试标题"), "前置 fixture 检查");
    let mut m = Id3EditMeta::default();
    m.title = Some("改过的标题".into());
    write_wav_tags(&f, &m).unwrap();
    let after = read_tags(&f).unwrap();
    assert_eq!(after.title.as_deref(), Some("改过的标题"));
    assert_eq!(after.artists, before.artists, "未点名的 IART 必须保留");
    assert_eq!(after.albums, before.albums, "未点名的 IPRD 必须保留");
    assert_eq!(after.genres, before.genres, "未点名的 IGNR 必须保留");
}

#[test]
fn w02_wav_pcm_data_chunk_untouched() {
    // WAV 完整性 = data chunk 逐位一致（PCM 无压缩，任何改动都可听出）
    let f = scratch("w02", "plain.wav");
    let orig = std::fs::read(&f).unwrap();
    let h0 = audio_hash_wav(&orig);
    let mut m = Id3EditMeta::default();
    m.title = Some("加个标题".into());
    m.lyrics = Some("歌词内容".into());
    write_wav_tags(&f, &m).unwrap();
    assert_eq!(audio_hash_wav(&std::fs::read(&f).unwrap()), h0, "WAV data chunk 必须逐位不变");
    assert_eq!(read_tags(&f).unwrap().title.as_deref(), Some("加个标题"));
}

#[test]
fn w03_wav_riff_size_consistent_after_edit() {
    // RIFF size 字段必须重算：漏算会让部分播放器按旧长度截断读取。
    // ⚠️ 用 comment（ICMT）而非 lyrics —— WAV 的 LIST INFO 没有歌词槽位，
    //   传 lyrics 会被静默丢弃、文件尺寸不变，于是"陈旧 size == 正确 size"，断言失去意义
    //   （第一版就踩了这个空壳断言，注入陈旧 size 后仍绿）。
    let f = scratch("w03", "tagged.wav");
    let mut m = Id3EditMeta::default();
    m.comment = Some("很长的备注以确实改变文件尺寸".repeat(60));
    write_wav_tags(&f, &m).unwrap();
    let b = std::fs::read(&f).unwrap();
    let orig = std::fs::read(sample("tagged.wav")).unwrap();
    assert_ne!(b.len(), orig.len(), "前置：本次写入必须真的改变文件尺寸，否则本测试无意义");
    let riff_size = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
    assert_eq!(riff_size + 8, b.len(), "RIFF size 应等于总长-8（未重算则为陈旧值）");
    assert!(!parse_wav_chunks(&b, 12, (riff_size + 8).min(b.len())).is_empty(), "chunk 树仍可解析");
}

#[test]
fn w04_not_wav_rejected_without_mutation() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/scratch/w04");
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("fake.wav");
    std::fs::write(&fake, vec![b'C'; 2048]).unwrap();
    let before = std::fs::read(&fake).unwrap();
    let mut m = Id3EditMeta::default(); m.title = Some("x".into());
    assert!(write_wav_tags(&fake, &m).is_err(), "非法 WAV 必须报错");
    assert_eq!(std::fs::read(&fake).unwrap(), before, "被拒文件必须一字未改");
}
