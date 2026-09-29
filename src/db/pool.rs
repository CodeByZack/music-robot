//! S2 · SQLite 连接池 —— 同步实现的「借 - 用 - 还」句柄池。
//!
//! ## 为什么需要它
//!
//! rusqlite 是同步 API，SQLite 连接本身也不是随便跨线程共享的对象。
//! 这个池做两件事：
//!
//!   1. 限制同时在用的连接数（句柄数量由 DbPoolConfig::size 配置）；
//!   2. **保证每条连接都开好 PRAGMA**，尤其是 foreign_keys —— 它是**连接级**开关，
//!      默认 OFF，漏开时 ON DELETE CASCADE 会静默失效（删歌单不会删条目）。
//!
//! ## 实现取舍
//!
//! 连接在 DbPool::with_config 时**一次性建好**，之后借出 / 归还不新建也不关闭。
//! 这么做的原因：
//!   * 池很小（默认 4），SQLite 开连接很便宜；
//!   * 借出时无需再走打开路径，也就不存在「一半线程在开库、一半在等锁」的竞态；
//!   * 每条连接天然独占一个 std::sync::Mutex，guard 直接就是 MutexGuard，
//!     全安全代码，不需要 ManuallyDrop / unsafe。
//!
//! 空闲连接索引放在 std::sync::Mutex 里，配上 std::sync::Condvar 做等待与唤醒。
//!
//! ## :memory: 的限制
//!
//! SQLite 的每条 :memory: 连接都是一个**互相独立**的库，所以
//! DbPool::open_in_memory 强制 size = 1。要测多连接并发，请用
//! DbPool::open_temp（临时文件库）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rusqlite::Connection;

/// 默认连接数。SQLite 写是串行的，读可以并发；4 条对小 NAS 足够。
pub const DEFAULT_POOL_SIZE: usize = 4;

/// 默认 busy_timeout：并发写时先等一会儿，而不是立刻抛 SQLITE_BUSY。
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// 默认借出等待上限：池满且迟迟不归还时的兜底。
pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// 池的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbPoolConfig {
    /// 连接数（必须 >= 1）
    pub size: usize,
    /// 每条连接的 busy_timeout
    pub busy_timeout: Duration,
    /// DbPool::acquire 的默认等待上限
    pub acquire_timeout: Duration,
}

impl Default for DbPoolConfig {
    fn default() -> Self {
        DbPoolConfig {
            size: DEFAULT_POOL_SIZE,
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
            acquire_timeout: DEFAULT_ACQUIRE_TIMEOUT,
        }
    }
}

/// 连接池相关错误。
#[derive(Debug)]
pub enum DbPoolError {
    /// 打开数据库连接失败
    Open {
        /// 数据库路径
        path: PathBuf,
        /// 底层错误
        source: rusqlite::Error,
    },
    /// 设置连接级 PRAGMA 失败
    Pragma {
        /// PRAGMA 名
        name: &'static str,
        /// 底层错误
        source: rusqlite::Error,
    },
    /// 外键开关读回来不是 1（设置被静默忽略）
    ForeignKeysOff {
        /// 数据库路径
        path: PathBuf,
    },
    /// 池已关闭
    Closed,
    /// 等待空闲连接超时
    Timeout(Duration),
    /// 临时库文件创建失败
    TempIo {
        /// 调用方给的标签
        tag: String,
        /// 底层 IO 错误
        source: std::io::Error,
    },
    /// 连接数配置为 0
    BadCapacity,
}

impl std::fmt::Display for DbPoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbPoolError::Open { path, source } => {
                write!(f, "打开 SQLite 数据库失败（{}）：{source}", path.display())
            }
            DbPoolError::Pragma { name, source } => {
                write!(f, "设置 SQLite 连接 PRAGMA {name} 失败：{source}")
            }
            DbPoolError::ForeignKeysOff { path } => write!(
                f,
                "SQLite 连接的外键约束未生效（PRAGMA foreign_keys != 1）：{}",
                path.display()
            ),
            DbPoolError::Closed => write!(f, "数据库连接池已关闭"),
            DbPoolError::Timeout(d) => {
                write!(f, "等待空闲数据库连接超时（{} ms）", d.as_millis())
            }
            DbPoolError::TempIo { tag, source } => {
                write!(f, "创建临时数据库文件失败（标签 {tag}）：{source}")
            }
            DbPoolError::BadCapacity => write!(f, "数据库连接池的连接数必须 >= 1"),
        }
    }
}

