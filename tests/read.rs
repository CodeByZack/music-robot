//! 读取层测试 —— 移植自 tagwash-test/tests/tag/read.test.ts（14 用例）
//!
//! 迁移原则：**断言逐条照搬，不改期望值**。TS 侧每条断言都已按真实样本字节校准过
//! （见下方注释），若 Rust 实现与期望不符，错的是实现，不是测试。
use music_tag::tag::read::{gbk_sniff, id3v1_parse, read_tags};

/// fixtures/ 是指向 TS 仓库 samples/ 的符号链接，避免复制 65MB 音频。
fn fixture(name: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name);
    assert!(p.exists(), "fixture 不存在：{}", p.display());
    p
}

struct Sample { name: &'static str, file: &'static str, duration_s: f64, has_cover: bool }

/// Ground truth：时长取 ffprobe 精确值（与 readTags 内部 round(sec*1000) 完全一致）
const SAMPLES: &[Sample] = &[
    Sample { name: "华夏传说", file: "华夏传说 - 凤凰传奇.mp3", duration_s: 216.75,      has_cover: false },
    Sample { name: "最美情侣", file: "最美情侣-白小白.mp3",     duration_s: 241.6,       has_cover: true },
    Sample { name: "牵丝戏",   file: "牵丝戏 - 白兀.flac",      duration_s: 237.870952,  has_cover: true },
    Sample { name: "盛夏",     file: "盛夏-毛不易.mp3",         duration_s: 292.246077,  has_cover: true },
    Sample { name: "老男孩",   file: "老男孩-筷子兄弟.mp3",     duration_s: 290.44,      has_cover: true },
    Sample { name: "Havana",   file: "Havana-Camila Cabello&YoungThug-大耳兽莫慢待.mp3", duration_s: 217.306667, has_cover: true },
];

#[test]
fn t01_basic_read() {
    for s in SAMPLES {
        let m = read_tags(&fixture(s.file)).unwrap_or_else(|e| panic!("{} 读取失败：{}", s.name, e));
        assert!(matches!(m.source.as_str(), "id3v2" | "vorbis" | "riff"), "{} source 应为 id3v2/vorbis/riff，实际 {}", s.name, m.source);
        assert!(!m.title.clone().unwrap_or_default().is_empty(), "{} 标题非空", s.name);
        assert!(m.artists.len() >= 1, "{} 至少一个歌手", s.name);
        assert!(m.albums.len() >= 1, "{} 至少一个专辑", s.name);
        // 与 TS 一致：Math.round(seconds * 1000)
        assert_eq!(m.duration_ms, (s.duration_s * 1000.0).round() as i64, "{} 时长不符", s.name);
        assert_eq!(m.pictures.len(), if s.has_cover { 1 } else { 0 }, "{} 封面数量不符", s.name);
    }
}

#[test]
fn t02_pollution_preserved_verbatim() {
    let hx = read_tags(&fixture("华夏传说 - 凤凰传奇.mp3")).unwrap();
    assert!(hx.artists[0].contains("音乐下载网站 yym4.com"), "污染字符串应原样保留，清洗由 wash 命令负责");
    let mc = read_tags(&fixture("最美情侣-白小白.mp3")).unwrap();
    assert!(mc.raw_frames.iter().any(|f| f.frame_id == "TXXX"
              && String::from_utf8_lossy(&f.data).contains("ALBUM ARTIST")),
            "TXXX ALBUM ARTIST 应在 rawFrames 里");
}

#[test]
fn t03_gbk_mojibake_detection() {
    let sm = read_tags(&fixture("盛夏-毛不易.mp3")).unwrap();
    let comm = sm.raw_frames.iter().find(|f| f.frame_id == "COMM"
                 && String::from_utf8_lossy(&f.data).contains("ID3v1 Comment"))
        .expect("应存在描述为 \"ID3v1 Comment\" 的 COMM 帧");
    // COMM 结构：enc(1B) + lang(3B) + desc(NUL 结尾) + text
    let d = &comm.data;
    assert_eq!(d[0], 0, "encoding 应为 Latin1(0)");
    let mut q = 4usize;
    while q < d.len() && d[q] != 0 { q += 1 }
    let desc = latin1(&d[4..q]);
    assert!(desc.contains("ID3v1 Comment"), "COMM description 应为 ID3v1 Comment");
    let text = trim_nuls(&latin1(&d[q + 1..]));
    assert!(!text.is_empty(), "ID3v1 Comment 内容应存在");
    // GBK 嗅探：高字节(0xC0~0xFF)占比 > 30%
    let high = d[q + 1..].iter().filter(|b| **b >= 0xC0).count();
    let total = d[q + 1..].len();
    assert!(total > 0 && high as f64 / total as f64 > 0.3, "GBK 乱码应被识别（高字节占比 >30%）");
    let sniff = gbk_sniff(&d[q + 1..]);
    assert!(sniff.looks_gbk, "gbkSniff 应返回 GBK 解码候选");
    // 原 TS 断言：GBK 解码应包含「音乐」——现解码器已就位，恢复完整断言
    let decoded = sniff.gbk.expect("GB18030 解码应有结果");
    assert!(decoded.contains("音乐"), "GBK 解码应包含\"音乐\"，实际：{decoded}");
    // parseID3v1 对合法/垃圾尾部的判定
    let hx_bytes = std::fs::read(fixture("华夏传说 - 凤凰传奇.mp3")).unwrap();
    let tail = &hx_bytes[hx_bytes.len() - 128..];
    assert!(id3v1_parse(tail).is_some(), "华夏传说尾部是合法 ID3v1");
    let sm_bytes = std::fs::read(fixture("盛夏-毛不易.mp3")).unwrap();
    let tail = &sm_bytes[sm_bytes.len() - 128..];
    assert!(id3v1_parse(tail).is_none(), "盛夏尾部是 0x55 垃圾，无 TAG magic");
}

