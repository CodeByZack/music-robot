//! src/watcher/** — 曲库文件监听层（服务端）
//!
//! 背景：曲库目录被 inotify 监视，文件新增入库、删除标记、内容变只重读标签。
//! 危险点：标签写回走 `atomic_replace`（copy → write → verify → **rename 覆盖**），
//! rename 会在**原文件路径**上产生 `IN_MOVED_TO` —— 与外部真的新建一个文件
//! 在位掩码上完全一致（实测序列见 inotify_poc 的打印）。监听器若据此入库并把
//! scrape_status 重置为 pending，就会形成「刮削 → 写回 → 入库 → pending → 再刮削」死循环。
//!
//! 本模块给出三道闸门（顺序见 classify）：
//!   · `suppress::is_our_tmp_file`   —— 本项目 tmp 命名（.music-robot-tmp-），按名忽略
//!   · `suppress::SelfWriteRegistry` —— 自己刚写过的路径，TTL 内忽略
//!   · `classify`                    —— 事件 → 决定（Ignore / NewFile / Removed / MetadataChanged）
//!
//! 核心不变式（classify 的测试逐条锁死）：
//!   任何路径下，自写引发的变更都不能判成 `NewFile`。

pub mod classify;
pub mod suppress;
pub mod watch;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod inotify_poc;

pub use classify::{classify, is_audio_file, watch_kind_from_mask, IgnoreReason, WatchDecision, WatchKind};
pub use suppress::{is_our_tmp_file, SelfWriteRegistry, DEFAULT_TTL, TMP_MARKER};
pub use watch::{LibraryWatcher, WatchConfig, WatchError, WatchEvent, WatchPoll};
