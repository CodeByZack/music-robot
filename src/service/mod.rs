//! S5 · 服务层 —— 跨模块编排（扫描、刮削、播放……）。
//!
//! 这一层不碰 SQL、不碰标签解析细节：它把「遍历 / 读标签 / 落库」串起来，
//! 并把**安全不变式**集中在这里看守。目前有曲库扫描编排 library、刮削编排 scrape，
//! 以及手工编辑标签的 tag_edit。

pub mod library;
pub mod scrape;
pub mod tag_edit;

pub use library::{LibraryError, LibraryService, ScanIssue, ScanReport};
pub use scrape::{
    BatchProgress, BatchReport, BatchRunner, Clock, ProgressSnapshot, ScrapeError, ScrapeIssue,
    ScrapeOutcome, ScrapePlugin, ScrapeProposal, ScrapeQuery, ScrapeService, SystemClock,
    HIT_CONFIDENCE,
};
/// 查询结果内联封面的上限（路由层用它决定要不要内联 + 给用户实话）。
pub use scrape::max_embed_cover_bytes;
pub use tag_edit::{CoverOp, EditResult, TagEditError, TagEditService};