#[test]
fn t04_id3v1_garbage_does_not_crash() {
    let lb = read_tags(&fixture("老男孩-筷子兄弟.mp3")).unwrap();
    assert!(lb.track.is_none(), "垃圾 ID3v1 track 字段应解析为 None，不崩溃");
    let bytes = std::fs::read(fixture("老男孩-筷子兄弟.mp3")).unwrap();
    assert!(id3v1_parse(&bytes[bytes.len() - 128..]).is_none(), "老男孩尾部 128B 全 0x55，不应识别为 ID3v1");
}

#[test]
fn t05_flac_vorbis_comment_full() {
    let q = read_tags(&fixture("牵丝戏 - 白兀.flac")).unwrap();
    assert_eq!(q.title.as_deref(), Some("牵丝戏"));
    assert_eq!(q.artists[0], "白兀");
    assert_eq!(q.albums[0], "T"); // 原样保留，不 trim
    assert!(q.lyrics_timed.as_deref().unwrap_or("").contains("嘲笑谁恃美扬威"), "FLAC lyrics 应完整");
    assert_eq!(q.pictures.len(), 1, "FLAC PICTURE block 应被读出");
    assert_eq!(q.pictures[0].pic_type, 3, "FrontCover type = 3");
    assert!(q.pictures[0].data.len() > 100_000, "封面数据应 >100KB");
    assert_eq!(q.sample_rate, 44100);
    assert_eq!(q.bits_per_sample, Some(24));
}

#[test]
fn t06_id3v2_frame_counts() {
    let hx = read_tags(&fixture("华夏传说 - 凤凰传奇.mp3")).unwrap();
    assert_eq!(hx.raw_frames.len(), 4, "华夏传说只有 4 个标准帧");
    let mc = read_tags(&fixture("最美情侣-白小白.mp3")).unwrap();
    assert_eq!(mc.raw_frames.len(), 15, "最美情侣有 15 个帧");
}

#[test]
fn t07_apic_picture_integrity() {
    let mc = read_tags(&fixture("最美情侣-白小白.mp3")).unwrap();
    assert_eq!(mc.pictures.len(), 1);
    let pic = &mc.pictures[0];
    assert_eq!(pic.mime_type, "image/jpeg");
    assert_eq!(pic.pic_type, 3, "APIC type 字节为 3（ID3 规范 FrontCover=3）");
    assert_eq!(pic.data.len(), 244_880, "图片数据 244880B（帧 244894 - 14B APIC 头）");
    assert_eq!(pic.data[0], 0xFF, "JPEG SOI 高字节");
    assert_eq!(pic.data[1], 0xD8, "JPEG SOI marker");
}

#[test]
fn t08_uslt_lyrics_present() {
    let mc = read_tags(&fixture("最美情侣-白小白.mp3")).unwrap();
    let uslt = mc.raw_frames.iter().find(|f| f.frame_id == "USLT").expect("应有 USLT 帧");
    assert!(uslt.data.len() > 2000, "歌词数据应 >2000 bytes");
}

#[test]
fn t09_txxx_custom_frame() {
    let mc = read_tags(&fixture("最美情侣-白小白.mp3")).unwrap();
    let txxxs: Vec<_> = mc.raw_frames.iter().filter(|f| f.frame_id == "TXXX").collect();
    assert_eq!(txxxs.len(), 1, "最美情侣只有 1 个 TXXX 帧");
    assert!(String::from_utf8_lossy(&txxxs[0].data).contains("ALBUM ARTIST"), "TXXX key 应包含 ALBUM ARTIST");
}