impl std::error::Error for DbPoolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbPoolError::Open { source, .. } | DbPoolError::Pragma { source, .. } => Some(source),
            DbPoolError::TempIo { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 池的即时统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbPoolStats {
    /// 池容量
    pub size: usize,
    /// 空闲（可立即借出）的连接数
    pub idle: usize,
    /// 已借出的连接数
    pub busy: usize,
}

/// 一个连接槽位。锁的存在让每条连接在任意时刻只被一个 guard 持有。
struct Slot {
    conn: Mutex<Connection>,
}

/// SQLite 连接池，Send + Sync，可放进 Arc 给多线程共享。
pub struct DbPool {
    path: PathBuf,
    cfg: DbPoolConfig,
    slots: Vec<Slot>,
    free: Mutex<Vec<usize>>,
    cv: Condvar,
    closed: AtomicBool,
}

impl DbPool {
    /// 用默认配置建池（默认 4 条连接）。
    pub fn new(path: impl AsRef<Path>) -> Result<DbPool, DbPoolError> {
        DbPool::with_config(path, DbPoolConfig::default())
    }

    /// 指定连接数建池，其余走默认值。
    pub fn with_size(path: impl AsRef<Path>, size: usize) -> Result<DbPool, DbPoolError> {
        DbPool::with_config(
            path,
            DbPoolConfig {
                size,
                ..DbPoolConfig::default()
            },
        )
    }

    /// 用完整配置建池。
    pub fn with_config(path: impl AsRef<Path>, cfg: DbPoolConfig) -> Result<DbPool, DbPoolError> {
        let path = path.as_ref();
        if cfg.size == 0 {
            return Err(DbPoolError::BadCapacity);
        }
        let mut slots = Vec::with_capacity(cfg.size);
        for _ in 0..cfg.size {
            let conn = open_connection(path, cfg.busy_timeout)?;
            slots.push(Slot {
                conn: Mutex::new(conn),
            });
        }
        let free = (0..cfg.size).collect();
        Ok(DbPool {
            path: path.to_path_buf(),
            cfg,
            slots,
            free: Mutex::new(free),
            cv: Condvar::new(),
            closed: AtomicBool::new(false),
        })
    }

    /// 建一个内存库池。
    ///
    /// 强制单连接：SQLite 的每条 :memory: 连接都是独立库，多连接会各看各的。
    /// 要测并发请用 DbPool::open_temp。
    pub fn open_in_memory() -> Result<DbPool, DbPoolError> {
        DbPool::with_config(
            Path::new(":memory:"),
            DbPoolConfig {
                size: 1,
                ..DbPoolConfig::default()
            },
        )
    }

    /// 在系统临时目录建一个文件库池，返回 (池, 临时文件句柄)。
    ///
    /// **必须持有返回的 TempDb**：它一析构就会删掉库文件。
    /// 之所以用文件而不是 :memory:，是因为多连接要能看见同一个库。
    pub fn open_temp(tag: &str) -> Result<(DbPool, TempDb), DbPoolError> {
        let temp = TempDb::new(tag).map_err(|source| DbPoolError::TempIo {
            tag: tag.to_string(),
            source,
        })?;
        let pool = DbPool::new(temp.path())?;
        Ok((pool, temp))
    }

    /// 数据库路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 生效的配置
    pub fn config(&self) -> &DbPoolConfig {
        &self.cfg
    }

    /// 池容量
    pub fn size(&self) -> usize {
        self.cfg.size
    }

    /// 当前空闲 / 借出统计
    pub fn stats(&self) -> DbPoolStats {
        let free = self.lock_free();
        let idle = free.len();
        DbPoolStats {
            size: self.cfg.size,
            idle,
            busy: self.cfg.size - idle,
        }
    }

    /// 借一条连接，最多等 DbPoolConfig::acquire_timeout。
    pub fn acquire(&self) -> Result<DbGuard<'_>, DbPoolError> {
        let wait = self.cfg.acquire_timeout;
        self.acquire_timeout(wait)
    }

    /// 借一条连接，最多等 wait。
    pub fn acquire_timeout(&self, wait: Duration) -> Result<DbGuard<'_>, DbPoolError> {
        let deadline = Instant::now() + wait;
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return Err(DbPoolError::Closed);
            }

            let mut taken = None;
            {
                let mut free = self.lock_free();
                if let Some(index) = free.pop() {
                    taken = Some(index);
                }
            }
            if let Some(index) = taken {
                let conn = self.slots[index]
                    .conn
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                return Ok(DbGuard {
                    pool: self,
                    index,
                    conn,
                });
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(DbPoolError::Timeout(wait));
            }
            let free = self.lock_free();
            // 关键：释放 free 锁到重新加锁之间可能已经有连接归还并 notify，
            // 所以等待前必须再确认一次队列为空，否则会丢掉这次唤醒、白等到超时。
            if !free.is_empty() {
                drop(free);
                continue;
            }
            let (free, _) = self
                .cv
                .wait_timeout(free, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(free);
        }
    }

    /// 关闭池：之后的 acquire 直接返回 DbPoolError::Closed，等待者被唤醒。幂等。
    ///
    /// 已经借出的连接不受影响，归还时会被正常放回（池对象析构时才真正关闭）。
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.cv.notify_all();
    }

    /// 锁：被毒化只说明别的线程 panic 过，空闲索引没有半更新不变量，取回继续用。
    fn lock_free(&self) -> MutexGuard<'_, Vec<usize>> {
        self.free
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 借出的连接。析构时自动归还池子。
///
/// 通过 std::ops::Deref / std::ops::DerefMut 当普通 Connection 用，
/// 例如直接把 guard 交给 migrations::apply。
pub struct DbGuard<'a> {
    pool: &'a DbPool,
    index: usize,
    conn: MutexGuard<'a, Connection>,
}

