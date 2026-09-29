//! S2 · schema 版本表与升级路径。
//!
//! 为什么不能只靠 `CREATE TABLE IF NOT EXISTS`：schema 已经改过 3 轮，
//! `IF NOT EXISTS` 只能保证「表存在」，既不知道库里现在是第几版，也没法把
//! 「第 2 轮新增的列」补到「第 1 轮建出来的旧库」上。所以这里用一张版本表
//! [`VERSION_TABLE`] 加一个有序迁移列表：
//!
//!   * [`MIGRATIONS`] 是唯一事实来源，版本号严格递增；
//!   * [`apply`] 只跑「版本号大于当前已应用版本」的迁移，已是最新则纯空操作；
//!   * 每个迁移**单独一个事务**：DDL 与版本号写入要么一起成功、要么一起回滚，
//!     绝不会留下「表建了但版本没记」的半吊子状态；
//!   * [`validate`] 在 apply 前拦住重复 / 倒序的迁移列表。
//!
//! 迁移列表只增不改：**已经发布过的迁移，其 SQL 一个字都不能再动**，
//! 要改 schema 就追加一条新版本。
//!
//! ## 时间戳
//!
//! 版本表的 `applied_at` 与全部业务时间列同单位：**Unix 毫秒**（[`crate::db::now_unix_ms`]）。

use rusqlite::{params, Connection};

/// 版本表名（schema 自省与测试都会用到）
pub const VERSION_TABLE: &str = "schema_migrations";

/// 一个迁移步骤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Migration {
    /// 版本号，从 1 开始严格递增
    pub version: i64,
    /// 迁移名，写进版本表，方便直接用 sqlite3 看历史
    pub name: &'static str,
    /// 该版本要执行的 SQL（可含多条语句）
    pub sql: &'static str,
}

/// 一条已经应用过的迁移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppliedMigration {
    /// 版本号
    pub version: i64,
    /// 迁移名
    pub name: &'static str,
}

/// 一次 [`apply`] 的结果，供启动日志使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// 执行前的版本
    pub from_version: i64,
    /// 执行后的目标版本
    pub to_version: i64,
    /// 本次真正执行的迁移（空 = 已经是最新）
    pub applied: Vec<AppliedMigration>,
}

impl MigrationReport {
    /// 本次是否真的改了 schema
    pub fn changed(&self) -> bool {
        !self.applied.is_empty()
    }
}

/// 「当前版本 / 目标版本」快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationStatus {
    /// 库里已应用的版本（空库为 0）
    pub current: i64,
    /// 本程序支持的最高版本
    pub target: i64,
}

impl MigrationStatus {
    /// 库里是否已经是最新
    pub fn is_up_to_date(&self) -> bool {
        self.current >= self.target
    }

    /// 还差几个版本（当前比目标新时为 0）
    pub fn pending(&self) -> i64 {
        (self.target - self.current).max(0)
    }
}

/// 迁移相关错误。
#[derive(Debug)]
pub enum MigrationError {
    /// 底层 SQLite 错误
    Sqlite(rusqlite::Error),
    /// 迁移列表里出现重复版本号
    Duplicate {
        /// 出错条目在列表里的下标（从 0 开始）
        index: usize,
        /// 重复的版本号
        version: i64,
    },
    /// 迁移列表版本号不是严格递增
    NonIncreasing {
        /// 出错条目在列表里的下标（从 0 开始）
        index: usize,
        /// 上一条的版本号
        previous: i64,
        /// 本条的版本号
        version: i64,
    },
    /// 版本号必须 >= 1
    NonPositiveVersion {
        /// 出错条目在列表里的下标（从 0 开始）
        index: usize,
        /// 非法的版本号
        version: i64,
    },
    /// 库里的版本比本程序支持的还新（拿旧二进制去开新库）
    FutureSchema {
        /// 库里的版本
        current: i64,
        /// 本程序支持的最高版本
        supported: i64,
    },
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::Sqlite(e) => write!(f, "执行数据库迁移失败：{e}"),
            MigrationError::Duplicate { index, version } => {
                write!(f, "迁移列表下标 {index} 的版本号重复：{version}")
            }
            MigrationError::NonIncreasing { index, previous, version } => write!(
                f,
                "迁移列表版本号必须严格递增：下标 {index} 为 {version}，其上一条已经是 {previous}"
            ),
            MigrationError::NonPositiveVersion { index, version } => {
                write!(f, "迁移列表下标 {index} 的版本号必须 >= 1，实际为 {version}")
            }
            MigrationError::FutureSchema { current, supported } => write!(
                f,
                "数据库 schema 版本为 {current}，高于本程序支持的最高版本 {supported}：请升级程序，不要用旧版本打开新库"
            ),
        }
    }
}

