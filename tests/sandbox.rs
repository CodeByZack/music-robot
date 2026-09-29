//! 路径沙箱测试 —— 服务端会拿用户输入的路径读写文件，这层是越权防线。
//! ⚠️ 每个用例的 fixture 都**独占一个根目录**（按用例名隔离）：
//!   cargo 默认并行跑用例，若共享同一个 sandbox 目录，多个线程同时 remove_dir_all 会互删，
//!   表现成诡异的 "AlreadyExists" —— 第一次跑就是这么死的。
use music_robot::fs::{FsError, PathSandbox};
use std::path::{Path, PathBuf};

/// 建一棵带陷阱的受控目录：库根内 / 库根外 / 逃逸 symlink 各一份。
fn mkroot(name: &str) -> PathBuf {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("sandbox").join(name);
    let r = base.join("root");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(r.join("sub")).unwrap();
    std::fs::create_dir_all(base.join("outside")).unwrap();
    std::fs::write(base.join("outside").join("secret.txt"), b"TOP-SECRET").unwrap();
    std::fs::write(r.join("ok.mp3"), b"payload").unwrap();
    std::fs::write(r.join("sub").join("deep.flac"), b"deep").unwrap();
    // 逃逸用 symlink：指向库根之外
    #[cfg(unix)] std::os::unix::fs::symlink(base.join("outside"), r.join("escape_link")).unwrap();
    #[cfg(unix)] std::os::unix::fs::symlink("/etc/passwd", r.join("abs_link")).unwrap();
    r
}

#[test]
fn s01_legitimate_paths_resolve() {
    let r = mkroot("s01");
    let sb = PathSandbox::new(&r).unwrap();
    assert!(sb.resolve(Path::new("ok.mp3")).is_ok());
    assert!(sb.resolve(Path::new("sub/deep.flac")).is_ok());
    // 绝对路径若确实在 root 内也允许（服务端常拿到绝对路径）
    assert!(sb.resolve(r.join("ok.mp3").as_path()).is_ok());
    let resolved = sb.resolve(Path::new("sub/deep.flac")).unwrap();
    assert!(resolved.ends_with("sub/deep.flac"), "相对路径应解析到库根下，实际 {resolved:?}");
}

#[test]
fn s02_rejects_parent_traversal() {
    let r = mkroot("s02");
    let sb = PathSandbox::new(&r).unwrap();
    for evil in ["../outside/secret.txt", "sub/../../outside/secret.txt", "./a/../../outside/secret.txt"] {
        let e = sb.resolve(Path::new(evil)).expect_err(&format!("{evil} 必须被拒"));
        assert!(matches!(e, FsError::Escape { .. }), "{evil} 应判为越界，实际 {e:?}");
    }
}

#[test]
fn s03_rejects_symlink_escape() {
    // 最关键的一条：只做字符串前缀检查的实现会在这里放行
    let r = mkroot("s03");
    let sb = PathSandbox::new(&r).unwrap();
    for link in ["escape_link/secret.txt", "abs_link"] {
        let e = sb.resolve(Path::new(link)).expect_err(&format!("{link} 经 symlink 越界，必须被拒"));
        assert!(matches!(e, FsError::Escape { .. }), "{link} 应判越界，实际 {e:?}");
    }
    // 且不得真的读到内容
    assert!(sb.read_file(Path::new("abs_link")).is_err(), "read_file 也必须拦下 symlink 逃逸");
}

#[test]
fn s04_absolute_path_outside_root_rejected() {
    let r = mkroot("s04");
    let sb = PathSandbox::new(&r).unwrap();
    assert!(matches!(sb.resolve(Path::new("/etc/passwd")), Err(FsError::Escape { .. })));
    assert!(sb.read_file(Path::new("/etc/passwd")).is_err());
    // 边界：同前缀但不同目录不得误判通过（/root vs /root2 这类字符串前缀陷阱）
    let r2 = r.with_file_name("root2");
    let _ = std::fs::create_dir_all(&r2);
    std::fs::write(r2.join("x.txt"), b"x").unwrap();
    assert!(matches!(sb.resolve(r2.join("x.txt").as_path()), Err(FsError::Escape { .. })), "/root2 不得因字符串前缀匹配 /root 而放行");
}

#[test]
fn s05_write_operations_confined_to_root() {
    let r = mkroot("s05");
    let sb = PathSandbox::new(&r).unwrap();
    // 写操作要走同一道闸：不能只给读侧加防护
    assert!(sb.write_file(Path::new("new.mp3"), b"x".to_vec()).is_ok());
    assert!(sb.write_file(Path::new("../outside/planted.mp3"), b"x".to_vec()).is_err(), "写不得越界");
    assert!(!r.parent().unwrap().join("outside").join("planted.mp3").exists(), "被拒的写不得留下文件");
    // rename 两端都要校验
    assert!(sb.rename(Path::new("new.mp3"), Path::new("renamed.mp3")).is_ok());
    assert!(sb.rename(Path::new("renamed.mp3"), Path::new("../outside/smuggled.mp3")).is_err());
    assert!(!r.parent().unwrap().join("outside").join("smuggled.mp3").exists());
    // 反向：从库外搬进来也不行
    assert!(sb.rename(r.parent().unwrap().join("outside").join("secret.txt").as_path(), Path::new("in.mp3")).is_err());
    assert!(!r.join("in.mp3").exists());
}

