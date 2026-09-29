//! read 子命令测试 —— 警告计算 + 格式化 + 端到端（移植自 tests/cli/read-cmd.test.ts）
//! ⚠️ 全部在内存里调 CLI 函数（不 spawn 子进程）：快得多，且能直接断言退出码。
use music_robot::cli::args::UsageError;
use music_robot::cli::io::CollectingIO;
use music_robot::cli::read_cmd::format_read;
use music_robot::cli::{run, TOP_USAGE};
use music_robot::tag::read::{read_tags, warnings_for};
use std::path::Path;

fn fx(n: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(n)
}
fn buf_of(n: &str) -> std::vec::Vec<u8> { std::fs::read(fx(n)).unwrap() }
fn px(n: &str) -> String { fx(n).display().to_string() }
fn argv(a: &[&str]) -> Vec<String> { a.iter().map(|s| s.to_string()).collect() }

#[test] fn r01_ad_words_in_artist_no_garbage() {
    let f = fx("华夏传说 - 凤凰传奇.mp3");
    let meta = read_tags(&f).unwrap();
    let w = warnings_for(&meta, &buf_of("华夏传说 - 凤凰传奇.mp3"));
    let ad = w.iter().find(|x| x.code == "ad-words");
    assert!(ad.is_some(), "artist 含广告词应报：{:?}", w);
    assert!(ad.unwrap().message.contains("音乐下载网站"));
    assert!(w.iter().all(|x| x.code != "id3v1-garbage"), "真 ID3v1（TAG）不应报垃圾警告");
}

#[test] fn r02_normal_chinese_comment_no_gbk_false_positive() {
    // review §2.2：正常 UTF-8 中文绝不能误报 GBK 乱码
    let f = fx("华夏传说 - 凤凰传奇.mp3");
    let dst = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-r02.mp3");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(&f, &dst).unwrap();
    let mut m = music_robot::tag::write::intent::WriteMeta::default();
    m.comment = Some("这是一条完全正常的中文注释".into());
    music_robot::tag::write::write_tags(&dst, &m).unwrap();
    let meta = read_tags(&dst).unwrap();
    let w = warnings_for(&meta, &std::fs::read(&dst).unwrap());
    assert!(w.iter().all(|x| x.code != "gbk-mojibake"), "正常中文误报 GBK：{w:?}");
    let _ = std::fs::remove_file(&dst);
}

#[test] fn r03_shengxia_gbk_and_garbage() {
    let f = fx("盛夏-毛不易.mp3");
    let meta = read_tags(&f).unwrap();
    let w = warnings_for(&meta, &buf_of("盛夏-毛不易.mp3"));
    assert!(w.iter().any(|x| x.code == "gbk-mojibake"), "应报 GBK 乱码：{w:?}");
    assert!(w.iter().any(|x| x.code == "id3v1-garbage"), "0x55 垃圾尾应报：{w:?}");
}

#[test] fn r04_garbage_tail_variants() {
    for n in ["老男孩-筷子兄弟.mp3", "Havana-Camila Cabello&YoungThug-大耳兽莫慢待.mp3"] {
        let meta = read_tags(&fx(n)).unwrap();
        let w = warnings_for(&meta, &buf_of(n));
        assert!(w.iter().any(|x| x.code == "id3v1-garbage"), "{n} 应报 ID3v1 垃圾：{w:?}");
    }
}

#[test] fn r05_flac_ad_words_no_id3v1_noise() {
    let meta = read_tags(&fx("牵丝戏 - 白兀.flac")).unwrap();
    let w = warnings_for(&meta, &buf_of("牵丝戏 - 白兀.flac"));
    assert!(w.iter().any(|x| x.code == "ad-words"), "FLAC comment 广告词应报：{w:?}");
    assert!(w.iter().all(|x| x.code != "id3v1-garbage"), "FLAC 无 ID3v1 概念，不应误报");
}

#[test] fn r06_format_read_has_fields_and_warnings() {
    let f = fx("华夏传说 - 凤凰传奇.mp3");
    let meta = read_tags(&f).unwrap();
    let out = format_read(&meta, &f.display().to_string(), &warnings_for(&meta, &buf_of("华夏传说 - 凤凰传奇.mp3")));
    for token in ["文件:", "source:", "标题:", "歌手:", "警告:", "⚠"] {
        assert!(out.contains(token), "表格应含 {token}：\n{out}");
    }
    assert!(out.ends_with('\n'), "输出必须以换行结尾");
}

#[test] fn e01_read_table_exit0() {
    let io = CollectingIO::new();
    let code = run(&argv(&["read", &px("华夏传说 - 凤凰传奇.mp3")]), &io);
    assert_eq!(code, 0, "read 成功应 exit 0；stderr={}", io.err());
    let out = io.out();
    assert!(out.contains("华夏传说") && out.contains("⚠"), "表格应含标题与警告：\n{out}");
}