impl std::error::Error for MigrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MigrationError::Sqlite(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for MigrationError {
    fn from(e: rusqlite::Error) -> Self {
        MigrationError::Sqlite(e)
    }
}

/// 全部迁移，按版本号严格递增排列。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: INITIAL_SCHEMA,
    },
    Migration {
        version: 2,
        name: "songs_soft_delete",
        sql: MIGRATION_2_SONGS_SOFT_DELETE,
    },
    Migration {
        version: 3,
        name: "songs_audio_hash_index",
        sql: MIGRATION_3_SONGS_AUDIO_HASH_INDEX,
    },
];

/// 第 1 版迁移：画布 ⑨ 的 10 张表 + 索引。
///
/// 设计约定（画布明确）：
///   * **不建 artists 表** —— 歌手名存 `songs.artists`，列表查询 GROUP BY 现算；
///   * **派生字段一律不落库** —— 例如 albums 不存 song_count，查询时 GROUP BY 现算；
///   * `albums` 的 UNIQUE(name, album_artist) 要生效，`album_artist` 必须 NOT NULL，
///     未知歌手统一写空串（SQLite 的 UNIQUE 不把多个 NULL 视为冲突）。
///
/// 外键动作：指向 `users` 的一律 CASCADE（删用户清干净其数据）；
/// `songs.album_id` / `song_requests.song_id` 用 SET NULL（删专辑/歌曲不该连带删歌单或需求）；
/// `playlist_items` / `request_votes` 用 CASCADE（画布明确）。
///
/// 这里刻意**不写 IF NOT EXISTS**：迁移只跑一次由版本表保证，
/// 一旦版本追踪失效，重跑会立刻报「table already exists」而不是被静默吞掉。
const INITIAL_SCHEMA: &str = r#"
-- ── 用户 ─────────────────────────────────────────────────────────────
CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT    NOT NULL,
    password_hash TEXT    NOT NULL,
    role          TEXT    NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
    created_at    INTEGER NOT NULL,
    last_login    INTEGER,
    UNIQUE (username)
);

-- ── 专辑（不存 song_count，查询时 GROUP BY 现算）─────────────────────
CREATE TABLE albums (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL,
    album_artist TEXT    NOT NULL DEFAULT '',
    year         INTEGER,
    cover_data   BLOB,
    cover_mime   TEXT,
    updated_at   INTEGER NOT NULL,
    UNIQUE (name, album_artist)
);

-- ── 歌曲（核心表）────────────────────────────────────────────────────
CREATE TABLE songs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path     TEXT    NOT NULL UNIQUE,
    album_id      INTEGER REFERENCES albums(id) ON DELETE SET NULL,
    title         TEXT,
    artists       TEXT,
    album_artist  TEXT,
    year          INTEGER,
    genres        TEXT,
    track         INTEGER,
    disc          INTEGER,
    duration_ms   INTEGER,
    bitrate_bps   INTEGER,
    format        TEXT,
    audio_hash    TEXT,
    file_size     INTEGER,
    file_mtime    INTEGER,
    search_text   TEXT,
    lyrics        TEXT,
    scrape_status TEXT    NOT NULL DEFAULT 'pending'
                          CHECK (scrape_status IN ('pending', 'processing', 'done', 'failed')),
    scrape_error  TEXT,
    scrape_at     INTEGER,
    added_at      INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
