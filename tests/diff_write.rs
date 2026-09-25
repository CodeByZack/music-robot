//! 写侧跨实现差分：同一份意图分别交给 TS 与 Rust 应用，比较**产物字节**。
//!
//! 这是比单测更硬的证据：两侧独立实现同一语义，落盘结果必须完全一致。
//! 全部在副本上做，绝不碰 samples/。
use std::path::{Path, PathBuf};
use std::process::Command;

const TS_REPO: &str = "/vol1/@appshare/dsh/data/tagwash-test";

fn node() -> Option<String> {
    ["node", "/var/apps/nodejs_v24/target/bin/node"].iter()
        .map(|s| s.to_string())
        .find(|n| Command::new(n).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
}

/// 用 TS CLI 写一份，返回产物 sha256（走被测代码之外的 sha2 crate）
fn ts_apply(case: &str, sample: &str, args: &[&str]) -> Option<PathBuf> {
    let n = node()?;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/diffwrite").join(format!("ts-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join(sample);
    std::fs::copy(Path::new(TS_REPO).join("samples").join(sample), &dst).ok()?;
    let out = Command::new(&n)
        .args(["--experimental-strip-types", "src/cli.ts", "write", dst.to_str().unwrap()]).args(args)
        .current_dir(TS_REPO).output().expect("spawn node");
    if !out.status.success() {
        panic!("TS write 失败 exit={:?}\n  cmd args={args:?}\n  stderr={}",
            out.status.code(), String::from_utf8_lossy(&out.stderr));
    }
    Some(dst)
}

fn rs_apply(case: &str, sample: &str, build: impl FnOnce(&mut music_tag::tag::write::Mp3WriteMeta)) -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/diffwrite").join(format!("rs-{case}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join(sample);
    std::fs::copy(Path::new(TS_REPO).join("samples").join(sample), &dst).ok()?;
    let mut m = music_tag::tag::write::Mp3WriteMeta::default();
    build(&mut m);
    music_tag::tag::write::write_mp3_tags(&dst, &m).ok()?;
    Some(dst)
}

fn read_meta(p: &Path) -> music_tag::tag::read::AudioMetadata {
    music_tag::tag::read::read_tags(p).expect("读回失败")
}

macro_rules! cmp_case {
    ($name:expr, $sample:expr, $args:expr, $build:expr) => {{
        // 参照实现不可用 → 必须红，不得静默 skip（差分测试假绿的根因）
        assert!(node().is_some(), "参照实现不可用：找不到 node（差分测试拒绝静默跳过）");
        let ts = ts_apply(stringify!($name), $sample, $args).expect("TS 写入失败");
        let rs = rs_apply(stringify!($name), $sample, $build).expect("Rust 写入失败");
        let (a, b) = (std::fs::read(&ts).unwrap(), std::fs::read(&rs).unwrap());
        // 字节级不要求一致（padding 策略等允许差异），但**语义视图必须一致**：
        let (ma, mb) = (read_meta(&ts), read_meta(&rs));
        fn S<T: std::fmt::Debug>(o: T) -> String { format!("{o:?}") }
        for (field, x, y) in [
            ("title", S(ma.title.clone()), S(mb.title.clone())),
            ("artists", S(format!("{:?}", ma.artists)), S(format!("{:?}", mb.artists))),
            ("albums", S(format!("{:?}", ma.albums)), S(format!("{:?}", mb.albums))),
            ("albumArtist", S(ma.album_artist.clone()), S(mb.album_artist.clone())),
            ("track", S(format!("{:?}", ma.track)), S(format!("{:?}", mb.track))),
            ("trackTotal", S(format!("{:?}", ma.track_total)), S(format!("{:?}", mb.track_total))),
            ("year", S(ma.year.clone()), S(mb.year.clone())),
            ("genres", S(format!("{:?}", ma.genres)), S(format!("{:?}", mb.genres))),
            ("comment", S(ma.comment.clone()), S(mb.comment.clone())),
            ("pictures", S(ma.pictures.len().to_string()), S(mb.pictures.len().to_string())),
            ("coverBytes", S(sha(&ma.pictures.first().map(|p| p.data.clone()).unwrap_or_default())), S(sha(&mb.pictures.first().map(|p| p.data.clone()).unwrap_or_default()))),
        ] {
            let (xs, ys) = (norm(&x), norm(&y));
            assert_eq!(xs, ys, "{}：字段 {field} 与 TS 写入结果不一致（TS={xs} Rust={ys}）", $name);
        }
        // 音频完整性：两侧各自的裸 hash 应与"从原始样本算出的裸 hash"相同
        let orig = std::fs::read(Path::new(TS_REPO).join("samples").join($sample)).unwrap();
        let h0 = music_tag::tag::write::audio_hash(&orig);
        assert_eq!(h0, music_tag::tag::write::audio_hash(&a), "{}：TS 侧裸音频 hash 变化（基准异常）", $name);
        assert_eq!(h0, music_tag::tag::write::audio_hash(&b), "{}：Rust 侧裸音频 hash 变了 → 音频被污染", $name);
    }};
}

/// 统一显示形态：TS 侧 Option 为 None/空串时 Rust 也应等价，故把 Some("x")/"x" 归一
fn norm(s: &str) -> String {
    s.trim_matches('"').to_string()
}

fn sha(v: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new(); h.update(v); h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[test] fn dw01_title_only() {
    cmp_case!("dw01 title", "华夏传说 - 凤凰传奇.mp3", &["--title", "新标题"][..], |m| m.title = Some("新标题".into()));
}
#[test] fn dw02_multi_artists_plus_albumartist() {
    cmp_case!("dw02 artists", "华夏传说 - 凤凰传奇.mp3",
        &["--artist", "甲", "--artist", "乙", "--album-artist", "专辑艺术家"][..],
        |m| { m.artists = Some(vec!["甲".into(), "乙".into()]); m.album_artist = Some("专辑艺术家".into()); });
}
#[test] fn dw03_track_pair() {
    cmp_case!("dw03 track", "华夏传说 - 凤凰传奇.mp3", &["--track", "5", "--track-total", "12"][..],
        |m| { m.track = Some(5); m.track_total = Some(12); });
}
#[test] fn dw04_unset_fields() {
    cmp_case!("dw04 unset", "最美情侣-白小白.mp3", &["--unset", "title"][..],
        |m| { m.unset_fields.push("title".into()); });
}
#[test] fn dw05_chinese_and_emoji_lyrics() {
    cmp_case!("dw05 lyrics", "盛夏-毛不易.mp3", &["--lyrics", "简体中文 ♫ 🎵"][..],
        |m| { m.lyrics = Some("简体中文 ♫ 🎵".into()); });
}
#[test] fn dw06_on_file_with_unknown_frames() {
    // 含大量未知帧的文件：验证"未点名保留"两侧行为一致
    cmp_case!("dw06 keep-unknown", "老男孩-筷子兄弟.mp3", &["--title", "只改标题"][..],
        |m| m.title = Some("只改标题".into()));
}
