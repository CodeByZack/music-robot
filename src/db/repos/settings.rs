//! user_settings 表读写。
//!
//! 复合主键 (user_id, key)：断点续播 position_ms、音量、播放模式都放这里。
//! [set] 是 upsert —— 同一个 key 再写就是覆盖，不需要调用方先查再决定增还是改。

use rusqlite::{params, Connection, Row};

use crate::db::models::UserSetting;

use super::RepoResult;

const COLUMNS: &str = "user_id, key, value";

/// 读一条设置；行不存在返回 None。
///
/// 注意「行存在但 value 是 NULL」和「行不存在」在这里返回的分别是
/// Some(UserSetting { value: None }) 和 None —— 需要区分就返回结构体，
/// 只想要字符串请用 [value]。
pub fn get(conn: &Connection, user_id: i64, key: &str) -> RepoResult<Option<UserSetting>> {
    Ok(query_settings(
        conn,
        "WHERE user_id = ?1 AND key = ?2",
        params![user_id, key],
    )?
    .into_iter()
    .next())
}

/// 便捷读：只要设置值。行不存在或值为 NULL 都返回 None。
pub fn value(conn: &Connection, user_id: i64, key: &str) -> RepoResult<Option<String>> {
    Ok(get(conn, user_id, key)?.and_then(|setting| setting.value))
}

/// 写设置（upsert）：行不存在就插入，存在就覆盖 value。
///
/// value 传 None 表示显式写一个空值（用户清空了这项设置），和「没有这条设置」
/// 是两回事 —— 后者用 [delete]。
pub fn set(conn: &Connection, user_id: i64, key: &str, value: Option<&str>) -> RepoResult<()> {
    conn.execute(
        "INSERT INTO user_settings (user_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id, key) DO UPDATE SET value = excluded.value",
        params![user_id, key, value],
    )?;
    Ok(())
}

/// 删一条设置，返回删掉的行数（没有就是 0）。
pub fn delete(conn: &Connection, user_id: i64, key: &str) -> RepoResult<usize> {
    let removed = conn.execute(
        "DELETE FROM user_settings WHERE user_id = ?1 AND key = ?2",
        params![user_id, key],
    )?;
    Ok(removed)
}

/// 列某用户的全部设置，按 key 升序。
pub fn list(conn: &Connection, user_id: i64) -> RepoResult<Vec<UserSetting>> {
    query_settings(conn, "WHERE user_id = ?1 ORDER BY key", params![user_id])
}

/// 清空某用户的全部设置，返回删掉的行数。
pub fn clear(conn: &Connection, user_id: i64) -> RepoResult<usize> {
    let removed = conn.execute(
        "DELETE FROM user_settings WHERE user_id = ?1",
        params![user_id],
    )?;
    Ok(removed)
}

/// 某用户的设置条数。
pub fn count(conn: &Connection, user_id: i64) -> RepoResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM user_settings WHERE user_id = ?1",
        params![user_id],
        |row| row.get(0),
    )?)
}

/// where_clause 只由本模块的常量传入，参数一律走 params 绑定。
fn query_settings<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> RepoResult<Vec<UserSetting>> {
    let sql = format!("SELECT {COLUMNS} FROM user_settings {where_clause}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_setting(row)?);
    }
    Ok(out)
}

fn row_to_setting(row: &Row<'_>) -> RepoResult<UserSetting> {
    Ok(UserSetting {
        user_id: row.get("user_id")?,
        key: row.get("key")?,
        value: row.get("value")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::test_support::{seed_user, TestDb};
    use crate::db::repos::RepoError;

    #[test]
    fn settings_crud_round_trip_with_upsert() {
        let db = TestDb::new("settings-crud");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");

        // 查：一开始什么都没有
        assert!(get(&conn, user_id, "position_ms").expect("查设置").is_none());
        assert!(value(&conn, user_id, "position_ms").expect("查值").is_none());

        // 增
        set(&conn, user_id, "position_ms", Some("12345")).expect("写设置");
        let row = get(&conn, user_id, "position_ms")
            .expect("查设置")
            .expect("应能查到");
        assert_eq!(row.user_id, user_id);
        assert_eq!(row.key, "position_ms");
        assert_eq!(row.value.as_deref(), Some("12345"));
        assert_eq!(
            value(&conn, user_id, "position_ms").expect("查值").as_deref(),
            Some("12345")
        );

        // 改：同一个 key 再写就是覆盖（upsert），不会撞主键
        set(&conn, user_id, "position_ms", Some("99999")).expect("覆盖设置");
        assert_eq!(
            value(&conn, user_id, "position_ms").expect("查值").as_deref(),
            Some("99999")
        );
        assert_eq!(count(&conn, user_id).expect("计数"), 1, "upsert 不该多出一行");

        // 值为 NULL 是合法状态，和「没有这条设置」不同
        set(&conn, user_id, "volume", None).expect("写空值");
        let empty = get(&conn, user_id, "volume")
            .expect("查设置")
            .expect("行应该在");
        assert!(empty.value.is_none());
        assert!(value(&conn, user_id, "volume").expect("查值").is_none());
        assert_eq!(count(&conn, user_id).expect("计数"), 2);

        // 再写一个，列表按 key 升序
        set(&conn, user_id, "play_mode", Some("shuffle")).expect("再写一个");
        let all = list(&conn, user_id).expect("列出设置");
        let keys: Vec<&str> = all.iter().map(|setting| setting.key.as_str()).collect();
        assert_eq!(keys, vec!["play_mode", "position_ms", "volume"]);

        // 删
        assert_eq!(delete(&conn, user_id, "position_ms").expect("删设置"), 1);
        assert!(get(&conn, user_id, "position_ms").expect("查").is_none());
        assert_eq!(delete(&conn, user_id, "position_ms").expect("重复删"), 0);

        // 清空
        assert_eq!(clear(&conn, user_id).expect("清空"), 2);
        assert!(list(&conn, user_id).expect("列表").is_empty());
        assert_eq!(clear(&conn, user_id).expect("重复清空"), 0);
    }

    #[test]
    fn settings_are_scoped_per_user() {
        let db = TestDb::new("settings-scope");
        let conn = db.conn();
        let alice = seed_user(&conn, "alice");
        let bob = seed_user(&conn, "bob");

        set(&conn, alice, "volume", Some("30")).expect("alice 写");
        assert!(
            value(&conn, bob, "volume").expect("bob 读").is_none(),
            "读不到别人的设置"
        );

        set(&conn, bob, "volume", Some("80")).expect("bob 写");
        assert_eq!(
            value(&conn, alice, "volume").expect("alice 读").as_deref(),
            Some("30")
        );
        assert_eq!(
            value(&conn, bob, "volume").expect("bob 读").as_deref(),
            Some("80")
        );
        assert_eq!(count(&conn, alice).expect("alice 计数"), 1);
        assert_eq!(count(&conn, bob).expect("bob 计数"), 1);
    }

    #[test]
    fn settings_for_a_missing_user_is_a_foreign_key_error() {
        let db = TestDb::new("settings-fk");
        let conn = db.conn();
        let err = set(&conn, 9999, "volume", Some("30")).expect_err("用户不存在必须失败");
        assert!(
            matches!(err, RepoError::ForeignKey { .. }),
            "应是可区分的外键错误，实际 {err:?}"
        );
    }
}