CREATE INDEX idx_songs_format        ON songs(format);
CREATE INDEX idx_songs_artists       ON songs(artists);
CREATE INDEX idx_songs_album_id      ON songs(album_id);
CREATE INDEX idx_songs_added_at      ON songs(added_at);
CREATE INDEX idx_songs_scrape_status ON songs(scrape_status);

-- ── 歌单 ─────────────────────────────────────────────────────────────
CREATE TABLE playlists (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    description TEXT,
    is_public   INTEGER NOT NULL DEFAULT 0 CHECK (is_public IN (0, 1)),
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX idx_playlists_user ON playlists(user_id);

-- ── 歌单条目 ─────────────────────────────────────────────────────────
CREATE TABLE playlist_items (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    song_id     INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, song_id)
);
CREATE INDEX idx_playlist_items_song ON playlist_items(song_id);

-- ── 收藏 ─────────────────────────────────────────────────────────────
CREATE TABLE favorites (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    song_id    INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    UNIQUE (user_id, song_id)
);
CREATE INDEX idx_favorites_song ON favorites(song_id);

-- ── 播放历史 ─────────────────────────────────────────────────────────
CREATE TABLE play_history (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id              INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    song_id              INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
    played_at            INTEGER NOT NULL,
    duration_listened_ms INTEGER
);
CREATE INDEX idx_play_history_user_time ON play_history(user_id, played_at);
CREATE INDEX idx_play_history_song      ON play_history(song_id);

-- ── 用户设置（断点续播 position_ms · 音量 · 播放模式）────────────────
CREATE TABLE user_settings (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    key     TEXT    NOT NULL,
    value   TEXT,
    PRIMARY KEY (user_id, key)
);

-- ── 点歌请求 ─────────────────────────────────────────────────────────
CREATE TABLE song_requests (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title         TEXT    NOT NULL,
    artist        TEXT,
    album         TEXT,
    note          TEXT,
    dedup_key     TEXT    NOT NULL,
    status        TEXT    NOT NULL DEFAULT 'pending'
                          CHECK (status IN ('pending', 'processing', 'done', 'rejected')),
    reject_reason TEXT,
    song_id       INTEGER REFERENCES songs(id) ON DELETE SET NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);
-- dedup_key = normalize(title) + normalize(artist)：画布要求「命中已有则不新建」，
-- 用唯一索引在库层面兜底，避免并发点歌时插入两条相同请求。
CREATE UNIQUE INDEX idx_song_requests_dedup ON song_requests(dedup_key);
CREATE INDEX idx_song_requests_status ON song_requests(status);
CREATE INDEX idx_song_requests_song   ON song_requests(song_id);

-- ── 点歌投票 ─────────────────────────────────────────────────────────
CREATE TABLE request_votes (
    request_id INTEGER NOT NULL REFERENCES song_requests(id) ON DELETE CASCADE,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (request_id, user_id)
);
CREATE INDEX idx_request_votes_user ON request_votes(user_id);
"#;

/// 第 2 版迁移：给 songs 加**软删除标记**。
///
/// 画布区块 ③ 的流程规则写着「磁盘消失 → 标记删除」，且配套一条安全规则
/// 「扫描结果为空 / 根目录读取失败 → **绝不**触发标记删除」；但画布 ⑨ 的
/// songs 列清单里**漏了承载这个标记的列**。这里补上。
///
/// 为什么是软删除而不是真删：playlist_items.song_id、favorites.song_id、
/// play_history.song_id 都指向 songs。真删一行会按外键动作把用户的歌单条目、
/// 收藏、播放历史一并带走 —— 移动硬盘临时没挂载就会造成不可逆的用户数据损失。
/// 标记删除则「文件回来就复原」，代价只是查询要带 deleted_at IS NULL。
///
/// 语义：deleted_at 为 NULL = 在库；非 NULL = 该时刻发现磁盘上已不存在。
/// 用时间戳而不是布尔，是因为「什么时候消失的」对排查有价值，且与全库其它
/// *_at 列保持同一约定（INTEGER Unix 毫秒）。
///
/// 刻意**不建索引**：绝大多数查询是 deleted_at IS NULL（命中几乎全表，
/// 低选择性，索引用不上）；唯一会受益的是「列出已删除」这种罕见的管理查询。
/// 等真有性能问题再补，避免为想象中的查询付写入代价。
const MIGRATION_2_SONGS_SOFT_DELETE: &str = r#"
ALTER TABLE songs ADD COLUMN deleted_at INTEGER;
"#;