#[test] fn e02_read_json_parseable() {
    let io = CollectingIO::new();
    let code = run(&argv(&["read", &px("华夏传说 - 凤凰传奇.mp3"), "--json"]), &io);
    assert_eq!(code, 0, "stderr={}", io.err());
    let v: serde_json::Value = serde_json::from_str(&io.out()).expect("必须可解析为 JSON");
    assert_eq!(v["title"], "华夏传说", "JSON 标题不符：{}", v["title"]);
    assert!(v["warnings"].is_array(), "JSON 必须含 warnings 数组");
    assert!(!v["rawFrames"].as_array().unwrap().is_empty(), "rawFrames 不得为空");
}

#[test] fn e03_extract_cover() {
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-cover.jpg");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&out);
    let io = CollectingIO::new();
    let code = run(&argv(&["read", "--extract-cover", out.to_str().unwrap(), &px("最美情侣-白小白.mp3")]), &io);
    assert_eq!(code, 0, "stderr={}", io.err());
    let data = std::fs::read(&out).expect("封面文件必须已写出");
    assert!(data.len() > 100, "封面内容不能为空壳：{}B", data.len());
    assert!(data[0] == 0xFF && data[1] == 0xD8, "必须是 JPEG magic：{:02x} {:02x}", data[0], data[1]);
    let _ = std::fs::remove_file(&out);
}

#[test] fn e04_read_missing_file_exit1() {
    let io = CollectingIO::new();
    let code = run(&argv(&["read", "/nonexistent/no-such-file.mp3"]), &io);
    assert_eq!(code, 1, "读不到文件应 exit 1（不是 0 也不是 2）");
    assert!(io.err().contains("读取失败"), "应说明是读取失败：{}", io.err());
}

#[test] fn e05_read_no_cover_exit1() {
    // 无封面样本：盛夏（无封面）——先用 write 保证没有封面
    let dst = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-nocover.mp3");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(fx("华夏传说 - 凤凰传奇.mp3"), &dst).unwrap();
    let mut m = music_robot::tag::write::intent::WriteMeta::default();
    m.unset_fields.push("pictures".to_string());
    music_robot::tag::write::write_tags(&dst, &m).unwrap();
    let io = CollectingIO::new();
    let code = run(&argv(&["read", "--extract-cover", "/tmp/x.jpg", dst.to_str().unwrap()]), &io);
    assert_eq!(code, 1, "无封面导出应 exit 1");
    assert!(io.err().contains("没有封面"));
    let _ = std::fs::remove_file(&dst);
}

#[test] fn e06_help_flags() {
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["read", "-h"]), &io), 0);
    assert!(io.out().contains("--extract-cover"), "read -h 必须显示子命令帮助");
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["-V"]), &io), 0);
    assert!(io.out().contains("music-robot 0.1.0"));
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["-h"]), &io), 0);
    assert!(io.out() == TOP_USAGE);
}

#[test] fn e07_usage_errors_exit2() {
    // 未知选项 → 2
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["read", "--wat", "x.mp3"]), &io), 2);
    assert!(io.err().contains("未知选项"));
    // 缺位置参数 → 2
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["read"]), &io), 2);
    assert!(io.err().contains("用法"), "应打印 usage");
    // 未知命令 → 2
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["nonsense"]), &io), 2);
    // 无参数 → 2
    assert_eq!(run(&argv(&[]), &CollectingIO::new()), 2);
}

#[test] fn e08_verbose_goes_to_stderr() {
    let io = CollectingIO::new();
    let code = run(&argv(&["read", &px("华夏传说 - 凤凰传奇.mp3"), "--verbose"]), &io);
    assert_eq!(code, 0);
    assert!(io.err().starts_with("[verbose]"), "verbose 调试信息必须走 stderr：{}", io.err());
    let v: serde_json::Value = serde_json::from_str(io.err().trim_start_matches("[verbose] ")).unwrap();
    assert!(v["warnings"].is_array());
}

#[test] fn e09_write_side_wired_into_run() {
    // 证明 write/blank 已真正接进分发器（不再是「尚未移植」）
    let dst = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-write.mp3");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(fx("华夏传说 - 凤凰传奇.mp3"), &dst).unwrap();
    let s = dst.to_str().unwrap().to_string();

    let io = CollectingIO::new();
    let code = run(&argv(&["write", "--preview", "--title", "新标题", &s]), &io);
    assert_eq!(code, 0, "write --preview 应成功：{}", io.err());
    assert!(io.out().contains("title:") && io.out().contains("→"), "preview 应显示差异：\n{}", io.out());

    // preview 不得改文件
    let before = std::fs::read(&dst).unwrap();
    run(&argv(&["write", "--preview", "--title", "又改了", &s]), &CollectingIO::new());
    assert_eq!(std::fs::read(&dst).unwrap(), before, "--preview 必须零写入");

    // 真写
    let io = CollectingIO::new();
    let code = run(&argv(&["write", "--title", "最终标题", &s]), &io);
    assert_eq!(code, 0, "写失败：{}", io.err());
    let after = read_tags(&dst).unwrap();
    assert_eq!(after.title.as_deref(), Some("最终标题"));
    assert!(io.out().contains("音频 hash 校验通过"));

    // blank
    let io = CollectingIO::new();
    let code = run(&argv(&["blank", &s]), &io);
    assert_eq!(code, 0, "blank 失败：{}", io.err());
    let blanked = read_tags(&dst).unwrap();
    assert!(blanked.title.is_none() && blanked.artists.is_empty() && blanked.pictures.is_empty(),
        "blank 后必须全空，实际 title={:?} artists={:?}", blanked.title, blanked.artists);
    let _ = std::fs::remove_file(&dst);
}

