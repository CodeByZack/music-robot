//! src/watcher/suppress.rs — 自写抑制注册表
//!
//! 为什么需要它（PoC 实测结论见 inotify_poc.rs 的打印输出）：
//! `crate::tag::write::atomic::atomic_replace` 用 **rename 覆盖** 落盘。Linux inotify
//! 在**原文件路径**上如实报出 `IN_MOVED_TO`（伴随 `IN_MOVED_FROM`，两者 cookie 相同），
//! 而该路径**没有任何** `IN_DELETE` —— 也就是说，「我们自己写回」与「外部新建一个
//! 文件」在位掩码上**完全无法区分**。既然事件本身不带来源，唯一可靠的判据就是
//! **时间相关性**：写回之前主动登记路径，TTL 窗口内把该路径的事件判成自写。
//!
//! 本模块只回答「这是不是我自己刚写的」，不回答「该不该入库」——后者在 classify.rs。

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// 默认抑制窗口：足够覆盖一次写回从 rename 到事件被消费的全部延迟，
/// 又不至于把用户「5 秒后真的替换了这个文件」的合法操作误伤。
pub const DEFAULT_TTL: Duration = Duration::from_secs(5);

/// 本项目原子写的 tmp 文件名标记（见 tag/write/atomic.rs 的 tmp_name）。
pub const TMP_MARKER: &str = ".music-robot-tmp-";

/// 识别本项目原子写留下的临时文件：文件名含 `.music-robot-tmp-`。
///
/// 按**名字**判断，不依赖注册表 —— 即使注册表没登记（或已过期），tmp 文件也绝不能被
/// 当成新音频入库。用 lossy 转换是为了让非 UTF-8 文件名也能命中 ASCII 标记。
pub fn is_our_tmp_file(path: &Path) -> bool {
    match path.file_name() {
        Some(n) => n.to_string_lossy().contains(TMP_MARKER),
        None => false,
    }
}

/// 词法规范化：不碰文件系统。去掉 `.`、折叠 `..`；相对路径先并上 cwd。
fn lexical_abs(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            // pop 在根目录上返回 false 且保持 "/"：越界的 .. 不会把路径搞坏
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 统一路径形状：能 canonicalize 就 canonicalize（解析符号链接 + `..` + `.`），
/// 失败（路径还不存在 / 权限不足）退回词法规范化。**任何情况下都不 panic**。
///
/// 已知取舍：若登记时文件存在（canonical）而查询时已被删除（词法），且中间隔着符号
/// 链接，两次形状可能不一致。Linux 上 /tmp 非符号链接，本模块的实际用法（同一目录
/// 树、同一相对基准）不会触发该组合。
fn normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| lexical_abs(path))
}

/// 记住「这些文件是我自己刚写的」，在 TTL 内忽略它们的事件。线程安全，可跨线程共享。
pub struct SelfWriteRegistry {
    ttl: Duration,
    /// key = 规范化路径，value = 登记时刻
    inner: Mutex<HashMap<PathBuf, Instant>>,
}

impl SelfWriteRegistry {
    /// 新建注册表。TTL 是抑制窗口长度（服务端用 `DEFAULT_TTL` = 5s）。
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, inner: Mutex::new(HashMap::new()) }
    }

    /// 写回**之前**调用：登记该路径为自写。重复登记以最后一次为准。
    pub fn note_write(&self, path: &Path) {
        let key = normalize(path);
        self.entries().insert(key, Instant::now());
    }

    /// 该路径是否处在自写抑制窗口内。TTL 过期后返回 false 并顺手清除该条目。
    pub fn is_self_write(&self, path: &Path) -> bool {
        let key = normalize(path);
        let ttl = self.ttl;
        let mut m = self.entries();
        match m.get(&key) {
            Some(t) if Instant::now().saturating_duration_since(*t) < ttl => true,
            Some(_) => {
                m.remove(&key);
                false
            }
            None => false,
        }
    }

    /// 清掉所有过期项。
    pub fn prune(&self) {
        let ttl = self.ttl;
        let now = Instant::now();
        self.entries().retain(|_, t| now.saturating_duration_since(*t) < ttl);
    }

    /// 注册表内的条目数（可能含尚未 prune 的过期项）。
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    /// 空表。
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    /// 抑制窗口长度。
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// 锁中毒不 panic：拿回内部数据继续用（生产路径禁止 unwrap/expect/panic）。
    fn entries(&self) -> MutexGuard<'_, HashMap<PathBuf, Instant>> {
        match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl Default for SelfWriteRegistry {
    fn default() -> Self {
        Self::new(DEFAULT_TTL)
    }
}

