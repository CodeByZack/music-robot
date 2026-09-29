//! S5 · 服务层 —— 跨模块编排（扫描、刮削、播放……）。
//!
//! 这一层不碰 SQL、不碰标签解析细节：它把「遍历 / 读标签 / 落库」串起来，
//! 并把**安全不变式**集中在这里看守。目前有曲库扫描编排 library 与刮削编排 scrape。

pub mod library;
pub mod scrape;

pub use library::{LibraryError, LibraryService, ScanIssue, ScanReport};
pub use scrape::{
    BatchProgress, BatchReport, BatchRunner, Clock, ProgressSnapshot, ScrapeError, ScrapeIssue,
    ScrapeOutcome, ScrapePlugin, ScrapeService, SystemClock, HIT_CONFIDENCE,
};