/// 第 3 版迁移：给 songs.audio_hash 建索引。
///
/// 为什么需要：S6 的多根去重对**每个新文件**都要做一次
/// `WHERE audio_hash = ?1 AND deleted_at IS NULL`。没有索引就是全表扫，
/// 首次扫描整体退化成 O(文件数 x 行数) —— 一万首的库就是上亿次行访问。
///
/// 为什么**不能**是 UNIQUE：同一份音频在历史上允许多行（例如一条已软删、
/// 一条在库；S6 自己也刻意保证「不软删的只有一条」而不是靠库约束）。
/// 加 UNIQUE 会让「软删行 + 在库行同哈希」这种合法状态插不进去。
const MIGRATION_3_SONGS_AUDIO_HASH_INDEX: &str = r#"
CREATE INDEX idx_songs_audio_hash ON songs(audio_hash);
"#;

/// 校验迁移列表：版本号必须 >= 1 且严格递增。
///
/// [`apply`] 会在动数据库之前先调用它，所以坏的迁移列表永远跑不到 DDL。
pub fn validate(list: &[Migration]) -> Result<(), MigrationError> {
    let mut previous: Option<i64> = None;
    for (index, m) in list.iter().enumerate() {
        if m.version <= 0 {
            return Err(MigrationError::NonPositiveVersion { index, version: m.version });
        }
        if let Some(prev) = previous {
            if m.version == prev {
                return Err(MigrationError::Duplicate { index, version: m.version });
            }
            if m.version < prev {
                return Err(MigrationError::NonIncreasing {
                    index,
                    previous: prev,
                    version: m.version,
                });
            }
        }
        previous = Some(m.version);
    }
    Ok(())
}

/// 本程序支持的最高版本（迁移列表为空时为 0）。
pub fn target_version() -> i64 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

/// 读库里当前已应用的版本；版本表还不存在（全新库）时返回 0。
pub fn current_version(conn: &Connection) -> Result<i64, MigrationError> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![VERSION_TABLE],
        |row| row.get(0),
    )?;
    if exists == 0 {
        return Ok(0);
    }
    let max: Option<i64> = conn.query_row(
        &format!("SELECT MAX(version) FROM {VERSION_TABLE}"),
        [],
        |row| row.get(0),
    )?;
    Ok(max.unwrap_or(0))
}

/// 当前版本 / 目标版本快照，方便启动时打一行日志。
pub fn status(conn: &Connection) -> Result<MigrationStatus, MigrationError> {
    Ok(MigrationStatus {
        current: current_version(conn)?,
        target: target_version(),
    })
}

/// 库是否已经是最新版本。
pub fn is_up_to_date(conn: &Connection) -> Result<bool, MigrationError> {
    Ok(status(conn)?.is_up_to_date())
}

/// 应用全部未执行的迁移（[`MIGRATIONS`]）。
///
/// 每个迁移一个事务；已是最新时返回空的 report，不改动任何数据。
pub fn apply(conn: &mut Connection) -> Result<MigrationReport, MigrationError> {
    apply_list(conn, MIGRATIONS)
}