impl std::fmt::Debug for SelfWriteRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelfWriteRegistry").field("ttl", &self.ttl).field("len", &self.len()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::test_support::TempDir;

    #[test]
    fn default_ttl_is_five_seconds() {
        assert_eq!(DEFAULT_TTL, Duration::from_secs(5));
        assert_eq!(SelfWriteRegistry::default().ttl(), DEFAULT_TTL);
        assert!(SelfWriteRegistry::new(Duration::from_millis(7)).is_empty());
    }

    #[test]
    fn note_then_is_self_write() {
        let reg = SelfWriteRegistry::new(DEFAULT_TTL);
        let p = std::env::temp_dir().join("music-robot-watcher-never-created.mp3");
        assert!(!reg.is_self_write(&p));
        reg.note_write(&p);
        assert!(reg.is_self_write(&p));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn unknown_path_is_not_self_write() {
        let reg = SelfWriteRegistry::new(DEFAULT_TTL);
        reg.note_write(Path::new("/tmp/a.mp3"));
        assert!(!reg.is_self_write(Path::new("/tmp/b.mp3")));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn ttl_expiry_turns_is_self_write_false() {
        let reg = SelfWriteRegistry::new(Duration::from_millis(40));
        let p = std::env::temp_dir().join("music-robot-watcher-ttl.mp3");
        reg.note_write(&p);
        assert!(reg.is_self_write(&p), "刚登记就必须命中");
        std::thread::sleep(Duration::from_millis(70));
        assert!(!reg.is_self_write(&p), "TTL 过期后必须返回 false");
        assert_eq!(reg.len(), 0, "过期条目应被 is_self_write 顺手清掉");
    }

    #[test]
    fn prune_drops_expired_keeps_fresh() {
        let reg = SelfWriteRegistry::new(Duration::from_millis(60));
        let stale = std::env::temp_dir().join("music-robot-watcher-stale.mp3");
        reg.note_write(&stale);
        std::thread::sleep(Duration::from_millis(80));
        let fresh = std::env::temp_dir().join("music-robot-watcher-fresh.mp3");
        reg.note_write(&fresh);
        assert_eq!(reg.len(), 2);
        reg.prune();
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_self_write(&stale));
        assert!(reg.is_self_write(&fresh));
    }

    #[test]
    fn path_shapes_with_dot_and_dotdot_are_equal() {
        let dir = TempDir::new("suppress-shape");
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).expect("建子目录");
        let file = dir.path().join("song.mp3");
        std::fs::write(&file, b"x").expect("写文件");

        let reg = SelfWriteRegistry::new(DEFAULT_TTL);
        reg.note_write(&file); // 绝对、canonical

        // 同一个文件的多种「不同写法」都必须命中
        assert!(reg.is_self_write(&sub.join("..").join("song.mp3")), ".. 形式");
        assert!(reg.is_self_write(&dir.path().join(".").join("song.mp3")), "./ 形式");
        assert!(reg.is_self_write(&dir.path().join("sub/../song.mp3")), "混合形式");
        assert!(reg.is_self_write(&std::fs::canonicalize(&file).expect("canonicalize")));
        // 同目录的另一个文件不命中
        assert!(!reg.is_self_write(&dir.path().join("other.mp3")));
    }

    #[test]
    fn relative_path_and_missing_file_use_lexical_fallback() {
        // 该文件**故意不创建**：两侧都走词法规范化，必须仍能互相命中
        let missing = std::env::temp_dir().join("music-robot-watcher-missing-xyz.mp3");
        assert!(!missing.exists(), "本用例前提：路径不存在");
        let reg = SelfWriteRegistry::new(DEFAULT_TTL);
        reg.note_write(&missing);
        assert!(reg.is_self_write(&missing));
        // 带 ./ 的非存在路径同样命中（词法去掉 CurDir）
        let dotted = std::env::temp_dir().join(".").join("music-robot-watcher-missing-xyz.mp3");
        assert!(reg.is_self_write(&dotted));
        // 相对路径：以 cwd 为基准的词法规范化，与绝对写法一致
        let cwd = std::env::current_dir().expect("cwd");
        let abs = cwd.join("music-robot-watcher-rel-xyz.mp3");
        reg.note_write(&abs);
        assert!(reg.is_self_write(Path::new("music-robot-watcher-rel-xyz.mp3")));
    }

    #[test]
    fn tmp_file_detection() {
        assert!(is_our_tmp_file(Path::new("/music/a.mp3.music-robot-tmp-1234-abcd-00ff11")));
        assert!(is_our_tmp_file(Path::new("b.flac.music-robot-tmp-9-1f-000001")));
        assert!(!is_our_tmp_file(Path::new("/music/a.mp3")));
        assert!(!is_our_tmp_file(Path::new("/music/music-robot-tmp")));
        assert!(!is_our_tmp_file(Path::new("/")));
        // 真实的原文件路径不能被当成 tmp（tmp 名只出现在 rename 之前的中间态）
        let dir = TempDir::new("suppress-tmpname");
        let f = dir.path().join("song.mp3");
        std::fs::write(&f, b"x").expect("写文件");
        crate::tag::write::atomic::atomic_replace(&f, b"hello world".to_vec(), None).expect("原子写");
        assert!(!is_our_tmp_file(&f));
    }

    #[test]
    fn registry_is_shareable_across_threads() {
        use std::sync::Arc;
        let reg = Arc::new(SelfWriteRegistry::new(DEFAULT_TTL));
        let dir = TempDir::new("suppress-threads");
        let mut hs = Vec::new();
        for i in 0..8 {
            let reg = Arc::clone(&reg);
            let p = dir.path().join(format!("f{i}.mp3"));
            std::fs::write(&p, b"x").expect("写文件");
            hs.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    reg.note_write(&p);
                    assert!(reg.is_self_write(&p));
                    let _ = reg.len();
                    reg.prune();
                }
            }));
        }
        for h in hs {
            h.join().expect("子线程不 panic");
        }
        assert_eq!(reg.len(), 8);
    }
}
