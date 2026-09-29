//! src/watcher/classify.rs — 文件事件分类：唯一决定「要不要入库」的地方
//!
//! ## 为什么需要分层
//!
//! inotify 只告诉你「目录里这个名字发生了变化」，**不告诉你变化是谁造成的**。
//! `atomic_replace` 的 rename 覆盖会让原文件路径收到 `IN_MOVED_TO`，
//! 与外部真新建文件完全同形（实测见 inotify_poc）。所以「是不是新文件」不能由位掩码
//! 单独决定，必须先过两道抑制闸门，再按 kind 映射。
//!
//! ## 判定顺序（严格，不可调换）
//!
//! 0. 目录 → `Ignore(Directory)`（见下方「与规格的差异」）
//! 1. `is_our_tmp_file` → `Ignore(OurTmpFile)`
//! 2. `reg.is_self_write` → `Ignore(SelfWrite)`
//! 3. 非音频扩展名 → `Ignore(NotAudio)`
//! 4. Created → `NewFile` / Removed → `Removed` / Modified → `MetadataChanged`
//!
//! ## 核心安全不变式（tests 里逐条锁死）
//!
//! > `MetadataChanged` 的语义是「只更新元数据，不动 scrape_status」。
//! > 任何路径下，自写引发的变更都不能产生 `NewFile`。

use std::path::Path;

use super::suppress::{is_our_tmp_file, SelfWriteRegistry};

/// 与 scanner::AUDIO_EXT 保持一致（scanner.rs:38 的常量是私有的，无法直接复用；
/// 这里复制同一份清单，并用 `audio_ext_matches_scanner` 测试对拍防漂移）。
const AUDIO_EXT: &[&str] = &["mp3", "flac", "wav"];

/// 事件在语义上属于哪一类（由 inotify 位掩码翻译而来，见 `watch_kind_from_mask`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    /// 目录里出现了这个名字（IN_CREATE / IN_MOVED_TO）
    Created,
    /// 这个名字消失了（IN_DELETE / IN_MOVED_FROM / IN_DELETE_SELF）
    Removed,
    /// 内容或属性变了（IN_CLOSE_WRITE / IN_MODIFY / IN_ATTRIB）
    Modified,
}

/// 为什么忽略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    /// 本次写回由我们自己发起（注册表命中）
    SelfWrite,
    /// 本项目原子写的中间文件（.music-robot-tmp-）
    OurTmpFile,
    /// 扩展名不在曲库清单里
    NotAudio,
    /// 目录（不是文件）
    Directory,
}

/// 对一个文件事件的处置决定
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchDecision {
    Ignore(IgnoreReason),
    /// 真的新文件 → 入库，scrape_status = pending
    NewFile,
    /// 真的被删 → 标记删除
    Removed,
    /// 内容变了 → **只重读标签，绝不重置 scrape_status**
    MetadataChanged,
}

/// 是否曲库关心的音频文件（大小写不敏感）。
pub fn is_audio_file(path: &Path) -> bool {
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => {
            let lower = ext.to_lowercase();
            AUDIO_EXT.contains(&lower.as_str())
        }
        None => false,
    }
}

/// 把 inotify 位掩码翻译成语义 kind。多个语义位同时置位时按
/// Created > Removed > Modified 取优先级（防止一次误判把删除吞成修改）。
pub fn watch_kind_from_mask(mask: u32) -> Option<WatchKind> {
    const CREATED: u32 = libc::IN_CREATE | libc::IN_MOVED_TO;
    const REMOVED: u32 = libc::IN_DELETE | libc::IN_MOVED_FROM | libc::IN_DELETE_SELF;
    const MODIFIED: u32 = libc::IN_CLOSE_WRITE | libc::IN_MODIFY | libc::IN_ATTRIB;
    if mask & CREATED != 0 {
        Some(WatchKind::Created)
    } else if mask & REMOVED != 0 {
        Some(WatchKind::Removed)
    } else if mask & MODIFIED != 0 {
        Some(WatchKind::Modified)
    } else {
        None
    }
}

