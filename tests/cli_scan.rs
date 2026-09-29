//! scan / doctor / wash 批量命令测试（移植自 tests/cli/scan.test.ts / wash.test.ts / doctor.test.ts）
use std::path::Path;

use music_robot::cli::io::CollectingIO;
use music_robot::cli::{run, wash};
use music_robot::scanner::{inspect_file, rel_path, scan_dir, scan_files, Level};

fn argv(a: &[&str]) -> Vec<String> { a.iter().map(|s| s.to_string()).collect() }

/// 建一个隔离的临时库：拷贝若干真实样本 + 手工造 rejected / broken 样本
/// with_edge=true 时额外塞入「文本冒充 mp3」这个边界样本（scanner 判 ok 但实际不可写）
fn mklib(name: &str) -> std::path::PathBuf { mklib2(name, true) }

fn mklib2(name: &str, with_edge: bool) -> std::path::PathBuf {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(name);
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("sub")).unwrap();
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    for (src, dst) in [
        ("华夏传说 - 凤凰传奇.mp3", "clean-or-warn.mp3"),
        ("盛夏-毛不易.mp3", "warn-cover.mp3"),
        ("牵丝戏 - 白兀.flac", "sub/flac-ok.flac"),
    ] {
        std::fs::copy(fx.join(src), base.join(dst)).unwrap();
    }
    // broken：FLAC 魔数 + 垃圾体 → probe 认成 FLAC 但解析失败
    let mut brk = b"fLaC".to_vec();
    brk.extend_from_slice(&[0u8; 64]);
    std::fs::write(base.join("broken.flac"), &brk).unwrap();
    // ⚠️ 顺带钉死一个真实语义：纯文本冒充 .mp3 会被判 ok（MP3 无帧也合法，
    // 与 TS 参照实现一致，见 tests/cli/scan.test.ts）。不要"修好"它。
    if with_edge {
        std::fs::write(base.join("text-as-mp3.mp3"), b"this is not audio at all").unwrap();
    }
    // rejected：ID3v2 头 + 非音频内容
    let mut rej = vec![b'I', b'D', b'3', 4, 0, 0, 0, 0, 0];
    rej.extend_from_slice(b"not-audio-payload-not-audio-payload");
    std::fs::write(base.join("rejected.mp3"), rej).unwrap();
    base
}

#[test] fn s01_scan_files_recursive_sorted() {
    let base = mklib("s01");
    let files = scan_files(&base).unwrap();
    assert_eq!(files.len(), 6, "应有 6 个音频文件（含子目录）：{:?}", files);
    let names: Vec<String> = files.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert!(names.contains(&"clean-or-warn.mp3".to_string()));
    assert!(names.contains(&"flac-ok.flac".to_string()), "子目录必须被递归到");
    assert!(names.contains(&"broken.flac".to_string()));
    let mut sorted = files.clone(); sorted.sort();
    assert_eq!(files, sorted, "结果必须按路径排序稳定");
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn s02_scan_files_not_a_dir() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    assert!(scan_files(&p).is_err(), "文件而不是目录必须报错");
    assert!(scan_files(Path::new("/no/such/dir")).is_err());
}