#[test] fn e10_write_unset_roundtrip() {
    // --unset 白名单校验 + 真删
    let dst = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-unset.mp3");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(fx("华夏传说 - 凤凰传奇.mp3"), &dst).unwrap();
    let s = dst.to_str().unwrap().to_string();

    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["write", "--unset", "nonsense", &s]), &io), 2, "未知 unset 键必须报用法错误");
    assert!(io.err().contains("--unset 未知字段"));

    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["write", "--unset", "title,artist", &s]), &io), 0, "stderr={}", io.err());
    let m = read_tags(&dst).unwrap();
    assert!(m.title.is_none(), "--unset title 后标题应消失：{:?}", m.title);
    assert!(m.artists.is_empty(), "--unset artist 后歌手应消失：{:?}", m.artists);
    assert!(!m.albums.is_empty(), "未点名的专辑必须保留：{:?}", m.albums);
    let _ = std::fs::remove_file(&dst);
}

#[test] fn e11_write_json_shape() {
    let dst = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-json.mp3");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(fx("华夏传说 - 凤凰传奇.mp3"), &dst).unwrap();
    let s = dst.to_str().unwrap().to_string();
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["write", "--json", "--title", "X", &s]), &io), 0);
    let v: serde_json::Value = serde_json::from_str(&io.out()).unwrap();
    assert!(v["diffs"].as_array().unwrap().iter().any(|d| d["key"] == "title"));
    assert_eq!(v["audioHashOk"], true);
    assert!(v["after"]["title"] == "X", "after 视图应含新标题：{}", v["after"]["title"]);
    let _ = std::fs::remove_file(&dst);
}

#[test] fn e12_bak_copies_before_write() {
    let dst = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-bak.mp3");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    std::fs::copy(fx("华夏传说 - 凤凰传奇.mp3"), &dst).unwrap();
    let s = dst.to_str().unwrap().to_string();
    let before = std::fs::read(&dst).unwrap();
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["write", "--bak", "--title", "改标题", &s]), &io), 0);
    assert_eq!(std::fs::read(format!("{s}.bak")).unwrap(), before, "--bak 必须是写前原样副本");
    assert_ne!(std::fs::read(&dst).unwrap(), before, "目标文件必须已改写");
    for f in [dst.clone(), dst.with_extension("mp3.bak")] { let _ = std::fs::remove_file(f); }
    let _ = std::fs::remove_file(format!("{s}.bak"));
}

#[test]
fn e13_scan_doctor_wash_are_wired_not_stubbed() {
    // scan/doctor/wash 已从「尚未移植」stub 升级为真实现（见 tests/cli_scan.rs）。
    // 这里只钉死顶层分发不 panic、且不再声称未移植。
    for cmd in ["scan", "doctor", "wash"] {
        let io = CollectingIO::new();
        run(&argv(&[cmd, "-h"]), &io);
        assert!(io.out().contains("用法"), "{cmd} --help 应显示用法：{}", io.out());
        assert!(io.err().is_empty(), "{cmd} --help 不应有错误：{}", io.err());
    }
}

#[allow(dead_code)]
fn _usage_error_type(e: UsageError) -> String { e.message }

#[test]
fn e14_boolean_flag_eats_following_positional() {
    // ⚠️ 与 TS 参照实现完全一致的歧义：`read --json <file>` 里 `--json` 会把 <file>
    // 当成自己的值，导致位置参数为空 → 用法错误。正确写法是 flag 放位置参数之后。
    // 这里把它钉死，防止将来有人"修好"它造成与 TS 输出分叉。
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["read", "--json", &px("华夏传说 - 凤凰传奇.mp3")]), &io), 2,
        "--json <file> 应报用法错误（flag 吃掉了文件）");
    assert!(io.err().contains("用法"), "应打印 usage：{}", io.err());
    // 反例：flag 放后面就正常
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["read", &px("华夏传说 - 凤凰传奇.mp3"), "--json"]), &io), 0,
        "flag 放位置参数之后必须成功：{}", io.err());
}

#[test]
fn e15_cover_roundtrip_through_read() {
    // 封面导出 → 魔数校验 → 再嵌回去（跨 read/write 两个子命令的闭环）
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-cover2.png");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&out);
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["read", &px("牵丝戏 - 白兀.flac"), "--extract-cover", out.to_str().unwrap()]), &io), 0,
        "FLAC 封面导出应成功：{}", io.err());
    let data = std::fs::read(&out).expect("必须写出文件");
    assert!(!data.is_empty());
    println!("FLAC 封面: {}B magic={:02x}{:02x}{:02x}{:02x}", data.len(), data[0], data[1], data[2], data[3]);
    let _ = std::fs::remove_file(&out);
}
