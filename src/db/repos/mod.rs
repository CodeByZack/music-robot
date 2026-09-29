//! S3 · Repositories —— 8 张业务表的读写入口。
//!
//! ## 方法签名为什么是 &Connection
//!
//! 调用方（S5 扫描、S13 刮削）经常要把多个写操作放进**同一个事务**：
//!
//!     let tx = conn.transaction()?;
//!     let album_id = albums::insert(&tx, &album)?;
//!     let song_id = songs::insert(&tx, &song)?;
//!     tx.commit()?;
//!
//! 所以每个 repo 都只接收调用方给的连接：自己绝不借池、不 BEGIN、不 commit。
//! rusqlite::Transaction 实现了 Deref<Target = Connection>，上面 &tx 会因为
//! 自动解引用直接当 &Connection 用 —— 同一套函数既能独立调用，也能原样组合进
//! 事务，不需要再维护一批 _tx 变体。
//!
//! 少数由多条语句组成、必须原子完成的组合操作（歌单条目重排等）用
//! [in_savepoint] 包一层：独立调用时有原子性；调用方已经在事务里时
//! SAVEPOINT 会正确嵌套，不会撞上 rusqlite「嵌套事务直接报错」的限制。
//!
//! ## 两条统一约定
//!
//! * **时间戳由 repo 盖章**：insert 一律用 [crate::db::now_unix_ms] 写
//!   created_at / added_at / updated_at / played_at 并忽略结构体里的同名字段，
//!   免得每个调用方各写各的时钟；
//! * **错误分门别类**：UNIQUE / PRIMARY KEY 冲突是 [RepoError::Conflict]、
//!   外键失败是 [RepoError::ForeignKey]，调用方据此走「复用已有行」这类正常
//!   业务分支，而不是把一切都当故障。

use rusqlite::ffi;
use rusqlite::Connection;

use crate::db::models::ModelError;

pub mod albums;
pub mod favorites;
pub mod history;
pub mod playlists;
pub mod requests;
pub mod settings;
pub mod songs;
pub mod users;

/// repo 层统一返回类型。
pub type RepoResult<T> = Result<T, RepoError>;

/// repo 层统一错误。
#[derive(Debug)]
pub enum RepoError {
    /// 底层 SQLite 错误（IO、SQL 语法、类型转换等）
    Sqlite(rusqlite::Error),
    /// UNIQUE / PRIMARY KEY 冲突：库里已经有相同的行
    ///
    /// 单独一个变体，是为了让「点歌命中已有请求」「重复收藏」这类**正常业务分支**
    /// 能和真正的故障区分开，不必去错误文本里翻 UNIQUE 字样。
    Conflict {
        /// SQLite 给出的约束说明（含冲突的表与列）
        constraint: String,
    },
    /// 外键约束失败：写入了指向不存在行的引用
    ForeignKey {
        /// SQLite 给出的约束说明
        constraint: String,
    },
    /// 其它约束失败（NOT NULL / CHECK）
    Constraint {
        /// SQLite 给出的约束说明
        constraint: String,
    },
    /// 枚举列出现 schema 之外的取值
    Model(ModelError),
    /// 写后读一致性被破坏（刚写入的行又查不到，说明有别的写入者抢先动了数据）
    Invariant {
        /// 说明
        message: String,
    },
}

impl std::fmt::Display for RepoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RepoError::Sqlite(e) => write!(f, "数据库读写失败：{e}"),
            RepoError::Conflict { constraint } => {
                write!(f, "唯一性冲突，相同记录已存在：{constraint}")
            }
            RepoError::ForeignKey { constraint } => {
                write!(f, "外键约束失败，引用了不存在的记录：{constraint}")
            }
            RepoError::Constraint { constraint } => {
                write!(f, "数据库约束校验失败：{constraint}")
            }
            RepoError::Model(e) => write!(f, "数据模型转换失败：{e}"),
            RepoError::Invariant { message } => write!(f, "数据库状态异常：{message}"),
        }
    }
}