/// 应用给定的迁移列表（测试用合成列表时直接走这里）。
///
/// 只执行「version 大于当前已应用版本」的迁移；重复调用幂等。
pub fn apply_list(
    conn: &mut Connection,
    list: &[Migration],
) -> Result<MigrationReport, MigrationError> {
    validate(list)?;
    ensure_version_table(conn)?;

    let from_version = current_version(conn)?;
    let to_version = list.last().map(|m| m.version).unwrap_or(0);
    if from_version > to_version {
        return Err(MigrationError::FutureSchema {
            current: from_version,
            supported: to_version,
        });
    }

    let mut applied = Vec::new();
    for m in list {
        if m.version <= from_version {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.execute(
            &format!(
                "INSERT INTO {VERSION_TABLE} (version, name, applied_at) VALUES (?1, ?2, ?3)"
            ),
            params![m.version, m.name, crate::db::now_unix_ms()],
        )?;
        tx.commit()?;
        applied.push(AppliedMigration {
            version: m.version,
            name: m.name,
        });
    }

    Ok(MigrationReport {
        from_version,
        to_version,
        applied,
    })
}

/// 建版本表（本身不纳入版本管理，用 IF NOT EXISTS 保证可重复调用）。
fn ensure_version_table(conn: &Connection) -> Result<(), MigrationError> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {VERSION_TABLE} (
            version    INTEGER PRIMARY KEY,
            name       TEXT    NOT NULL,
            applied_at INTEGER NOT NULL
        );"
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::pool::{DbPool, TempDb};
    use rusqlite::Connection;

    /// 画布 ⑨ 要求的 10 张表
    const CANVAS_TABLES: [&str; 10] = [
        "users",
        "albums",
        "songs",
        "playlists",
        "playlist_items",
        "favorites",
        "play_history",
        "user_settings",
        "song_requests",
        "request_votes",
    ];

    fn temp_pool(tag: &str) -> (DbPool, TempDb) {
        DbPool::open_temp(tag).expect("建临时文件库池")
    }

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .expect("准备 sqlite_master 查询");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("查表名");
        rows.map(|r| r.expect("读表名")).collect()
    }

    fn row_count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .expect("计数")
    }

    /// 某张表的索引名（自省用）。
    fn index_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1")
            .expect("准备索引自省");
        let rows = stmt.query_map(params![table], |row| row.get::<_, String>(0)).expect("查索引名");
        rows.map(|r| r.expect("读索引名")).collect()
    }

    /// 某张表的列名（自省用）。
    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("准备列自省");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("查列名");
        rows.map(|r| r.expect("读列名")).collect()
    }

    #[test]
    fn migration_list_is_strictly_increasing_and_constants_are_sane() {
        assert!(validate(MIGRATIONS).is_ok(), "内置迁移列表必须合法");
        assert_eq!(MIGRATIONS.first().expect("至少一条迁移").version, 1);
        assert_eq!(
            target_version(),
            MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0)
        );
        assert!(target_version() >= 1);
    }

    #[test]
    fn validate_rejects_duplicate_and_non_increasing_versions() {
        let dup = [
            Migration { version: 1, name: "a", sql: "" },
            Migration { version: 1, name: "b", sql: "" },
        ];
        assert!(matches!(
            validate(&dup),
            Err(MigrationError::Duplicate { index: 1, version: 1 })
        ));

        let dec = [
            Migration { version: 2, name: "a", sql: "" },
            Migration { version: 1, name: "b", sql: "" },
        ];
        assert!(matches!(
            validate(&dec),
            Err(MigrationError::NonIncreasing { index: 1, previous: 2, version: 1 })
        ));

        let zero = [Migration { version: 0, name: "a", sql: "" }];
        assert!(matches!(
            validate(&zero),
            Err(MigrationError::NonPositiveVersion { index: 0, version: 0 })
        ));

        assert!(validate(&[]).is_ok(), "空列表没有可校验的东西");
    }

    #[test]
    fn first_apply_creates_all_ten_canvas_tables() {
        let (pool, _tmp) = temp_pool("mig-create");
        let mut guard = pool.acquire().expect("借连接");
        assert_eq!(current_version(&guard).expect("读版本"), 0, "全新库应无版本表");

        let report = apply(&mut guard).expect("首次迁移");
        assert_eq!(report.from_version, 0);
        assert_eq!(report.to_version, target_version());
        assert_eq!(report.applied.len(), MIGRATIONS.len());
        assert!(report.changed());

        let names = table_names(&guard);
        for table in CANVAS_TABLES {
            assert!(names.iter().any(|n| n == table), "缺表 {table}：{names:?}");
        }
        assert!(names.iter().any(|n| n == VERSION_TABLE));
        assert!(
            !names.iter().any(|n| n.starts_with("sqlite_")),
            "自省查询不该把 SQLite 内部表算进来：{names:?}"
        );
        assert_eq!(row_count(&guard, VERSION_TABLE), MIGRATIONS.len() as i64);
        assert!(is_up_to_date(&guard).expect("读状态"));
    }



    /// 迁移 v3 建的 audio_hash 索引：不仅要存在，还要**真的被查询计划用上** ——
    /// 建了索引但规划器不走，等于白建。
    #[test]
    fn audio_hash_index_exists_and_the_lookup_actually_uses_it() {
        let (pool, _tmp) = temp_pool("mig-hash-index");
        let mut guard = pool.acquire().expect("借连接");
        apply(&mut guard).expect("迁移到最新");

        let idx = index_names(&guard, "songs");
        assert!(
            idx.iter().any(|n| n == "idx_songs_audio_hash"),
            "songs 缺 audio_hash 索引：{idx:?}"
        );

        // S6 去重用的正是这条查询，规划器必须走索引而不是全表扫
        let plan: String = guard
            .query_row(
                "EXPLAIN QUERY PLAN SELECT id FROM songs WHERE audio_hash = ?1 AND deleted_at IS NULL",
                params!["deadbeef"],
                |row| row.get(3),
            )
            .expect("取查询计划");
        assert!(
            plan.contains("idx_songs_audio_hash"),
            "audio_hash 查询没走索引，退化成全表扫：{plan}"
        );
    }

    /// 真实的 v1 → v2 升级路径：只建到 v1、塞一行、再升到 v2。
    /// 断言新列出现，且**升级前的数据没丢**（ALTER TABLE ADD COLUMN 不该动已有行）。
    #[test]
    fn upgrading_from_v1_to_v2_adds_deleted_at_without_losing_rows() {
        let (pool, _tmp) = temp_pool("mig-upgrade-v2");
        let mut guard = pool.acquire().expect("借连接");

        // 1) 只应用第 1 版
        let first = apply_list(&mut guard, &MIGRATIONS[..1]).expect("应用 v1");
        assert_eq!(first.to_version, 1);
        assert!(
            !column_names(&guard, "songs").iter().any(|c| c == "deleted_at"),
            "v1 的 songs 不该有 deleted_at"
        );

        // 2) 塞一行 v1 时代的数据
        guard
            .execute(
                "INSERT INTO songs (file_path, title, added_at, updated_at) \
                 VALUES ('/m/a.mp3', '老歌', 111, 222)",
                [],
            )
            .expect("插入 v1 数据");

        // 3) 升级到完整列表（v1 + v2）
        let second = apply_list(&mut guard, MIGRATIONS).expect("升级到 v2");
        assert_eq!(second.from_version, 1);
        assert_eq!(second.applied.len(), MIGRATIONS.len() - 1, "只该跑 v2");
        assert_eq!(second.applied[0].version, 2);

        // 4) 新列在，旧行数据完好
        assert!(
            column_names(&guard, "songs").iter().any(|c| c == "deleted_at"),
            "升级后 songs 必须多出 deleted_at"
        );
        let (title, deleted): (String, Option<i64>) = guard
            .query_row(
                "SELECT title, deleted_at FROM songs WHERE file_path = '/m/a.mp3'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("读回升级前的行");
        assert_eq!(title, "老歌", "升级不该丢数据");
        assert_eq!(deleted, None, "新列默认应为 NULL（在库）");
    }

    #[test]
    fn second_apply_is_a_noop_and_does_not_touch_version_table() {
        let (pool, _tmp) = temp_pool("mig-idempotent");
        let mut guard = pool.acquire().expect("借连接");
        let first = apply(&mut guard).expect("首次迁移");
        assert!(first.changed());
        let rows_after_first = row_count(&guard, VERSION_TABLE);

        // 迁移 SQL 里刻意没有 IF NOT EXISTS：一旦版本追踪失效，重跑必定撞 table exists
        let second = apply(&mut guard).expect("第二次迁移必须空操作");
        assert!(
            second.applied.is_empty(),
            "不该重复执行迁移：{:?}",
            second.applied
        );
        assert!(!second.changed());
        assert_eq!(second.from_version, first.to_version);
        assert_eq!(second.to_version, first.to_version);
        assert_eq!(row_count(&guard, VERSION_TABLE), rows_after_first);
        assert_eq!(current_version(&guard).expect("读版本"), target_version());
    }

    #[test]
    fn apply_list_skips_applied_versions_and_runs_only_new_ones() {
        let (pool, _tmp) = temp_pool("mig-skip");
        let mut guard = pool.acquire().expect("借连接");
        let v1 = Migration {
            version: 1,
            name: "alpha",
            sql: "CREATE TABLE alpha (id INTEGER PRIMARY KEY);",
        };
        let v2 = Migration {
            version: 2,
            name: "beta",
            sql: "CREATE TABLE beta (id INTEGER PRIMARY KEY);",
        };

        let r1 = apply_list(&mut guard, &[v1]).expect("只应用 v1");
        assert_eq!(
            r1.applied,
            vec![AppliedMigration { version: 1, name: "alpha" }]
        );

        let r2 = apply_list(&mut guard, &[v1, v2]).expect("再应用 v1 + v2");
        assert_eq!(r2.from_version, 1);
        assert_eq!(r2.to_version, 2);
        assert_eq!(
            r2.applied,
            vec![AppliedMigration { version: 2, name: "beta" }],
            "已应用的 v1 必须被跳过"
        );
        assert_eq!(current_version(&guard).expect("读版本"), 2);

        let names = table_names(&guard);
        assert!(names.iter().any(|n| n == "alpha"));
        assert!(names.iter().any(|n| n == "beta"));
    }

    #[test]
    fn old_binary_refuses_to_open_a_newer_schema() {
        let (pool, _tmp) = temp_pool("mig-future");
        let mut guard = pool.acquire().expect("借连接");
        apply(&mut guard).expect("迁移");
        guard
            .execute(
                &format!(
                    "INSERT INTO {VERSION_TABLE} (version, name, applied_at) VALUES (999, 'from_future', 0)"
                ),
                [],
            )
            .expect("伪造未来版本");

        match apply(&mut guard) {
            Err(MigrationError::FutureSchema {
                current: 999,
                supported,
            }) => assert_eq!(supported, target_version()),
            other => panic!("应当拒绝用旧程序打开新库，实际 {other:?}"),
        }
    }

    #[test]
    fn migration_error_display_is_chinese() {
        let cases: Vec<MigrationError> = vec![
            MigrationError::Duplicate { index: 1, version: 3 },
            MigrationError::NonIncreasing { index: 2, previous: 5, version: 4 },
            MigrationError::NonPositiveVersion { index: 0, version: 0 },
            MigrationError::FutureSchema { current: 9, supported: 1 },
        ];
        for e in &cases {
            let text = e.to_string();
            assert!(!text.is_empty());
            assert!(
                text.contains("迁移") || text.contains("schema"),
                "应为中文错误：{text}"
            );
        }
        let _boxed: Box<dyn std::error::Error> =
            Box::new(MigrationError::FutureSchema { current: 9, supported: 1 });
    }
}
