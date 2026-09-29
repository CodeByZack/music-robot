//! users 表读写。

use rusqlite::{params, Connection, Row};

use crate::db::models::{Role, User};

use super::RepoResult;

const COLUMNS: &str = "id, username, password_hash, role, created_at, last_login";

/// 建用户，返回新行 id。
///
/// created_at 由本函数盖章；last_login 用调用方给的值（新用户一般是 None，
/// 登录成功后再走 [update_last_login]）。
pub fn insert(conn: &Connection, user: &User) -> RepoResult<i64> {
    conn.execute(
        "INSERT INTO users (username, password_hash, role, created_at, last_login)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            user.username,
            user.password_hash,
            user.role.as_str(),
            crate::db::now_unix_ms(),
            user.last_login,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 按主键查，查不到返回 None。
pub fn get(conn: &Connection, id: i64) -> RepoResult<Option<User>> {
    Ok(query_users(conn, "WHERE id = ?1", params![id])?
        .into_iter()
        .next())
}

/// 按登录名查（username 有 UNIQUE 约束，最多一条）。
pub fn find_by_username(conn: &Connection, username: &str) -> RepoResult<Option<User>> {
    Ok(query_users(conn, "WHERE username = ?1", params![username])?
        .into_iter()
        .next())
}

/// 列出全部用户，按 id 升序。
pub fn list(conn: &Connection) -> RepoResult<Vec<User>> {
    query_users(conn, "ORDER BY id", [])
}

/// 全表行数。
pub fn count(conn: &Connection) -> RepoResult<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?)
}

/// 登录成功后把 last_login 盖成当前时间。
pub fn update_last_login(conn: &Connection, id: i64) -> RepoResult<usize> {
    let changed = conn.execute(
        "UPDATE users SET last_login = ?2 WHERE id = ?1",
        params![id, crate::db::now_unix_ms()],
    )?;
    Ok(changed)
}

/// 删用户；其歌单 / 收藏 / 历史 / 设置 / 点歌请求会按外键级联一起删。
pub fn delete(conn: &Connection, id: i64) -> RepoResult<usize> {
    let removed = conn.execute("DELETE FROM users WHERE id = ?1", params![id])?;
    Ok(removed)
}

/// where_clause 只由本模块的常量传入，参数一律走 params 绑定。
fn query_users<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> RepoResult<Vec<User>> {
    let sql = format!("SELECT {COLUMNS} FROM users {where_clause}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_user(row)?);
    }
    Ok(out)
}

/// 把一行还原成 User；role 的非法取值会变成 RepoError::Model 而不是静默降级成 User。
fn row_to_user(row: &Row<'_>) -> RepoResult<User> {
    let role_text: String = row.get("role")?;
    let role = Role::parse(&role_text)?;
    Ok(User {
        id: row.get("id")?,
        username: row.get("username")?,
        password_hash: row.get("password_hash")?,
        role,
        created_at: row.get("created_at")?,
        last_login: row.get("last_login")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::test_support::TestDb;
    use crate::db::repos::RepoError;

    fn sample_user(username: &str, role: Role) -> User {
        User {
            id: 0,
            username: username.to_string(),
            password_hash: "hash".to_string(),
            role,
            created_at: 0,
            last_login: None,
        }
    }

    #[test]
    fn user_crud_round_trip() {
        let db = TestDb::new("users-crud");
        let conn = db.conn();

        let id = insert(&conn, &sample_user("alice", Role::Admin)).expect("建用户");
        let loaded = get(&conn, id).expect("按 id 查").expect("应能查到");
        assert_eq!(loaded.username, "alice");
        assert_eq!(loaded.role, Role::Admin);
        assert_eq!(loaded.password_hash, "hash");
        assert!(loaded.created_at > 0, "created_at 应由 repo 盖章");
        assert!(loaded.last_login.is_none(), "新用户还没登录过");

        let by_name = find_by_username(&conn, "alice")
            .expect("按用户名查")
            .expect("应能查到");
        assert_eq!(by_name.id, id);
        assert!(find_by_username(&conn, "nobody")
            .expect("按用户名查")
            .is_none());

        // 改：盖 last_login
        assert_eq!(update_last_login(&conn, id).expect("更新 last_login"), 1);
        let logged_in = get(&conn, id).expect("重查").expect("应能查到");
        assert!(logged_in.last_login.is_some(), "登录后应有时间戳");
        assert!(logged_in.last_login.unwrap_or(0) >= logged_in.created_at);

        // 查：列表
        insert(&conn, &sample_user("bob", Role::User)).expect("建第二个用户");
        let all = list(&conn).expect("列出用户");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, id, "按 id 升序");
        assert_eq!(all[1].role, Role::User);
        assert_eq!(count(&conn).expect("计数"), 2);

        // 删
        assert_eq!(delete(&conn, id).expect("删用户"), 1);
        assert!(get(&conn, id).expect("重查").is_none());
        assert_eq!(count(&conn).expect("计数"), 1);
    }

    #[test]
    fn duplicate_username_is_reported_as_a_conflict() {
        let db = TestDb::new("users-dup-name");
        let conn = db.conn();
        insert(&conn, &sample_user("alice", Role::User)).expect("建第一个");
        let err = insert(&conn, &sample_user("alice", Role::Admin)).expect_err("重名必须冲突");
        assert!(
            matches!(err, RepoError::Conflict { .. }),
            "应是可区分的 Conflict，实际 {err:?}"
        );
        assert!(
            err.to_string().contains("唯一性冲突"),
            "冲突信息要说人话：{err}"
        );
        assert_eq!(count(&conn).expect("计数"), 1, "冲突不该写进第二行");
    }
}
