//! 插件子系统端到端 —— **真的拉起 node / python3** 把整条协议链跑通。
//!
//! ## 为什么单独有这个文件
//!
//! `manifest` 的单元测试只断言 `command_of()` 返回的**字符串**；`pool` 的单元测试用的是
//! `/bin/sh` 拼出来的假插件。于是这条链 ——
//! 「按扩展名推断命令 → 真的用那个解释器起进程 → stdin/stdout JSON Lines 往返 → 协议解析」——
//! 在它们各自的测试里**没有一处被走通**。命令推断错了、插件进程没 flush、
//! 响应少个字段，单元测试都不会红。
//!
//! 这里改用仓库里真实存在的 `plugins/example.js` 与 `plugins/example.py` 把整条链跑实。

use music_robot::plugin::{
    parse_manifest, DownloadPrefer, DownloadRequest, ErrorCode, PluginRequest, PluginResponse,
    PoolConfig, RequestOptions, SandboxConfig, ScrapeRequest, SongRef, WorkerPool,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 仓库根目录（`CARGO_MANIFEST_DIR`），示例插件就放在它的 `plugins/` 下。
fn plugins_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins")
}

/// 测试用临时工作目录，**Drop 时自动清理**（否则每跑一次就在 /tmp 漏一个目录）。
struct TempRoot(PathBuf);

impl std::ops::Deref for TempRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_work(tag: &str) -> TempRoot {
    let dir = std::env::temp_dir().join(format!("music-robot-e2e-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建测试临时目录失败");
    TempRoot(dir)
}

/// 宽松沙箱：让 spawn 路径真的走一遍 setrlimit，但不至于把正常解释器掐死。
/// NPROC 给 4096 是因为 RLIMIT_NPROC 按 uid 全局计数，测试机上不可控。
fn loose_sandbox() -> SandboxConfig {
    SandboxConfig {
        memory_mb: 4096,
        cpu_sec: 60,
        procs: 4096,
        file_mb: 100,
        nofile: 256,
        plugin_uid: None,
        plugin_gid: None,
    }
}

fn scrape_req(id: &str, work_dir: &Path, title: &str) -> PluginRequest {
    PluginRequest::Scrape(ScrapeRequest {
        id: id.to_string(),
        song: SongRef {
            title: Some(title.to_string()),
            ..SongRef::default()
        },
        work_dir: work_dir.to_string_lossy().into_owned(),
        want: Vec::new(),
        options: RequestOptions::default(),
    })
}

/// 跑一遍「清单解析 → 池 → 真实进程 → 协议解析」的完整链路。
/// `expect_cmd` 是**应当被真正执行**的解释器。
fn run_e2e(plugin_file: &str, expect_cmd: &str, expect_source: &str, tag: &str) {
    let path = plugins_dir().join(plugin_file);
    assert!(path.is_file(), "示例插件不存在：{}", path.display());

    // 1) 清单解析：命令是**按扩展名推断**出来的
    let meta = parse_manifest(&path).expect("解析示例插件清单");
    assert_eq!(
        meta.command,
        vec![expect_cmd.to_string(), path.to_string_lossy().into_owned()],
        "命令推断结果不对"
    );
    assert_eq!(meta.protocol, 1);

    // 2) 真的用那个解释器起进程
    let work = temp_work(tag);
    let pool = WorkerPool::new(
        meta,
        loose_sandbox(),
        work.join("plugin-work"),
        PoolConfig {
            max: 1,
            idle_timeout: Duration::from_secs(60),
            task_timeout: Duration::from_secs(10),
        },
    );

    let mut guard = pool.acquire(Duration::from_secs(10)).expect("应当借到 worker");

    let first = guard
        .call(&scrape_req("e2e-1", &work, "七里香"))
        .expect("第一次调用");
    match &first {
        PluginResponse::ScrapeOk(ok) => {
            assert_eq!(ok.id, "e2e-1", "响应必须原样回显请求 id");
            assert_eq!(ok.source.as_deref(), Some(expect_source), "source 不对");
            assert!(
                ok.confidence > 0.5,
                "confidence 应当 > 0.5（默认才是 0.50），实际 {}",
                ok.confidence
            );
        }
        other => panic!("期望 ScrapeOk，实际 {:?}", other),
    }

    // 3) daemon 复用：第二次调用**不该**再起进程
    let second = guard
        .call(&scrape_req("e2e-2", &work, "夜的第七章"))
        .expect("第二次调用");
    match &second {
        PluginResponse::ScrapeOk(ok) => assert_eq!(ok.id, "e2e-2"),
        other => panic!("期望 ScrapeOk，实际 {:?}", other),
    }
    assert_eq!(
        pool.stats().spawned_total,
        1,
        "daemon 模式必须复用同一个进程"
    );

    // 4) 未实现的 action：同样要回显 action，并给出明确错误码
    let dl = PluginRequest::Download(DownloadRequest {
        id: "e2e-dl".to_string(),
        song: SongRef::default(),
        target_dir: work.to_string_lossy().into_owned(),
        prefer: DownloadPrefer::default(),
        options: RequestOptions::default(),
    });
    match guard.call(&dl).expect("download 调用") {
        PluginResponse::Error(e) => {
            assert_eq!(e.id, "e2e-dl", "错误响应也要回显 id");
            assert_eq!(
                e.error.code,
                ErrorCode::NotFound,
                "示例插件对不支持的 action 回 NOT_FOUND"
            );
            assert!(!e.error.message.is_empty(), "错误信息不该为空");
        }
        other => panic!("不支持 download 的插件应当回 Error，实际 {:?}", other),
    }

    drop(guard);
    pool.shutdown();
}

#[test]
fn node_plugin_end_to_end() {
    run_e2e("example.js", "node", "example-js", "node");
}

#[test]
fn python_plugin_end_to_end() {
    run_e2e("example.py", "python3", "example-py", "py");
}

#[test]
fn shell_plugin_end_to_end() {
    run_e2e("example.sh", "sh", "example-sh", "sh");
}