#[test] fn s03_scan_files_empty_dir_reports_zero() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/s03-empty");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("readme.txt"), "不是音频").unwrap();
    assert!(scan_files(&base).unwrap().is_empty(), "无音频文件的目录应为空集");
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn s04_inspect_file_grades_four_levels() {
    let base = mklib("s04");
    let by = |n: &str| inspect_file(&base.join(n));

    let broken = by("broken.flac");
    assert_eq!(broken.level, Level::Broken, "随机字节应判 broken，实际 {:?}：{}", broken.level, broken.error.as_deref().unwrap_or("无"));
    assert!(broken.error.is_some() && broken.view.is_none());

    let rejected = by("rejected.mp3");
    assert_eq!(rejected.level, Level::Rejected, "ID3v2+非音频应判 rejected，实际 {:?}：{}", rejected.level, rejected.error.as_deref().unwrap_or("无"));
    assert!(rejected.view.is_none(), "rejected 不应有元数据视图");

    let ok_or_warn = by("warn-cover.mp3");
    assert!(matches!(ok_or_warn.level, Level::Ok | Level::Warn));
    assert!(ok_or_warn.view.is_some(), "ok/warn 必须有 readJson 视图");
    assert!(ok_or_warn.frame_count.unwrap() > 0);
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn s05_inspect_file_never_panics() {
    // 每文件独立 try/catch：坏文件不能污染好文件
    let base = mklib("s05");
    let good = inspect_file(&base.join("sub/flac-ok.flac"));
    let bad = inspect_file(&base.join("broken.flac"));
    assert!(matches!(good.level, Level::Ok | Level::Warn), "好文件不受坏文件影响：{:?} {:?}", good.level, good.error);
    assert_eq!(bad.level, Level::Broken);
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn s06_scan_dir_counts_all_four() {
    let base = mklib("s06");
    let r = scan_dir(&base).unwrap();
    assert_eq!(r.counts.total, 6);
    assert!(r.counts.broken >= 1, "应有 broken：{r:?}");
    assert!(r.counts.rejected >= 1, "应有 rejected：{r:?}");
    assert_eq!(r.counts.ok + r.counts.warn + r.counts.rejected + r.counts.broken, r.counts.total,
        "四级计数之和必须等于总数");
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn s07_rel_path_strips_dir() {
    let base = mklib("s07");
    let p = base.join("sub/flac-ok.flac");
    assert_eq!(rel_path(&p, &base), "sub/flac-ok.flac");
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e01_scan_table_lists_and_summarizes() {
    let base = mklib("e01");
    let io = CollectingIO::new();
    let code = run(&argv(&["scan", base.to_str().unwrap()]), &io);
    assert_eq!(code, 0, "扫描成功应 exit 0：{}", io.err());
    let out = io.out();
    assert!(out.contains("[") && out.contains("汇总:"), "应有状态行 + 汇总：\n{out}");
    assert!(out.contains("ok ") && out.contains("broken"), "汇总必须含四级计数：\n{out}");
    assert!(out.contains("⚠"), "有警告的文件必须标 ⚠：\n{out}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e02_scan_json_shape() {
    let base = mklib("e02");
    let io = CollectingIO::new();
    let code = run(&argv(&["scan", base.to_str().unwrap(), "--json"]), &io);
    assert_eq!(code, 0, "stderr={}", io.err());
    let v: serde_json::Value = serde_json::from_str(&io.out()).unwrap();
    assert!(v["counts"]["total"].as_u64().unwrap() == 6);
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 6);
    assert!(entries.iter().any(|e| e["level"] == "rejected"), "JSON 应含 rejected 条目");
    assert!(entries.iter().all(|e| e["path"].is_string() && e["warnings"].is_array()));
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e03_scan_empty_dir_exit1() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/e03-empty");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["scan", base.to_str().unwrap()]), &io), 1, "空目录应 exit 1");
    assert!(io.err().contains("没有 .mp3"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e04_scan_not_a_dir_exit1() {
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["scan", "/nonexistent/nope"]), &io), 1);
    assert!(io.err().contains("不是目录"));
}

#[test] fn e05_scan_help_and_usage() {
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["scan", "-h"]), &io), 0);
    assert!(io.out().contains("rejected"));
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["scan", "--wat", "x"]), &io), 2);
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["scan"]), &io), 2);
}

