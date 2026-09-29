//! 数据库层：连接池、版本化迁移、表模型、仓库。
//!
//! 四层职责：
//!   * [`pool`]       —— 管理若干条已开好 PRAGMA 的 SQLite 连接，借出 / 归还（RAII）；
//!   * [`migrations`] —— schema 版本表 + 有序升级路径，启动时调用一次；
//!   * [`models`]     —— 10 张表对应的 Rust 结构体与枚举（不含 SQL）；
//!   * [`repos`]      —— 各表的读写（S3）。方法一律取 `&Connection`，
//!     这样调用方能把多个写操作组合进同一个事务（`Transaction` 解引用成 `Connection`）。
//!
//! 不引入 serde derive：需要 JSON 时手写 `serde_json::Value`，这是项目一贯规范。
//!
//! ## 软删除约定（重要）
//!
//! `songs.deleted_at` 为 NULL = 在库；非 NULL = 该时刻发现磁盘上已消失。
//! **面向用户的查询一律要过滤 `deleted_at IS NULL`** —— 各 repo 的 `include_deleted`
//! 参数默认 `false` 就是这个语义。
//!
//! 磁盘上的文件不见了要用 `repos::songs::mark_deleted`（文件回来时 `restore`），
//! **绝不要**改用 `repos::songs::purge`：物理删会按外键动作连带清掉用户的歌单条目、
//! 收藏与播放历史，移动硬盘临时没挂载就会造成不可逆的数据损失。
//!
//! ## 时间戳统一约定
//!
//! 所有 `*_at` / `file_mtime` 列都是 **INTEGER，Unix 毫秒**（[`now_unix_ms`]）。
//! 不用 SQLite 的 TEXT 时间串：数值可直接比较、排序、参与算术，也不受
//! `datetime()` 本地时区影响。`duration_ms` / `duration_listened_ms` 本就是毫秒，
//! 同单位不用换算。

pub mod migrations;
pub mod models;
pub mod pool;
pub mod repos;

/// 当前 Unix 毫秒时间戳。
///
/// 系统时钟早于 1970-01-01（几乎只能是时钟没校准）时返回 0，而不是 panic ——
/// 时间戳不该成为让服务起不来的理由。
pub fn now_unix_ms() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => 0,
    }
}
