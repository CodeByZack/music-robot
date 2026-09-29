//! play_history 表读写。

use rusqlite::{params, Connection};

use crate::db::models::PlayHistory;

use super::RepoResult;

const COLUMNS: &str = "id, user_id, song_id, played_at, duration_listened_ms";

/// 插一条播放历史，返回新行 id。
///
/// played_at 由本函数盖当前时间，结构体里的同名字段被忽略 —— 历史的时间语义
/// 就是「记下来的那一刻」。
pub fn insert(conn: &Connection, history: &PlayHistory) -> RepoResult<i64> {
    conn.execute(
        "INSERT INTO play_history (user_id, song_id, played_at, duration_listened_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            history.user_id,
            history.song_id,
            crate::db::now_unix_ms(),
            history.duration_listened_ms,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 记一次播放，[insert] 的便捷壳（播放时长可以不知道，传 None）。
pub fn record(
    conn: &Connection,
    user_id: i64,
    song_id: i64,
    duration_listened_ms: Option<i64>,
) -> RepoResult<i64> {
    insert(
        conn,
        &PlayHistory {
            id: 0,
            user_id,
            song_id,
            played_at: 0,
            duration_listened_ms,
        },
    )
}

/// 按主键查，查不到返回 None。
pub fn get(conn: &Connection, id: i64) -> RepoResult<Option<PlayHistory>> {
    Ok(query_history(conn, "WHERE id = ?1", params![id])?
        .into_iter()
        .next())
}

/// 最近播放分页，最新的在前。
pub fn recent(
    conn: &Connection,
    user_id: i64,
    limit: i64,
    offset: i64,
) -> RepoResult<Vec<PlayHistory>> {
    query_history(
        conn,
        "WHERE user_id = ?1 ORDER BY played_at DESC, id DESC LIMIT ?2 OFFSET ?3",
        params![user_id, limit, offset],
    )
}

/// 某用户的历史条数。
pub fn count_by_user(conn: &Connection, user_id: i64) -> RepoResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM play_history WHERE user_id = ?1",
        params![user_id],
        |row| row.get(0),
    )?)
}

/// 清空某用户的全部播放历史，返回删掉的行数。
pub fn clear_by_user(conn: &Connection, user_id: i64) -> RepoResult<usize> {
    let removed = conn.execute(
        "DELETE FROM play_history WHERE user_id = ?1",
        params![user_id],
    )?;
    Ok(removed)
}

/// 删单条历史。
pub fn delete(conn: &Connection, id: i64) -> RepoResult<usize> {
    let removed = conn.execute("DELETE FROM play_history WHERE id = ?1", params![id])?;
    Ok(removed)
}

/// where_clause 只由本模块的常量传入，参数一律走 params 绑定。
fn query_history<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> RepoResult<Vec<PlayHistory>> {
    let sql = format!("SELECT {COLUMNS} FROM play_history {where_clause}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(PlayHistory {
            id: row.get("id")?,
            user_id: row.get("user_id")?,
            song_id: row.get("song_id")?,
            played_at: row.get("played_at")?,
            duration_listened_ms: row.get("duration_listened_ms")?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::test_support::{seed_song, seed_user, TestDb};

    #[test]
    fn history_crud_round_trip() {
        let db = TestDb::new("history-crud");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        let song_id = seed_song(&conn, "/music/a.mp3", None);

        let id = record(&conn, user_id, song_id, Some(30_000)).expect("记一次播放");
        let loaded = get(&conn, id).expect("按 id 查").expect("应能查到");
        assert_eq!(loaded.user_id, user_id);
        assert_eq!(loaded.song_id, song_id);
        assert_eq!(loaded.duration_listened_ms, Some(30_000));
        assert!(loaded.played_at > 0, "played_at 应由 repo 盖章");

        // 结构体入口：时长未知也是合法状态
        let second = insert(
            &conn,
            &PlayHistory {
                id: 0,
                user_id,
                song_id,
                played_at: 0,
                duration_listened_ms: None,
            },
        )
        .expect("插第二条");
        assert!(get(&conn, second)
            .expect("查")
            .expect("行在")
            .duration_listened_ms
            .is_none());

        assert_eq!(count_by_user(&conn, user_id).expect("计数"), 2);

        // 删单条
        assert_eq!(delete(&conn, id).expect("删一条"), 1);
        assert!(get(&conn, id).expect("查").is_none());
        assert_eq!(count_by_user(&conn, user_id).expect("计数"), 1);
        assert_eq!(delete(&conn, id).expect("重复删"), 0);
    }

    #[test]
    fn recent_returns_newest_first_and_paginates() {
        let db = TestDb::new("history-recent");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        let songs: Vec<i64> = (0..3)
            .map(|index| seed_song(&conn, &format!("/music/{index}.mp3"), None))
            .collect();
        for (index, song_id) in songs.iter().enumerate() {
            record(&conn, user_id, *song_id, Some(index as i64 * 1000)).expect("记播放");
        }

        let newest = recent(&conn, user_id, 10, 0).expect("最近播放");
        assert_eq!(newest.len(), 3);
        assert_eq!(newest[0].song_id, songs[2], "最新的排最前");
        assert_eq!(newest[1].song_id, songs[1]);
        assert_eq!(newest[2].song_id, songs[0]);
        assert_eq!(newest[0].duration_listened_ms, Some(2000));

        // 分页
        let page = recent(&conn, user_id, 2, 1).expect("第二页");
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].song_id, songs[1]);
        assert_eq!(recent(&conn, user_id, 10, 99).expect("越界页").len(), 0);

        // 别人的历史互不干扰
        let bob = seed_user(&conn, "bob");
        assert!(recent(&conn, bob, 10, 0).expect("bob 的历史").is_empty());
    }

    #[test]
    fn clear_by_user_only_touches_that_user() {
        let db = TestDb::new("history-clear");
        let conn = db.conn();
        let alice = seed_user(&conn, "alice");
        let bob = seed_user(&conn, "bob");
        let song_id = seed_song(&conn, "/music/a.mp3", None);

        record(&conn, alice, song_id, None).expect("alice 播放");
        record(&conn, alice, song_id, None).expect("alice 再播放");
        record(&conn, bob, song_id, None).expect("bob 播放");

        assert_eq!(count_by_user(&conn, alice).expect("alice 计数"), 2);
        assert_eq!(clear_by_user(&conn, alice).expect("清空 alice"), 2);
        assert_eq!(count_by_user(&conn, alice).expect("alice 计数"), 0);
        assert_eq!(
            count_by_user(&conn, bob).expect("bob 计数"),
            1,
            "清空一个人不该动别人的历史"
        );
        assert_eq!(clear_by_user(&conn, alice).expect("重复清空"), 0);
    }
}