impl std::error::Error for RepoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RepoError::Sqlite(e) => Some(e),
            RepoError::Model(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for RepoError {
    fn from(err: rusqlite::Error) -> RepoError {
        classify(err)
    }
}

impl From<ModelError> for RepoError {
    fn from(err: ModelError) -> RepoError {
        RepoError::Model(err)
    }
}

/// 把 SQLite 错误按约束类别细分。
///
/// 每个 repo 里的问号运算符都走这个转换，所以「约束冲突」不会退化成笼统的
/// [RepoError::Sqlite]。
pub fn classify(err: rusqlite::Error) -> RepoError {
    if let rusqlite::Error::SqliteFailure(code, message) = &err {
        if code.code == ffi::ErrorCode::ConstraintViolation {
            let detail = match message {
                Some(text) => text.clone(),
                None => "SQLite 约束冲突".to_string(),
            };
            return match constraint_kind(code.extended_code, &detail) {
                ConstraintKind::Unique => RepoError::Conflict { constraint: detail },
                ConstraintKind::ForeignKey => RepoError::ForeignKey { constraint: detail },
                ConstraintKind::Other => RepoError::Constraint { constraint: detail },
            };
        }
    }
    RepoError::Sqlite(err)
}

/// 约束类别，只给 [classify] 内部分流用。
enum ConstraintKind {
    /// UNIQUE / PRIMARY KEY
    Unique,
    /// FOREIGN KEY
    ForeignKey,
    /// NOT NULL / CHECK 等
    Other,
}

/// 判定约束类别：优先看扩展错误码；某些构建下扩展码没打开，就退回匹配错误文本。
fn constraint_kind(extended_code: i32, message: &str) -> ConstraintKind {
    match extended_code {
        ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY => {
            return ConstraintKind::Unique
        }
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY => return ConstraintKind::ForeignKey,
        _ => {}
    }
    if message.contains("UNIQUE constraint failed") || message.contains("PRIMARY KEY constraint failed")
    {
        ConstraintKind::Unique
    } else if message.contains("FOREIGN KEY constraint failed") {
        ConstraintKind::ForeignKey
    } else {
        ConstraintKind::Other
    }
}

/// 把一段多条语句的写操作包进 SAVEPOINT：要么全成，要么一条都不留。
///
/// 之所以用 SAVEPOINT 而不是 conn.unchecked_transaction()：后者在调用方已经
/// 开了事务时会直接报「嵌套事务」错，而 SAVEPOINT 是 SQLite 规定的嵌套机制，
/// 里外两层都能正常回滚。
///
/// name 只允许由本模块内的字面量传入 —— 保存点名要拼进 SQL，不能带外部输入。
pub(crate) fn in_savepoint<T>(
    conn: &Connection,
    name: &'static str,
    body: impl FnOnce() -> RepoResult<T>,
) -> RepoResult<T> {
    conn.execute_batch(&format!("SAVEPOINT {name}"))?;
    match body() {
        Ok(value) => {
            conn.execute_batch(&format!("RELEASE {name}"))?;
            Ok(value)
        }
        Err(err) => {
            // 原始错误更有诊断价值，回滚本身失败就不再覆盖它
            let _ = conn.execute_batch(&format!("ROLLBACK TO {name}; RELEASE {name}"));
            Err(err)
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! 各 repo 测试共用的临时库与预置数据。
    //!
    //! 预置数据一律用裸 SQL 插，不走 repo 自己的方法：这样某个 repo 坏掉时，
    //! 只有它自己的测试会红，不会把别的 repo 的测试一起带塌。

    use rusqlite::{params, Connection};

    use crate::db::migrations;
    use crate::db::pool::{DbGuard, DbPool, TempDb};

    /// 一个已经跑完全部迁移的临时文件库。
    pub(crate) struct TestDb {
        pool: DbPool,
        /// 必须留在结构体里：TempDb 一析构就会删掉库文件。
        _temp: TempDb,
    }

    impl TestDb {
        pub(crate) fn new(tag: &str) -> TestDb {
            let (pool, temp) = DbPool::open_temp(tag).expect("建临时文件库池");
            {
                let mut guard = pool.acquire().expect("借连接");
                migrations::apply(&mut guard).expect("应用迁移");
            }
            TestDb { pool, _temp: temp }
        }

        /// 借一条连接（DbGuard 会自动解引用成 &Connection）。
        pub(crate) fn conn(&self) -> DbGuard<'_> {
            self.pool.acquire().expect("借连接")
        }
    }

    /// 预置一个用户，返回 id。
    pub(crate) fn seed_user(conn: &Connection, username: &str) -> i64 {
        conn.execute(
            "INSERT INTO users (username, password_hash, role, created_at) VALUES (?1, 'hash', 'user', ?2)",
            params![username, crate::db::now_unix_ms()],
        )
        .expect("预置用户");
        conn.last_insert_rowid()
    }

    /// 预置一张专辑，返回 id。
    pub(crate) fn seed_album(conn: &Connection, name: &str, album_artist: &str) -> i64 {
        conn.execute(
            "INSERT INTO albums (name, album_artist, updated_at) VALUES (?1, ?2, ?3)",
            params![name, album_artist, crate::db::now_unix_ms()],
        )
        .expect("预置专辑");
        conn.last_insert_rowid()
    }

    /// 预置一首歌（可挂到专辑上），返回 id。
    pub(crate) fn seed_song(conn: &Connection, file_path: &str, album_id: Option<i64>) -> i64 {
        let now = crate::db::now_unix_ms();
        conn.execute(
            "INSERT INTO songs (file_path, album_id, scrape_status, added_at, updated_at)
             VALUES (?1, ?2, 'pending', ?3, ?3)",
            params![file_path, album_id, now],
        )
        .expect("预置歌曲");
        conn.last_insert_rowid()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::TestDb;
    use super::*;
    use crate::db::models::{Role, User};

    fn sample_user(username: &str) -> User {
        User {
            id: 0,
            username: username.to_string(),
            password_hash: "hash".to_string(),
            role: Role::User,
            created_at: 0,
            last_login: None,
        }
    }

    #[test]
    fn repo_error_display_is_chinese_and_keeps_the_source_chain() {
        let cases: Vec<RepoError> = vec![
            RepoError::Conflict {
                constraint: "UNIQUE constraint failed: users.username".to_string(),
            },
            RepoError::ForeignKey {
                constraint: "FOREIGN KEY constraint failed".to_string(),
            },
            RepoError::Constraint {
                constraint: "NOT NULL constraint failed: songs.file_path".to_string(),
            },
            RepoError::Model(ModelError::UnknownValue {
                field: "role",
                value: "root".to_string(),
                allowed: "admin | user",
            }),
            RepoError::Invariant {
                message: "刚插入的点歌请求查不到".to_string(),
            },
            RepoError::Sqlite(rusqlite::Error::QueryReturnedNoRows),
        ];
        for err in &cases {
            let text = err.to_string();
            assert!(!text.is_empty());
            assert!(
                text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "错误信息必须是中文：{text}"
            );
        }

        let boxed: Box<dyn std::error::Error> = Box::new(RepoError::Model(ModelError::UnknownValue {
            field: "role",
            value: "root".to_string(),
            allowed: "admin | user",
        }));
        assert!(boxed.source().is_some(), "模型错误应保留 source 链");
    }

    #[test]
    fn repo_writes_roll_back_with_the_callers_transaction() {
        let db = TestDb::new("repos-tx-rollback");
        let mut conn = db.conn();
        {
            // 注意 &tx 是靠 Deref 直接当 &Connection 用的，没有 _tx 专用变体
            let tx = conn.transaction().expect("开事务");
            users::insert(&tx, &sample_user("alice")).expect("事务内插第一个用户");
            users::insert(&tx, &sample_user("bob")).expect("事务内插第二个用户");
            assert_eq!(users::list(&tx).expect("事务内自读").len(), 2);
            tx.rollback().expect("回滚");
        }
        assert_eq!(
            users::list(&conn).expect("回滚后列表").len(),
            0,
            "回滚后两行都不该在"
        );
        assert_eq!(users::count(&conn).expect("回滚后计数"), 0);
    }

    #[test]
    fn errors_raised_inside_a_transaction_still_leave_the_connection_usable() {
        let db = TestDb::new("repos-tx-error");
        let mut conn = db.conn();
        {
            let tx = conn.transaction().expect("开事务");
            users::insert(&tx, &sample_user("carol")).expect("先插一个");
            let err = users::insert(&tx, &sample_user("carol")).expect_err("重复用户名应报冲突");
            assert!(
                matches!(err, RepoError::Conflict { .. }),
                "事务里也应拿到 Conflict：{err:?}"
            );
            // 冲突之后事务仍可继续使用，也可以整体回滚
            tx.rollback().expect("回滚");
        }
        assert_eq!(users::count(&conn).expect("回滚后计数"), 0);
        // 连接没被留在半开的事务里
        assert!(conn.is_autocommit(), "回滚后应回到自动提交状态");
    }
}