#[test]
fn t10_mp3_without_tags() {
    // TS 侧用 ffmpeg 现场生成无标签文件；此处同样依赖 ffmpeg，缺失则显式跳过而非假通过。
    if which("ffmpeg").is_none() { eprintln!("skip: 无 ffmpeg"); return }
    let tmp = std::env::temp_dir().join("rust-music-tag-notag.mp3");
    let _ = std::fs::remove_file(&tmp);
    let st = std::process::Command::new("ffmpeg")
        .args(["-y", "-i", fixture("华夏传说 - 凤凰传奇.mp3").to_str().unwrap(),
               "-c", "copy", "-map_metadata", "-1", tmp.to_str().unwrap()])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().unwrap();
    assert!(st.success(), "ffmpeg 剥标签失败");
    let meta = read_tags(&tmp).unwrap();
    assert!(meta.title.clone().unwrap_or_default().is_empty(), "无标签文件 title 应为空（无 ID3v2），但不报错");
    assert_eq!(meta.artists.len(), 0, "无 TPE1");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn t11_magic_first_flac_renamed_as_mp3() {
    let tmp = std::env::temp_dir().join("rust-music-tag-magic-flac.mp3");
    std::fs::copy(fixture("牵丝戏 - 白兀.flac"), &tmp).unwrap();
    let meta = read_tags(&tmp).unwrap();
    assert_eq!(meta.title.as_deref(), Some("牵丝戏"), "扩展名说谎也应读出真实格式的标签");
    assert_eq!(meta.artists[0], "白兀");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn t12_decode_uslt_enc2_utf16be_no_bom() {
    use music_tag::tag::read::id3v2::decode_uslt;
    // enc=2 定义即无 BOM 的 UTF-16BE：desc='测试' text='歌词'
    let mut body: Vec<u8> = vec![2];
    body.extend_from_slice(b"eng");
    body.extend_from_slice(&[0x6d, 0x4b, 0x8b, 0xd5]); // 测试 (BE)
    body.extend_from_slice(&[0, 0]);                     // BE 终止符
    body.extend_from_slice(&[0x6b, 0x4c, 0x8b, 0xcd]);  // 歌词 (BE)
    let r = decode_uslt(&body).expect("应解析成功");
    assert_eq!(r.description, "测试");
    assert_eq!(r.text, "歌词");
}

#[test]
fn t13_decode_text_odd_length_utf16be_no_panic() {
    use music_tag::tag::read::id3v2::decode_text;
    // BE 分支要求偶数长：畸形截断帧不得 panic 炸掉整文件读取
    let _: String = decode_text(&[2, 0x4e]);                  // BE 1B
    let _: String = decode_text(&[2, 0x4e, 0x00, 0x4f]);      // BE 3B
    let _: String = decode_text(&[1, 0xff, 0xfe]);            // BOM 截断
}

#[test]
fn t14_id3_prefix_with_unknown_container_rejected() {
    // OggS 容器必须拒绝，不得因 .mp3 扩展名走 mp3 分支
    let tmp = std::env::temp_dir().join("rust-music-tag-id3-ogg.mp3");
    let mut v: Vec<u8> = vec![0; 10];
    v[..3].copy_from_slice(b"ID3"); v[3] = 4; v[4] = 0; v[5] = 0; // tagSize = 0
    v.extend_from_slice(b"OggS");
    v.extend_from_slice(&[b'x'; 32]);
    std::fs::write(&tmp, &v).unwrap();
    let err = read_tags(&tmp).expect_err("OggS 容器必须被拒绝");
    assert!(err.message().contains("无法识别"), "错误文案应含「无法识别」，实际：{}", err);
    let _ = std::fs::remove_file(&tmp);
}

// ---- helpers（与 TS Buffer.toString('latin1') 语义等价）----
fn latin1(b: &[u8]) -> String { b.iter().map(|c| *c as u16).collect::<Vec<u16>>().iter().map(|u| char::from_u32(*u as u32).unwrap_or('\u{fffd}')).collect() }
fn trim_nuls(s: &str) -> String { s.trim_end_matches('\u{0}').to_string() }
fn which(bin: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths)
        .map(|d| d.join(bin)).find(|p| p.is_file()))
}

#[test]
fn id3v1_genre_always_at_127_not_padding() {
    // 回归：ID3v1 genre 恒在 offset 127。旧实现非 v1.1 时错读 126（padding 0x00）
    // → genre=0 被当成 "Blues"，blank 后 genres 残留。TS 参照实现同样有此 bug。
    // 变异点：把 `tag[127]` 改回 `if track_present { tag[127] } else { tag[126] }` → 本用例必 RED。
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/华夏传说 - 凤凰传奇.mp3");
    let dst = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/id3v1-genre.mp3");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(&src, &dst).unwrap();
    music_tag::tag::write::write_tags(&dst, &music_tag::tag::write::WriteMeta { blank_all: true, ..Default::default() }).unwrap();

    let back = music_tag::tag::read::read_tags(&dst).unwrap();
    assert!(back.genres.is_empty(), "blank 后 genres 必须为空，实际 {:?}（ID3v1 genre 字节错读）", back.genres);
    assert!(back.composers.is_empty());
    let leftover = music_tag::cli::wash::blank_leftovers(&back);
    assert!(leftover.is_empty(), "blank_leftovers 必须干净：{leftover:?}");
    let _ = std::fs::remove_file(&dst);
}
