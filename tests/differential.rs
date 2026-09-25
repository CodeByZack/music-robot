//! 跨实现差分测试 —— Rust 读结果必须与 TS 实现的 `read --json` 逐字段相等。
//!
//! 这是本迁移的安全网：TS 侧已 101 用例全绿并经 ffprobe 对拍，把它当**参照实现**。
//! Rust 行为一漂移就当场红。**铁律：不修改期望值来让测试通过。**
use std::process::Command;

const TS_REPO: &str = "/vol1/@appshare/dsh/data/tagwash-test";
const NODE_CANDIDATES: &[&str] = &["node", "/var/apps/nodejs_v24/target/bin/node"];

fn ts_read_json(file: &str) -> Option<serde_json::Value> {
    let node = NODE_CANDIDATES.iter()
        .find(|n| Command::new(n).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))?;
    let out = match Command::new(node)
        .args(["--experimental-strip-types", "src/cli.ts", "read", file, "--json"])
        .current_dir(TS_REPO).output() { Ok(o) => o, Err(e) => { eprintln!("DIAG spawn failed: {e}"); return None } };
    if !out.status.success() { eprintln!("DIAG node exit={:?} stderr={}", out.status.code(), String::from_utf8_lossy(&out.stderr)); return None }
    let txt = String::from_utf8_lossy(&out.stdout);
    match serde_json::from_str::<serde_json::Value>(&txt) { Ok(v) => Some(v), Err(e) => { eprintln!("DIAG JSON parse failed: {e}"); None } }
}

fn str_of(v: Option<&serde_json::Value>) -> String { v.and_then(|x| x.as_str()).map(str::to_string).unwrap_or_default() }
fn num_of(v: Option<&serde_json::Value>) -> i64 { v.and_then(|x| x.as_i64()).unwrap_or(-1) }
fn arr_first(v: Option<&serde_json::Value>) -> String {
    v.and_then(|x| x.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()).map(str::to_string).unwrap_or_default()
}
fn count(v: Option<&serde_json::Value>) -> i64 { v.and_then(|x| x.as_array()).map(|a| a.len() as i64).unwrap_or(-1) }

#[test]
fn rust_matches_ts_reference_on_all_samples() {
    // ⚠️ 参照实现不可用时**必须红**，不得静默 skip —— 否则会伪装成通过（SCRIPTC-NOTES §三 教训）。
    if which_node().is_none() { panic!("参照实现不可用：找不到 node。差分测试拒绝静默跳过。") }
    if !std::path::Path::new(TS_REPO).exists() { panic!("参照仓库不可达：{TS_REPO}") }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| matches!(p.extension().and_then(|s| s.to_str()), Some("mp3") | Some("flac")))
        .collect();
    files.sort();
    assert_eq!(files.len(), 6, "样本应为 6 个");

    for p in &files {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let r = ts_read_json(&format!("samples/{name}")).expect("TS read 失败");
        let m = music_tag::tag::read::read_tags(p).unwrap_or_else(|e| panic!("{name}: Rust 读取失败 {e}"));

        for (field, want, have) in [
            ("source",     str_of(r.get("source")),                     m.source.clone()),
            ("title",      str_of(r.get("title")),                      m.title.clone().unwrap_or_default()),
            ("artists[0]", arr_first(r.get("artists")),                 m.artists.first().cloned().unwrap_or_default()),
            ("albums[0]",  arr_first(r.get("albums")),                  m.albums.first().cloned().unwrap_or_default()),
            ("durationMs", num_of(r.get("durationMs")).to_string(),     m.duration_ms.to_string()),
            ("sampleRate", num_of(r.get("sampleRate")).to_string(),     m.sample_rate.to_string()),
            ("pictures",   count(r.get("pictures")).to_string(),        m.pictures.len().to_string()),
            ("rawFrames",  count(r.get("rawFrames")).to_string(),       m.raw_frames.len().to_string()),
        ] {
            assert_eq!(want, have, "{name}：字段 {field} 与 TS 参照不一致");
        }
    }
}


fn which_node() -> Option<std::path::PathBuf> {
    NODE_CANDIDATES.iter().map(|n| std::path::PathBuf::from(n))
        .find(|p| p.exists() || Command::new(p).arg("--version").output().map(|o| o.status.success()).unwrap_or(false))
}
