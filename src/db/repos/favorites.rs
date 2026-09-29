//! favorites 表读写。
//!
//! UNIQUE(user_id, song_id)：同一个用户对同一首歌只能收藏一次，
//! 重复收藏会拿到 [super::RepoError::Conflict]（调用方据此当「已收藏」处理）。

use rusqlite::{params, Connection, Row};

use crate::db::models::Favorite;

use super::RepoResult;

const COLUMNS: &str = "id, user_id, song_id, created_at";

/// 加收藏，返回新行 id；created_at 由本函数盖章。
///
/// 重复收藏返回 [super::RepoError::Conflict]；想要幂等语义请先查 [is_favorited]，
/// 或者接住 Conflict 当成功。
pub fn add(conn: &Connection, user_id: i64, song_id: i64) -> RepoResult<i64> {
    conn.execute(
        "INSERT INTO favorites (user_id, song_id, created_at) VALUES (?1, ?2, ?3)",
        params![user_id, song_id, crate::db::now_unix_ms()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 按 (user_id, song_id) 查，查不到返回 None。
pub fn get(conn: &Connection, user_id: i64, song_id: i64) -> RepoResult<Option<Favorite>> {
    Ok(query_favorites(
        conn,
        "WHERE user_id = ?1 AND song_id = ?2",
        params![user_id, song_id],
    )?
    .into_iter()
    .next())
}

/// 取消收藏，返回删掉的行数（没收藏过就是 0）。
pub fn remove(conn: &Connection, user_id: i64, song_id: i64) -> RepoResult<usize> {
    let removed = conn.execute(
        "DELETE FROM favorites WHERE user_id = ?1 AND song_id = ?2",
        params![user_id, song_id],
    )?;
    Ok(removed)
}

/// 是否已收藏。
pub fn is_favorited(conn: &Connection, user_id: i64, song_id: i64) -> RepoResult<bool> {
    let found: i64 = conn.query_row(
        "SELECT COUNT(*) FROM favorites WHERE user_id = ?1 AND song_id = ?2",
        params![user_id, song_id],
        |row| row.get(0),
    )?;
    Ok(found > 0)
}

/// 列某用户的收藏，最近收藏的在前。
pub fn list_by_user(
    conn: &Connection,
    user_id: i64,
    limit: i64,
    offset: i64,
) -> RepoResult<Vec<Favorite>> {
    query_favorites(
        conn,
        "WHERE user_id = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2 OFFSET ?3",
        params![user_id, limit, offset],
    )
}

/// 某用户的收藏数。
pub fn count_by_user(conn: &Connection, user_id: i64) -> RepoResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM favorites WHERE user_id = ?1",
        params![user_id],
        |row| row.get(0),
    )?)
}

/// where_clause 只由本模块的常量传入，参数一律走 params 绑定。
fn query_favorites<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> RepoResult<Vec<Favorite>> {
    let sql = format!("SELECT {COLUMNS} FROM favorites {where_clause}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_favorite(row)?);
    }
    Ok(out)
}

fn row_to_favorite(row: &Row<'_>) -> RepoResult<Favorite> {
    Ok(Favorite {
        id: row.get("id")?,
        user_id: row.get("user_id")?,
        song_id: row.get("song_id")?,
        created_at: row.get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::test_support::{seed_song, seed_user, TestDb};
    use crate::db::repos::RepoError;

    #[test]
    fn favorite_crud_round_trip() {
        let db = TestDb::new("favorites-crud");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        let other_user = seed_user(&conn, "bob");
        let song_a = seed_song(&conn, "/music/a.mp3", None);
        let song_b = seed_song(&conn, "/music/b.mp3", None);

        // 增
        let id = add(&conn, user_id, song_a).expect("加收藏");
        let row = get(&conn, user_id, song_a)
            .expect("按自然键查")
            .expect("应能查到");
        assert_eq!(row.id, id);
        assert_eq!(row.song_id, song_a);
        assert!(row.created_at > 0, "created_at 应由 repo 盖章");
        assert!(is_favorited(&conn, user_id, song_a).expect("是否已收藏"));
        assert!(!is_favorited(&conn, user_id, song_b).expect("未收藏的歌"));
        assert!(get(&conn, user_id, song_b).expect("查未收藏").is_none());

        // 别人的收藏互不影响
        add(&conn, other_user, song_a).expect("bob 也收藏 a");
        assert_eq!(count_by_user(&conn, user_id).expect("alice 收藏数"), 1);
        assert_eq!(count_by_user(&conn, other_user).expect("bob 收藏数"), 1);

        add(&conn, user_id, song_b).expect("再加一首");
        let mine = list_by_user(&conn, user_id, 10, 0).expect("列出收藏");
        assert_eq!(mine.len(), 2);
        assert_eq!(mine[0].song_id, song_b, "最近收藏的在前");
        assert_eq!(count_by_user(&conn, user_id).expect("收藏数"), 2);
        assert_eq!(list_by_user(&conn, user_id, 1, 1).expect("分页").len(), 1);

        // 删（favorites 没有可改的负载列，所以「改」这一步就是移除后重新收藏）
        assert_eq!(remove(&conn, user_id, song_a).expect("取消收藏"), 1);
        assert!(!is_favorited(&conn, user_id, song_a).expect("已取消"));
        assert_eq!(remove(&conn, user_id, song_a).expect("重复取消"), 0);
        assert_eq!(count_by_user(&conn, user_id).expect("收藏数"), 1);

        // 重新收藏会产生一条全新的行（新的 id 与 created_at）
        let again = add(&conn, user_id, song_a).expect("重新收藏");
        assert_ne!(again, id, "重新收藏是新的一行");
        assert_eq!(count_by_user(&conn, user_id).expect("收藏数"), 2);
    }

    #[test]
    fn duplicate_favorite_is_reported_as_a_conflict() {
        let db = TestDb::new("favorites-dup");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        let song_id = seed_song(&conn, "/music/a.mp3", None);

        add(&conn, user_id, song_id).expect("第一次收藏");
        let err = add(&conn, user_id, song_id).expect_err("重复收藏必须冲突");
        assert!(
            matches!(err, RepoError::Conflict { .. }),
            "应是可区分的 Conflict，实际 {err:?}"
        );
        assert_eq!(count_by_user(&conn, user_id).expect("收藏数"), 1);
    }

    #[test]
    fn favorite_for_missing_song_is_a_foreign_key_error() {
        let db = TestDb::new("favorites-fk");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        let err = add(&conn, user_id, 999_999).expect_err("歌不存在必须失败");
        assert!(
            matches!(err, RepoError::ForeignKey { .. }),
            "应是可区分的外键错误，实际 {err:?}"
        );
    }

    #[test]
    fn soft_deleting_a_song_keeps_the_favorite_but_purging_cascades_it_away() {
        let db = TestDb::new("favorites-cascade");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        let song_id = seed_song(&conn, "/music/a.mp3", None);
        add(&conn, user_id, song_id).expect("收藏");

        // 软删（磁盘上文件不见了）绝不能动用户的收藏 —— 这正是它不硬删的理由
        super::super::songs::mark_deleted(&conn, song_id).expect("标记删除");
        assert_eq!(
            count_by_user(&conn, user_id).expect("收藏数"),
            1,
            "软删不该连带删掉收藏"
        );

        // 只有管理端明确 purge 才会按外键级联删掉收藏
        super::super::songs::purge(&conn, song_id).expect("物理删除");
        assert_eq!(count_by_user(&conn, user_id).expect("收藏数"), 0);
    }
}