impl std::ops::Deref for DbGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        &self.conn
    }
}

impl std::ops::DerefMut for DbGuard<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

impl Drop for DbGuard<'_> {
    fn drop(&mut self) {
        {
            let mut free = self.pool.lock_free();
            free.push(self.index);
        }
        self.pool.cv.notify_one();
    }
}

/// 临时库文件句柄：析构时删掉库文件（以及可能出现的 -wal / -shm）。
///
/// 建池不会立刻创建文件，SQLite 在第一次写入时才落盘，这没关系 —— 删除时
/// 文件不存在就忽略错误。
pub struct TempDb {
    path: PathBuf,
}

impl TempDb {
    /// 生成一个唯一的临时库路径（不创建文件）。
    pub fn new(tag: &str) -> std::io::Result<TempDb> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "music-robot-db-{}-{}-{}-{}.sqlite",
            std::process::id(),
            sanitize_tag(tag),
            crate::db::now_unix_ms(),
            seq
        ));
        // 极端重名下先清干净，保证拿到的是全新库
        let _ = std::fs::remove_file(&path);
        Ok(TempDb { path })
    }

    /// 库文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = self.path.clone().into_os_string();
            name.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(name));
        }
    }
}

/// 打开一条连接并把它配置到可用状态。
fn open_connection(path: &Path, busy_timeout: Duration) -> Result<Connection, DbPoolError> {
    let conn = Connection::open(path).map_err(|source| DbPoolError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    configure_connection(&conn, path, busy_timeout)?;
    Ok(conn)
}

/// 设置每条连接都必须有的 PRAGMA。
///
/// foreign_keys 是**连接级**开关，默认 OFF。漏开的话 ON DELETE CASCADE 会静默失效，
/// 所以这里不仅设置，还要读回来复核一次。
fn configure_connection(
    conn: &Connection,
    path: &Path,
    busy_timeout: Duration,
) -> Result<(), DbPoolError> {
    conn.execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|source| DbPoolError::Pragma {
            name: "foreign_keys",
            source,
        })?;
    conn.busy_timeout(busy_timeout)
        .map_err(|source| DbPoolError::Pragma {
            name: "busy_timeout",
            source,
        })?;

    let on: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .map_err(|source| DbPoolError::Pragma {
            name: "foreign_keys",
            source,
        })?;
    if on != 1 {
        return Err(DbPoolError::ForeignKeysOff {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// 清洗临时文件名里的标签，避免路径分隔符跑出临时目录。
fn sanitize_tag(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len());
    for ch in tag.chars().take(32) {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("db");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use rusqlite::params;

    fn temp_pool(tag: &str) -> (DbPool, TempDb) {
        DbPool::open_temp(tag).expect("建临时文件库池")
    }

    fn migrate(pool: &DbPool) {
        let mut guard = pool.acquire().expect("借连接");
        migrations::apply(&mut guard).expect("迁移");
    }

    fn foreign_keys(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("读外键开关")
    }

    fn row_count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .expect("计数")
    }

    /// 建 user -> album -> song -> playlist -> playlist_item，返回 (user, playlist, song)
    fn seed_playlist(conn: &Connection) -> (i64, i64, i64) {
        conn.execute(
            "INSERT INTO users (username, password_hash, role, created_at) VALUES ('u1', 'h', 'user', 1)",
            [],
        )
        .expect("插用户");
        let user_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO albums (name, album_artist, updated_at) VALUES ('专辑', '歌手', 1)",
            [],
        )
        .expect("插专辑");
        let album_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO songs (file_path, album_id, scrape_status, added_at, updated_at) VALUES ('/music/a.mp3', ?1, 'pending', 1, 1)",
            params![album_id],
        )
        .expect("插歌");
        let song_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO playlists (user_id, name, is_public, created_at, updated_at) VALUES (?1, '列表', 0, 1, 1)",
            params![user_id],
        )
        .expect("插歌单");
        let playlist_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO playlist_items (playlist_id, song_id, position, added_at) VALUES (?1, ?2, 1, 1)",
            params![playlist_id, song_id],
        )
        .expect("插条目");

        (user_id, playlist_id, song_id)
    }

    #[test]
    fn every_pooled_connection_has_foreign_keys_on() {
        let (pool, _tmp) = temp_pool("fk-on");
        assert_eq!(pool.size(), DEFAULT_POOL_SIZE);

        let mut guards = Vec::new();
        for _ in 0..pool.size() {
            guards.push(pool.acquire().expect("借连接"));
        }
        assert_eq!(pool.stats().busy, pool.size());
        assert_eq!(pool.stats().idle, 0);
        for guard in &guards {
            assert_eq!(foreign_keys(guard), 1, "每条连接的外键开关都必须打开");
        }
    }

    #[test]
    fn deleting_playlist_cascades_to_playlist_items() {
        let (pool, _tmp) = temp_pool("cascade-playlist");
        migrate(&pool);
        let guard = pool.acquire().expect("借连接");
        let (_user, playlist, _song) = seed_playlist(&guard);
        assert_eq!(row_count(&guard, "playlist_items"), 1, "先确认条目真的写进去了");

        guard
            .execute("DELETE FROM playlists WHERE id = ?1", params![playlist])
            .expect("删歌单");
        assert_eq!(
            row_count(&guard, "playlist_items"),
            0,
            "删歌单必须级联删掉 playlist_items"
        );
    }

    #[test]
    fn deleting_song_cascades_to_playlist_items() {
        let (pool, _tmp) = temp_pool("cascade-song");
        migrate(&pool);
        let guard = pool.acquire().expect("借连接");
        let (_user, _playlist, song) = seed_playlist(&guard);
        assert_eq!(row_count(&guard, "playlist_items"), 1);

        guard
            .execute("DELETE FROM songs WHERE id = ?1", params![song])
            .expect("删歌");
        assert_eq!(
            row_count(&guard, "playlist_items"),
            0,
            "删歌必须级联删掉 playlist_items"
        );
    }

    #[test]
    fn cascade_test_is_sensitive_to_the_foreign_keys_pragma() {
        // 反证：把 PRAGMA 关掉，同样的删除就**不会**级联 —— 说明上面两个用例
        // 测的确实是外键级联，而不是恰好没有别的行。
        let (pool, _tmp) = temp_pool("cascade-off");
        migrate(&pool);
        let guard = pool.acquire().expect("借连接");
        assert_eq!(foreign_keys(&guard), 1, "借出的连接默认必须开着外键");

        let (_user, playlist, _song) = seed_playlist(&guard);
        guard
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("关外键");
        assert_eq!(foreign_keys(&guard), 0);

        guard
            .execute("DELETE FROM playlists WHERE id = ?1", params![playlist])
            .expect("删歌单");
        assert_eq!(
            row_count(&guard, "playlist_items"),
            1,
            "外键关掉后条目必须残留（证明级联用例不是假绿）"
        );

        guard
            .execute_batch("PRAGMA foreign_keys = ON;")
            .expect("复原外键");
    }

    #[test]
    fn concurrent_reads_and_writes_across_threads_do_not_panic_or_lose_rows() {
        use std::sync::Arc;

        let (pool, _tmp) = temp_pool("concurrent");
        migrate(&pool);
        let pool = Arc::new(pool);

        let threads = 8usize;
        let per_thread = 10usize;
        let mut handles = Vec::new();
        for t in 0..threads {
            let pool = Arc::clone(&pool);
            handles.push(std::thread::spawn(move || {
                for i in 0..per_thread {
                    let guard = pool.acquire().expect("借连接");
                    let name = format!("u-{t}-{i}");
                    guard
                        .execute(
                            "INSERT INTO users (username, password_hash, role, created_at) VALUES (?1, 'h', 'user', 1)",
                            params![name],
                        )
                        .expect("并发插入");
                    let seen: i64 = guard
                        .query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))
                        .expect("并发读");
                    assert!(seen >= 1);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("工作线程不该 panic");
        }

        let guard = pool.acquire().expect("借连接");
        let total = row_count(&guard, "users");
        assert_eq!(
            total,
            (threads * per_thread) as i64,
            "并发写入必须全部落库"
        );
        let distinct: i64 = guard
            .query_row("SELECT COUNT(DISTINCT username) FROM users", [], |row| {
                row.get(0)
            })
            .expect("去重计数");
        assert_eq!(distinct, total, "用户名不该重复");
        drop(guard);
        assert_eq!(pool.stats().idle, pool.size(), "全部归还后应当都空闲");
    }

    #[test]
    fn acquire_times_out_when_all_connections_are_busy_then_recovers() {
        let (pool, _tmp) = temp_pool("busy");
        let mut held = Vec::new();
        for _ in 0..pool.size() {
            held.push(pool.acquire().expect("借连接"));
        }

        let outcome = pool.acquire_timeout(Duration::from_millis(80));
        assert!(
            matches!(outcome, Err(DbPoolError::Timeout(_))),
            "池满时必须超时报错，不能 panic 也不能硬等"
        );
        assert_eq!(pool.stats().busy, pool.size());

        held.clear();
        assert_eq!(pool.stats().idle, pool.size());
        let _again = pool.acquire().expect("归还后应当立刻借到");
    }

    #[test]
    fn memory_pool_is_forced_to_a_single_connection() {
        let pool = DbPool::open_in_memory().expect("内存库池");
        assert_eq!(pool.size(), 1, ":memory: 多连接会各看各的库，池必须强制单连接");
        {
            let guard = pool.acquire().expect("借连接");
            guard
                .execute_batch("CREATE TABLE probe (id INTEGER PRIMARY KEY);")
                .expect("建表");
        }
        let guard = pool.acquire().expect("借连接");
        let exists: i64 = guard
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'probe'",
                [],
                |row| row.get(0),
            )
            .expect("查表");
        assert_eq!(exists, 1, "同一连接复用时表必须还在");
    }

    #[test]
    fn closed_pool_refuses_new_acquires() {
        let (pool, _tmp) = temp_pool("closed");
        pool.close();
        let outcome = pool.acquire();
        assert!(matches!(outcome, Err(DbPoolError::Closed)));
    }

    #[test]
    fn zero_capacity_is_rejected() {
        let temp = TempDb::new("zero").expect("临时路径");
        let outcome = DbPool::with_size(temp.path(), 0);
        assert!(matches!(outcome, Err(DbPoolError::BadCapacity)));
    }

    #[test]
    fn sanitize_tag_strips_path_separators() {
        assert_eq!(sanitize_tag("pool-fk"), "pool-fk");
        assert_eq!(sanitize_tag(""), "db");
        let cleaned = sanitize_tag("../../etc/passwd");
        assert!(
            !cleaned.contains('/') && !cleaned.contains('.'),
            "清洗后不该有路径分隔符：{cleaned}"
        );
        assert_eq!(sanitize_tag(&"x".repeat(100)).len(), 32);
    }

    #[test]
    fn pool_error_display_is_chinese() {
        let cases: Vec<DbPoolError> = vec![
            DbPoolError::Open {
                path: PathBuf::from("/tmp/x.db"),
                source: rusqlite::Error::QueryReturnedNoRows,
            },
            DbPoolError::Pragma {
                name: "foreign_keys",
                source: rusqlite::Error::QueryReturnedNoRows,
            },
            DbPoolError::ForeignKeysOff {
                path: PathBuf::from("/tmp/x.db"),
            },
            DbPoolError::Closed,
            DbPoolError::Timeout(Duration::from_millis(50)),
            DbPoolError::TempIo {
                tag: "t".to_string(),
                source: std::io::Error::other("x"),
            },
            DbPoolError::BadCapacity,
        ];
        for e in &cases {
            let text = e.to_string();
            assert!(!text.is_empty());
            assert!(
                text.contains("数据库") || text.contains("SQLite") || text.contains("连接池"),
                "应为中文错误：{text}"
            );
        }
        assert!(std::error::Error::source(&cases[0]).is_some());
        assert!(std::error::Error::source(&cases[5]).is_some());
        assert!(std::error::Error::source(&cases[4]).is_none());
        let _boxed: Box<dyn std::error::Error> = Box::new(DbPoolError::Closed);
    }
}