#[test] fn e06_doctor_reports_problems() {
    let base = mklib("e06");
    let io = CollectingIO::new();
    let code = run(&argv(&["doctor", base.to_str().unwrap()]), &io);
    assert_eq!(code, 1, "有 broken/rejected 时 doctor 应 exit 1（不是 0）");
    let out = io.out();
    assert!(out.contains("ffprobe") && out.contains("全库"), "应逐项报告：\n{out}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e07_doctor_json_shape() {
    let base = mklib("e07");
    let io = CollectingIO::new();
    let code = run(&argv(&["doctor", base.to_str().unwrap(), "--json"]), &io);
    assert_eq!(code, 1);
    let v: serde_json::Value = serde_json::from_str(&io.out()).unwrap();
    for k in ["probe", "config", "scan"] { assert!(v[k].get("ok").is_some(), "缺 {k}") }
    assert_eq!(v["exitCode"].as_i64().unwrap(), 1);
    assert!(v["scan"]["counts"]["broken"].as_u64().unwrap() >= 1);
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e08_wash_preview_default_no_write() {
    let base = mklib2("e08", false);
    let target = base.join("warn-cover.mp3");
    let before = std::fs::read(&target).unwrap();
    let io = CollectingIO::new();
    let code = run(&argv(&["wash", base.to_str().unwrap(), "--blank"]), &io);
    assert_eq!(code, 0, "preview 无失败应 exit 0：{}", io.err());
    assert_eq!(std::fs::read(&target).unwrap(), before, "⚠️ --preview 必须零写入");
    let out = io.out();
    assert!(out.contains("preview") && out.contains("汇总:"), "\n{out}");
    assert!(!out.lines().any(|l| l.contains("→ applied")),
        "preview 模式的状态行不得声称已应用：\n{out}");
    assert!(out.lines().count() >= 6, "每个文件都要有一行剧本：\n{out}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e09_wash_apply_blank_verifies_no_leftovers() {
    let base = mklib2("e09", false);
    let io = CollectingIO::new();
    let code = run(&argv(&["wash", base.to_str().unwrap(), "--blank", "--apply"]), &io);
    assert_eq!(code, 0, "blank apply 应全部成功。stdout=\n{}\nstderr={}", io.out(), io.err());
    let out = io.out();
    assert!(out.lines().any(|l| l.contains("→ applied")), "apply 模式应有 applied 状态行：\n{out}");
    // 复核：被处理的文件必须真的空了
    let m = music_robot::tag::read::read_tags(&base.join("warn-cover.mp3")).unwrap();
    let left = wash::blank_leftovers(&m);
    assert!(left.is_empty(), "blank 后不得有残留字段：{left:?}（title={:?} artists={:?}）", m.title, m.artists);
    // 坏文件必须被跳过
    assert!(out.lines().any(|l| l.contains("→ skipped") && l.contains("broken")),
        "坏文件必须跳过并说明原因：\n{out}");
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e10_wash_unset_keys_and_rule_parsing() {
    // --blank 与 --unset 互斥
    let base = mklib2("e10", false);
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["wash", base.to_str().unwrap(), "--blank", "--unset", "title"]), &io), 2,
        "互斥规则必须报用法错误");
    assert!(io.err().contains("互斥"));
    // 缺规则
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["wash", base.to_str().unwrap(), "--apply"]), &io), 2);
    assert!(io.err().contains("需要 --blank 或 --unset"));
    // 未知字段
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["wash", base.to_str().unwrap(), "--unset", "nonsense"]), &io), 2);
    assert!(io.err().contains("未知字段"));
    // 空字段列表
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["wash", base.to_str().unwrap(), "--unset", ""]), &io), 2);

    // 真 unset：title 必须消失，未点名字段保留
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["wash", base.to_str().unwrap(), "--unset", "title", "--apply"]), &io), 0);
    let m = music_robot::tag::read::read_tags(&base.join("warn-cover.mp3")).unwrap();
    assert!(m.title.is_none(), "unset title 后应消失：{:?}", m.title);
    assert!(!m.albums.is_empty(), "未点名的 albums 必须保留：{:?}", m.albums);
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e11_wash_bak_keeps_first_backup() {
    let base = mklib2("e11", false);
    let target = base.join("warn-cover.mp3");
    let orig = std::fs::read(&target).unwrap();

    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["wash", base.to_str().unwrap(), "--unset", "title", "--bak", "--apply"]), &io), 0);
    let bak_s = format!("{}.bak", target.display());
    let bak = Path::new(&bak_s);
    assert!(bak.exists(), "--bak 必须建备份");
    assert_eq!(std::fs::read(bak).unwrap(), orig, "首备份必须是写前原样");

    // 重跑：不得覆盖已有首备份（round8 P2-2）
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["wash", base.to_str().unwrap(), "--unset", "title", "--bak", "--apply"]), &io), 0);
    assert_eq!(std::fs::read(bak).unwrap(), orig, "⚠️ 已有 .bak 时必须保留首备份，不得被覆盖");
    assert!(io.out().contains("保留首备份") || std::fs::read(bak).unwrap() == orig);
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e12_wash_events_ndjson() {
    let base = mklib2("e12", false);
    let evf = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/e12-events.ndjson");
    let _ = std::fs::remove_file(&evf);
    let io = CollectingIO::new();
    let code = run(&argv(&["wash", base.to_str().unwrap(), "--unset", "title", "--bak", "--apply",
                            "--events", evf.to_str().unwrap()]), &io);
    assert_eq!(code, 0, "stderr={}", io.err());
    assert!(evf.exists(), "--events 必须写出 NDJSON 文件");
    let text = std::fs::read_to_string(&evf).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(!lines.is_empty(), "事件流不能是空文件");
    for l in &lines {
        let v: serde_json::Value = serde_json::from_str(l).expect("每行必须是合法 JSON: {l}");
        assert_eq!(v["cmd"], "wash");
        assert_eq!(v["kind"], "file");
        assert!(v["path"].is_string() && v["state"].is_string() && v["ts"].is_string());
    }
    let _ = std::fs::remove_file(&evf);
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e13_wash_unwritable_events_path_is_not_fatal() {
    // events 父目录不存在 → 不应让整个 wash 崩溃
    let base = mklib("e13");
    let io = CollectingIO::new();
    let code = run(&argv(&["wash", base.to_str().unwrap(), "--unset", "title", "--apply",
                            "--events", "/no/such/dir/e.ndjson"]), &io);
    assert_ne!(code, 2, "事件流写失败不该变成用法错误");
    let _ = std::fs::remove_dir_all(&base);
}

#[test] fn e14_scan_command_registered() {
    // 顶层分发必须真的接到 scan/doctor/wash（不是「尚未移植」）
    let base = mklib("e14");
    let io = CollectingIO::new();
    assert_eq!(run(&argv(&["scan", base.to_str().unwrap(), "--json"]), &io), 0, "{}", io.err());
    let io = CollectingIO::new();
    assert!(run(&argv(&["doctor", base.to_str().unwrap()]), &io) <= 1);
    assert!(!io.err().contains("尚未移植"), "doctor 不得再报未移植：{}", io.err());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn s08_text_disguised_as_mp3_is_ok_like_ts() {
    // ⚠️ 忠实移植钉死项：MP3 是 sync-marker 格式，无帧也合法，所以纯文本冒充 .mp3
    // 会被判 ok / format=unknown / frameCount=0。TS 参照实现行为完全一致。
    // 如果有人"觉得这不合理"想改成 broken，必须同时改 TS，并保持两边一致。
    let base = mklib("s08");
    let e = inspect_file(&base.join("text-as-mp3.mp3"));
    assert_eq!(e.level, Level::Ok, "纯文本冒充 mp3 → ok，实际 {:?}：{:?}", e.level, e.error);
    assert_eq!(e.frame_count, Some(0), "无帧");
    assert!(e.artists_summary.is_none() && e.title.is_none());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn e15_wash_on_frameless_mp3_is_a_known_limitation() {
    // ⚠️ 已知局限（真实、未修）：scanner 把「文本冒充 .mp3」判 ok（MP3 无帧也合法），
    // 于是 wash 会对它执行 blank，把 ID3 头写进一个纯文本文件，事后读回复核失败。
    // 这是 scanner「ok = 可解析」与 wash「可处理」之间的语义缝隙，TS 参照实现同样存在。
    // 钉死它，防止将来有人悄悄改变行为却不更新本用例。
    let base = mklib("e15");
    let before = std::fs::read(base.join("text-as-mp3.mp3")).unwrap();
    let io = CollectingIO::new();
    let code = run(&argv(&["wash", base.to_str().unwrap(), "--unset", "title", "--apply"]), &io);
    assert_eq!(code, 1, "非音频文件被 wash 处理应报 failed（exit 1）");
    assert!(io.out().lines().any(|l| l.contains("text-as-mp3") && l.contains("failed")),
        "该文件应如实报 failed：\n{}", io.out());
    assert!(std::fs::read(base.join("text-as-mp3.mp3")).unwrap() != before,
        "⚠️ 局限：文件已被改写（音频字节保护对这类文件不适用）");
    let _ = std::fs::remove_dir_all(&base);
}