#[test]
fn s06_atomic_replace_via_sandbox_keeps_audio_and_cleans_tmp() {
    // 原子写必须走沙箱：tmp 落在同目录（rename 同设备），失败不留残留
    use music_robot::tag::write::atomic::atomic_replace_fs;
    let r = mkroot("s06");
    let sb = PathSandbox::new(&r).unwrap();
    let target = Path::new("ok.mp3");
    let before = sb.read_file(target).unwrap();
    let err = atomic_replace_fs(&sb, &r.join("ok.mp3"), b"changed".to_vec(), Some(&|_: &[u8], _: &[u8]| false))
        .expect_err("verify 恒假必须报错");
    assert!(err.to_string().contains("完整性"), "错误文案应指明完整性失败：{err}");
    assert_eq!(sb.read_file(target).unwrap(), before, "校验失败后原内容必须不变");
    let leftovers: Vec<String> = std::fs::read_dir(&r).unwrap().filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("music-robot-tmp")).collect();
    assert!(leftovers.is_empty(), "不得残留 tmp：{leftovers:?}");
    // 越界路径做原子写也必须被拒
    assert!(atomic_replace_fs(&sb, Path::new("/etc/nope"), b"x".to_vec(), None).is_err(), "库外路径不得原子写");
}

#[test]
fn s07_library_read_respects_sandbox() {
    // 端到端：把沙箱交给 read_tags，越界路径要报错而不是静默读成功
    use music_robot::tag::read::{read_tags_fs, ReadError};
    let r = mkroot("s07");
    let sb = PathSandbox::new(&r).unwrap();
    assert!(read_tags_fs(&sb, &r.join("nope.mp3")).is_err());
    // 逃逸错误必须能被类型区分（服务端要据此返回 403 而不是 500）
    let e = read_tags_fs(&sb, Path::new("/etc/passwd")).expect_err("任意绝对路径必须被拒");
    match e {
        ReadError::Escape(msg) => assert!(msg.contains("越界"), "错误应说明越界：{msg}"),
        other => panic!("越界应返回 Escape，实际 {other:?}"),
    }
    // root 内的真实 mp3 仍可正常读（证明收口没把功能改坏）
    std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures").join("华夏传说 - 凤凰传奇.mp3"), r.join("lib_test.mp3")).unwrap();
    let m = read_tags_fs(&sb, Path::new("lib_test.mp3")).expect("沙箱内的合法样本应能读");
    assert_eq!(m.title.as_deref(), Some("华夏传说"));
}

#[test]
fn s08_list_dir_stays_inside_and_marks_directories() {
    let r = mkroot("s08");
    let sb = PathSandbox::new(&r).unwrap();
    let mut names: Vec<String> = sb.list_files("", &["mp3", "flac"]).unwrap().into_iter()
        .map(|p| p.to_string_lossy().to_string()).collect();
    names.sort();
    assert!(names.iter().any(|n| n.ends_with("ok.mp3")), "应列出 root 内文件：{names:?}");
    assert!(names.iter().any(|n| n.ends_with("deep.flac")), "应递归子目录：{names:?}");
    assert!(!names.iter().any(|n| n.contains("secret") || n.contains("outside")), "不得列出 root 之外：{names:?}");
    assert!(!names.iter().any(|n| n.contains("escape_link")), "symlink 目标在库外的条目不应作为可读文件暴露");
    assert!(sb.list_files("..", &["txt"]).is_err(), "列目录同样受路径闸门约束");
    assert!(sb.list_files("ok.mp3", &["txt"]).is_err(), "指向文件的目录参数应报 NotADirectory");
}

#[test]
fn s09_missing_root_is_an_error_not_open_access() {
    // fail-closed：root 配错/不存在时绝不能退化成"无限制访问"
    let e = PathSandbox::new(Path::new("/nonexistent/music-robot-sandbox-root-xyz")).err_or_fail();
    assert!(matches!(e, FsError::Io { .. } | FsError::NotADirectory { path: _ }), "应有明确错误，实际 {e:?}");
}

#[test]
fn s10_case_and_normalization_games_rejected() {
    let r = mkroot("s10");
    let sb = PathSandbox::new(&r).unwrap();
    // 规范化绕过：多余点、重复斜杠、以及 ./ 前缀组合
    for evil in ["./../outside/secret.txt", "//../outside/secret.txt", "sub/.././../outside/secret.txt"] {
        assert!(sb.resolve(Path::new(evil)).is_err(), "{evil} 必须被拒");
    }
    assert!(sb.resolve(Path::new("")).is_ok(), "空串应等价于库根本身");
    assert!(sb.resolve(Path::new("sub/../")).is_ok(), "规范化后仍在根内");
}

trait OrFail { fn err_or_fail(self) -> FsError; }
impl OrFail for Result<PathSandbox, FsError> {
    fn err_or_fail(self) -> FsError {
        match self { Ok(_) => panic!("root 不存在时不该构造成功（fail-closed）"), Err(e) => e }
    }
}
