//! 测试专用工具：带 Drop 清理的临时目录（复用 std::env::temp_dir 下的唯一名）。

use std::path::{Path, PathBuf};

/// 一个独占的临时目录；离开作用域时递归删除（失败也忽略，绝不 panic）。
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!(
            "music-robot-watcher-{tag}-{}-{nanos:x}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&p);
        TempDir(p)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