/// 分类一个事件。`path` 是**受影响的名字**（目录 + 文件名），不是被 watch 的目录。
pub fn classify(path: &Path, kind: WatchKind, reg: &SelfWriteRegistry) -> WatchDecision {
    // 0) 目录永远不是曲库文件。规格的四步里没有它，放在最前只是为了尽早短路；
    //    它与下面 1→4 的相对顺序无关（无论放哪，抑制闸门都在「按 kind 映射」之前）。
    if path.is_dir() {
        return WatchDecision::Ignore(IgnoreReason::Directory);
    }
    // 1) 本项目 tmp 文件 —— 按名字，不依赖注册表
    if is_our_tmp_file(path) {
        return WatchDecision::Ignore(IgnoreReason::OurTmpFile);
    }
    // 2) 自写抑制 —— 这是「rename 覆盖」与「真新建」之间唯一的区分手段
    if reg.is_self_write(path) {
        return WatchDecision::Ignore(IgnoreReason::SelfWrite);
    }
    // 3) 音频门
    if !is_audio_file(path) {
        return WatchDecision::Ignore(IgnoreReason::NotAudio);
    }
    // 4) 按 kind 映射
    match kind {
        WatchKind::Created => WatchDecision::NewFile,
        WatchKind::Removed => WatchDecision::Removed,
        WatchKind::Modified => WatchDecision::MetadataChanged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::suppress::DEFAULT_TTL;
    use crate::watcher::test_support::TempDir;
    use std::path::PathBuf;
    use std::time::Duration;

    const KINDS: [WatchKind; 3] = [WatchKind::Created, WatchKind::Removed, WatchKind::Modified];

    fn audio(dir: &TempDir, name: &str) -> PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, b"fake audio bytes").expect("写测试文件");
        p
    }

    #[test]
    fn audio_extension_is_case_insensitive_and_closed() {
        assert!(is_audio_file(Path::new("/m/a.mp3")));
        assert!(is_audio_file(Path::new("/m/a.MP3")));
        assert!(is_audio_file(Path::new("/m/a.FlAc")));
        assert!(is_audio_file(Path::new("a.wav")));
        assert!(!is_audio_file(Path::new("/m/a.ogg")));
        assert!(!is_audio_file(Path::new("/m/a.txt")));
        assert!(!is_audio_file(Path::new("/m/a")));
        assert!(!is_audio_file(Path::new("/m/.mp3"))); // 隐藏文件：extension() 为空
        assert!(!is_audio_file(Path::new("/m/")));
    }

    /// 防漂移：我们的扩展名判断必须与 scanner::scan_files 的枚举完全一致
    #[test]
    fn audio_ext_matches_scanner() {
        let dir = TempDir::new("classify-scanner");
        for n in ["a.mp3", "b.MP3", "c.flac", "d.WAV", "e.ogg", "f.txt", "g"] {
            std::fs::write(dir.path().join(n), b"x").expect("写文件");
        }
        std::fs::create_dir_all(dir.path().join("sub.mp3")).expect("建目录");
        let mut scanned: Vec<String> = crate::scanner::scan_files(dir.path())
            .expect("scan_files")
            .iter()
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
            .collect();
        scanned.sort();
        let mut ours: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .flatten()
            .map(|e| e.path())
            .filter(|p| !p.is_dir() && is_audio_file(p))
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
            .collect();
        ours.sort();
        assert_eq!(scanned, ours, "watcher::is_audio_file 与 scanner::scan_files 清单必须一致");
        assert_eq!(scanned, vec!["a.mp3", "b.MP3", "c.flac", "d.WAV"]);
    }

    #[test]
    fn our_tmp_file_wins_over_everything() {
        let dir = TempDir::new("classify-tmp");
        let tmp = dir.path().join("song.mp3.music-robot-tmp-123-abc-000001");
        std::fs::write(&tmp, b"x").expect("写文件");
        let reg = SelfWriteRegistry::new(DEFAULT_TTL);
        reg.note_write(&tmp); // 即使同时命中自写，也必须报 OurTmpFile（顺序 1 先于 2）
        for k in KINDS {
            assert_eq!(classify(&tmp, k, &reg), WatchDecision::Ignore(IgnoreReason::OurTmpFile));
        }
    }

    /// 核心不变式：自写引发的变更在任何 kind 下都不能是 NewFile
    #[test]
    fn self_write_never_becomes_new_file() {
        let dir = TempDir::new("classify-selfwrite");
        let f = audio(&dir, "song.mp3");
        let reg = SelfWriteRegistry::new(DEFAULT_TTL);
        reg.note_write(&f);
        for k in KINDS {
            let d = classify(&f, k, &reg);
            assert_ne!(d, WatchDecision::NewFile, "自写文件在 {k:?} 下被判成了 NewFile！");
            assert_eq!(d, WatchDecision::Ignore(IgnoreReason::SelfWrite));
        }
        // 无扩展名的自写文件：仍是 SelfWrite（顺序 2 先于 3）
        let bare = dir.path().join("cover");
        std::fs::write(&bare, b"x").expect("写文件");
        reg.note_write(&bare);
        assert_eq!(
            classify(&bare, WatchKind::Created, &reg),
            WatchDecision::Ignore(IgnoreReason::SelfWrite)
        );
    }

    #[test]
    fn not_audio_is_ignored_before_kind_mapping() {
        let dir = TempDir::new("classify-nonaudio");
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, b"x").expect("写文件");
        let reg = SelfWriteRegistry::new(DEFAULT_TTL);
        for k in KINDS {
            assert_eq!(classify(&txt, k, &reg), WatchDecision::Ignore(IgnoreReason::NotAudio));
        }
    }

    #[test]
    fn directory_is_ignored() {
        let dir = TempDir::new("classify-dir");
        std::fs::create_dir_all(dir.path().join("album")).expect("建目录");
        std::fs::create_dir_all(dir.path().join("专辑.mp3")).expect("建怪目录");
        let reg = SelfWriteRegistry::new(DEFAULT_TTL);
        assert_eq!(
            classify(&dir.path().join("album"), WatchKind::Created, &reg),
            WatchDecision::Ignore(IgnoreReason::Directory)
        );
        // 目录名带音频扩展名也不能被当成音频
        assert_eq!(
            classify(&dir.path().join("专辑.mp3"), WatchKind::Created, &reg),
            WatchDecision::Ignore(IgnoreReason::Directory)
        );
    }

    #[test]
    fn plain_audio_events_map_by_kind() {
        let dir = TempDir::new("classify-map");
        let f = audio(&dir, "song.flac");
        let reg = SelfWriteRegistry::new(DEFAULT_TTL);
        assert_eq!(classify(&f, WatchKind::Created, &reg), WatchDecision::NewFile);
        assert_eq!(classify(&f, WatchKind::Modified, &reg), WatchDecision::MetadataChanged);
        // Removed：文件已不存在（is_dir 为 false），仍按 kind 映射
        std::fs::remove_file(&f).expect("删文件");
        assert_eq!(classify(&f, WatchKind::Removed, &reg), WatchDecision::Removed);
        assert_eq!(classify(&f, WatchKind::Modified, &reg), WatchDecision::MetadataChanged);
    }

    #[test]
    fn metadata_changed_is_not_a_rescrape_trigger() {
        // 语义断言：MetadataChanged 与 NewFile 是两个不同决定，
        // 调用方据此只重读标签、绝不重置 scrape_status。
        assert_ne!(WatchDecision::MetadataChanged, WatchDecision::NewFile);
        assert_ne!(WatchDecision::MetadataChanged, WatchDecision::Removed);
    }

    #[test]
    fn ttl_expiry_makes_old_self_write_a_real_event_again() {
        let dir = TempDir::new("classify-ttl");
        let f = audio(&dir, "song.wav");
        let reg = SelfWriteRegistry::new(Duration::from_millis(40));
        reg.note_write(&f);
        assert_eq!(classify(&f, WatchKind::Created, &reg), WatchDecision::Ignore(IgnoreReason::SelfWrite));
        std::thread::sleep(Duration::from_millis(70));
        assert_eq!(classify(&f, WatchKind::Created, &reg), WatchDecision::NewFile);
    }

    #[test]
    fn mask_translation() {
        assert_eq!(watch_kind_from_mask(libc::IN_CREATE), Some(WatchKind::Created));
        assert_eq!(watch_kind_from_mask(libc::IN_MOVED_TO), Some(WatchKind::Created));
        assert_eq!(watch_kind_from_mask(libc::IN_DELETE), Some(WatchKind::Removed));
        assert_eq!(watch_kind_from_mask(libc::IN_MOVED_FROM), Some(WatchKind::Removed));
        assert_eq!(watch_kind_from_mask(libc::IN_CLOSE_WRITE), Some(WatchKind::Modified));
        assert_eq!(watch_kind_from_mask(libc::IN_MODIFY), Some(WatchKind::Modified));
        assert_eq!(watch_kind_from_mask(libc::IN_ATTRIB), Some(WatchKind::Modified));
        assert_eq!(watch_kind_from_mask(libc::IN_OPEN), None);
        assert_eq!(watch_kind_from_mask(libc::IN_IGNORED), None);
        // 优先级：Created 压过 Removed/Modified
        assert_eq!(
            watch_kind_from_mask(libc::IN_MOVED_TO | libc::IN_ATTRIB),
            Some(WatchKind::Created)
        );
    }
}
