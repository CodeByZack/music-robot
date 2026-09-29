//! playlists / playlist_items 表读写。
//!
//! position 的不变量：同一歌单内恒为连续的 1..n。
//! 往中间插歌会让后面的顺移；删歌、挪位置之后会重新编号，
//! 所以调用方可以直接把 position 当展示序号用。

use rusqlite::{params, Connection, Row};

use crate::db::models::{Playlist, PlaylistItem};

use super::{in_savepoint, RepoResult};

const PLAYLIST_COLUMNS: &str = "id, user_id, name, description, is_public, created_at, updated_at";
const ITEM_COLUMNS: &str = "playlist_id, song_id, position, added_at";

/// 建房单，返回新行 id；created_at / updated_at 由本函数盖章。
pub fn insert(conn: &Connection, playlist: &Playlist) -> RepoResult<i64> {
    let now = crate::db::now_unix_ms();
    conn.execute(
        "INSERT INTO playlists (user_id, name, description, is_public, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![
            playlist.user_id,
            playlist.name,
            playlist.description,
            bool_to_int(playlist.is_public),
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 按主键查，查不到返回 None。
pub fn get(conn: &Connection, id: i64) -> RepoResult<Option<Playlist>> {
    Ok(query_playlists(conn, "WHERE id = ?1", params![id])?
        .into_iter()
        .next())
}

/// 覆盖歌单字段（不碰 created_at），updated_at 重新盖章。
pub fn update(conn: &Connection, playlist: &Playlist) -> RepoResult<usize> {
    let changed = conn.execute(
        "UPDATE playlists
            SET name = ?2, description = ?3, is_public = ?4, updated_at = ?5
          WHERE id = ?1",
        params![
            playlist.id,
            playlist.name,
            playlist.description,
            bool_to_int(playlist.is_public),
            crate::db::now_unix_ms(),
        ],
    )?;
    Ok(changed)
}

/// 只切公开 / 私有。
pub fn set_public(conn: &Connection, id: i64, is_public: bool) -> RepoResult<usize> {
    let changed = conn.execute(
        "UPDATE playlists SET is_public = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, bool_to_int(is_public), crate::db::now_unix_ms()],
    )?;
    Ok(changed)
}

/// 删歌单；playlist_items 会在外键级联下一起删。
pub fn delete(conn: &Connection, id: i64) -> RepoResult<usize> {
    let removed = conn.execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
    Ok(removed)
}

/// 列某个用户的全部歌单（含私有），按 id 升序。
pub fn list_by_owner(conn: &Connection, user_id: i64) -> RepoResult<Vec<Playlist>> {
    query_playlists(conn, "WHERE user_id = ?1 ORDER BY id", params![user_id])
}

/// 列 viewer 能看到的歌单：自己的全部 + 别人的公开歌单。
///
/// viewer 传 None（未登录）时只剩公开歌单：SQL 里 user_id = NULL 恒为 NULL，
/// 不会命中任何私有歌单。
pub fn list_visible_to(conn: &Connection, viewer: Option<i64>) -> RepoResult<Vec<Playlist>> {
    query_playlists(
        conn,
        "WHERE is_public = 1 OR user_id = ?1 ORDER BY id",
        params![viewer],
    )
}

/// 全表行数。
pub fn count(conn: &Connection) -> RepoResult<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM playlists", [], |row| row.get(0))?)
}

/// 往歌单里加一首歌，返回它最终的 position。
///
/// position 传 None 表示追加到末尾；给了位置就插到那里，原来的歌往后顺移。
/// 越界的位置会被夹到 [1, 末尾 + 1]，保证不出现空洞。
pub fn add_item(
    conn: &Connection,
    playlist_id: i64,
    song_id: i64,
    position: Option<i64>,
) -> RepoResult<i64> {
    in_savepoint(conn, "mr_playlist_add_item", || {
        let max_position: i64 = conn.query_row(
            "SELECT COALESCE(MAX(position), 0) FROM playlist_items WHERE playlist_id = ?1",
            params![playlist_id],
            |row| row.get(0),
        )?;
        let target = match position {
            Some(requested) => requested.clamp(1, max_position + 1),
            None => max_position + 1,
        };
        conn.execute(
            "UPDATE playlist_items
                SET position = position + 1
              WHERE playlist_id = ?1 AND position >= ?2",
            params![playlist_id, target],
        )?;
        conn.execute(
            "INSERT INTO playlist_items (playlist_id, song_id, position, added_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![playlist_id, song_id, target, crate::db::now_unix_ms()],
        )?;
        Ok(target)
    })
}

/// 从歌单里删一首歌，并把剩下的条目重排成连续的 1..n。返回删掉的行数。
pub fn remove_item(conn: &Connection, playlist_id: i64, song_id: i64) -> RepoResult<usize> {
    in_savepoint(conn, "mr_playlist_remove_item", || {
        let removed = conn.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1 AND song_id = ?2",
            params![playlist_id, song_id],
        )?;
        if removed > 0 {
            resequence(conn, playlist_id)?;
        }
        Ok(removed)
    })
}

/// 把 song_id 挪到 new_position（1 起，越界自动夹到两端），其余歌顺移，
/// 挪完 position 仍是连续的 1..n。返回歌单里的条目数；歌不在歌单里时返回 0。
pub fn reorder_item(
    conn: &Connection,
    playlist_id: i64,
    song_id: i64,
    new_position: i64,
) -> RepoResult<usize> {
    in_savepoint(conn, "mr_playlist_reorder_item", || {
        let mut song_ids = ordered_song_ids(conn, playlist_id)?;
        let from = match song_ids.iter().position(|id| *id == song_id) {
            Some(index) => index,
            None => return Ok(0),
        };
        let moved = song_ids.remove(from);
        // 取出一首之后，能插入的下标范围是 0..=len
        let last = song_ids.len() as i64;
        let to = new_position.saturating_sub(1).clamp(0, last) as usize;
        song_ids.insert(to, moved);
        write_positions(conn, playlist_id, &song_ids)?;
        Ok(song_ids.len())
    })
}

/// 按 position 升序列出歌单条目。
pub fn list_items(conn: &Connection, playlist_id: i64) -> RepoResult<Vec<PlaylistItem>> {
    let sql = format!(
        "SELECT {ITEM_COLUMNS} FROM playlist_items WHERE playlist_id = ?1 ORDER BY position, song_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![playlist_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(PlaylistItem {
            playlist_id: row.get("playlist_id")?,
            song_id: row.get("song_id")?,
            position: row.get("position")?,
            added_at: row.get("added_at")?,
        });
    }
    Ok(out)
}

/// 这首歌被哪些歌单收了（按歌单 id 升序）。
pub fn playlists_for_song(conn: &Connection, song_id: i64) -> RepoResult<Vec<Playlist>> {
    let sql = format!(
        "SELECT p.id, p.user_id, p.name, p.description, p.is_public, p.created_at, p.updated_at
           FROM playlists p
           JOIN playlist_items i ON i.playlist_id = p.id
          WHERE i.song_id = ?1
          ORDER BY p.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![song_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_playlist(row)?);
    }
    Ok(out)
}

/// 把歌单条目的 position 重排成连续的 1..n（按当前 position, song_id 顺序）。
fn resequence(conn: &Connection, playlist_id: i64) -> RepoResult<()> {
    let song_ids = ordered_song_ids(conn, playlist_id)?;
    write_positions(conn, playlist_id, &song_ids)
}

/// 按当前顺序读出歌单里的 song_id。
fn ordered_song_ids(conn: &Connection, playlist_id: i64) -> RepoResult<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT song_id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position, song_id",
    )?;
    let mut rows = stmt.query(params![playlist_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row.get(0)?);
    }
    Ok(out)
}

/// 按给定顺序把 position 覆写成 1..n。
fn write_positions(conn: &Connection, playlist_id: i64, song_ids: &[i64]) -> RepoResult<()> {
    for (index, song_id) in song_ids.iter().enumerate() {
        conn.execute(
            "UPDATE playlist_items SET position = ?3 WHERE playlist_id = ?1 AND song_id = ?2",
            params![playlist_id, song_id, index as i64 + 1],
        )?;
    }
    Ok(())
}

/// where_clause 只由本模块的常量传入，参数一律走 params 绑定。
fn query_playlists<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> RepoResult<Vec<Playlist>> {
    let sql = format!("SELECT {PLAYLIST_COLUMNS} FROM playlists {where_clause}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_playlist(row)?);
    }
    Ok(out)
}

fn row_to_playlist(row: &Row<'_>) -> RepoResult<Playlist> {
    let is_public: i64 = row.get("is_public")?;
    Ok(Playlist {
        id: row.get("id")?,
        user_id: row.get("user_id")?,
        name: row.get("name")?,
        description: row.get("description")?,
        is_public: is_public != 0,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn bool_to_int(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::test_support::{seed_song, seed_user, TestDb};
    use crate::db::repos::RepoError;

    fn sample_playlist(user_id: i64, name: &str) -> Playlist {
        Playlist {
            id: 0,
            user_id,
            name: name.to_string(),
            description: None,
            is_public: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn playlist_crud_round_trip() {
        let db = TestDb::new("playlists-crud");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");

        let mut playlist = sample_playlist(user_id, "我的歌单");
        playlist.description = Some("随便听听".to_string());
        let id = insert(&conn, &playlist).expect("建歌单");

        let loaded = get(&conn, id).expect("按 id 查").expect("应能查到");
        assert_eq!(loaded.name, "我的歌单");
        assert_eq!(loaded.description.as_deref(), Some("随便听听"));
        assert!(!loaded.is_public);
        assert!(loaded.created_at > 0, "created_at 应由 repo 盖章");
        assert!(loaded.updated_at > 0);

        // 改
        let mut edited = loaded.clone();
        edited.name = "改名了".to_string();
        edited.description = None;
        edited.is_public = true;
        assert_eq!(update(&conn, &edited).expect("改歌单"), 1);
        let reloaded = get(&conn, id).expect("重查").expect("应能查到");
        assert_eq!(reloaded.name, "改名了");
        assert!(reloaded.description.is_none());
        assert!(reloaded.is_public);

        assert_eq!(set_public(&conn, id, false).expect("切私有"), 1);
        assert!(!get(&conn, id).expect("重查").expect("应能查到").is_public);

        // 查
        assert_eq!(count(&conn).expect("计数"), 1);
        assert_eq!(list_by_owner(&conn, user_id).expect("按 owner 查").len(), 1);
        assert_eq!(
            list_by_owner(&conn, user_id + 999)
                .expect("别人的 owner 查")
                .len(),
            0
        );

        // 删
        assert_eq!(delete(&conn, id).expect("删歌单"), 1);
        assert!(get(&conn, id).expect("重查").is_none());
        assert_eq!(count(&conn).expect("计数"), 0);
    }

    #[test]
    fn playlist_pointing_at_a_missing_user_is_a_foreign_key_error() {
        let db = TestDb::new("playlists-fk");
        let conn = db.conn();
        let err = insert(&conn, &sample_playlist(9999, "野歌单")).expect_err("属主不存在必须失败");
        assert!(
            matches!(err, RepoError::ForeignKey { .. }),
            "应是可区分的外键错误，实际 {err:?}"
        );
        assert_eq!(count(&conn).expect("计数"), 0);
    }

    #[test]
    fn visibility_hides_other_users_private_playlists() {
        let db = TestDb::new("playlists-visibility");
        let conn = db.conn();
        let alice = seed_user(&conn, "alice");
        let bob = seed_user(&conn, "bob");

        let alice_private = insert(&conn, &sample_playlist(alice, "alice 私有")).expect("建");
        let mut public = sample_playlist(alice, "alice 公开");
        public.is_public = true;
        let alice_public = insert(&conn, &public).expect("建");
        let bob_private = insert(&conn, &sample_playlist(bob, "bob 私有")).expect("建");

        // alice 能看到自己的全部 + 别人的公开
        let alice_sees = list_visible_to(&conn, Some(alice)).expect("alice 视角");
        let alice_ids: Vec<i64> = alice_sees.iter().map(|p| p.id).collect();
        assert!(alice_ids.contains(&alice_private));
        assert!(alice_ids.contains(&alice_public));
        assert!(!alice_ids.contains(&bob_private), "别人的私有歌单不该出现");

        // bob 看不到 alice 的私有歌单
        let bob_sees = list_visible_to(&conn, Some(bob)).expect("bob 视角");
        let bob_ids: Vec<i64> = bob_sees.iter().map(|p| p.id).collect();
        assert!(!bob_ids.contains(&alice_private));
        assert!(bob_ids.contains(&alice_public));
        assert!(bob_ids.contains(&bob_private));

        // 未登录只能看到公开的
        let anonymous = list_visible_to(&conn, None).expect("未登录视角");
        let anonymous_ids: Vec<i64> = anonymous.iter().map(|p| p.id).collect();
        assert_eq!(anonymous_ids, vec![alice_public]);

        // 按 owner 查不受可见性影响
        assert_eq!(list_by_owner(&conn, alice).expect("alice 的歌单").len(), 2);
    }

    #[test]
    fn items_can_be_added_removed_and_reordered_without_position_gaps() {
        let db = TestDb::new("playlists-items");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        let playlist_id = insert(&conn, &sample_playlist(user_id, "顺序")).expect("建歌单");
        let songs: Vec<i64> = (0..4)
            .map(|index| seed_song(&conn, &format!("/music/{index}.mp3"), None))
            .collect();

        // 追加
        for (index, song_id) in songs.iter().enumerate() {
            let position = add_item(&conn, playlist_id, *song_id, None).expect("追加");
            assert_eq!(position, index as i64 + 1, "追加的 position 应连续");
        }

        // 插到中间：后面的顺移
        let extra = seed_song(&conn, "/music/extra.mp3", None);
        assert_eq!(add_item(&conn, playlist_id, extra, Some(2)).expect("插到第 2 位"), 2);
        let after_insert = item_order(&conn, playlist_id);
        assert_eq!(
            after_insert,
            vec![songs[0], extra, songs[1], songs[2], songs[3]],
            "插队后顺序应正确"
        );
        assert_contiguous_positions(&conn, playlist_id, 5);

        // 越界位置夹到末尾
        let tail = seed_song(&conn, "/music/tail.mp3", None);
        assert_eq!(
            add_item(&conn, playlist_id, tail, Some(999)).expect("越界位置夹到末尾"),
            6
        );
        assert_eq!(item_order(&conn, playlist_id).last(), Some(&tail));
        // 非法位置（<= 0）夹到最前。这里必须换一首歌：主键是 (playlist_id, song_id)，
        // 同一首歌在同一个歌单里只能出现一次。
        let head = seed_song(&conn, "/music/head.mp3", None);
        assert_eq!(
            add_item(&conn, playlist_id, head, Some(0)).expect("非法位置夹到最前"),
            1
        );
        assert_eq!(item_order(&conn, playlist_id)[0], head);

        // 删中间一首，剩下的重排成 1..n
        assert_eq!(remove_item(&conn, playlist_id, extra).expect("删条目"), 1);
        assert_contiguous_positions(&conn, playlist_id, 6);
        assert!(!item_order(&conn, playlist_id).contains(&extra));
        assert_eq!(remove_item(&conn, playlist_id, extra).expect("重复删"), 0);

        // 挪位置
        let order_before = item_order(&conn, playlist_id);
        let last_song = *order_before.last().expect("非空");
        assert_eq!(
            reorder_item(&conn, playlist_id, last_song, 1).expect("挪到最前"),
            order_before.len()
        );
        let order_after = item_order(&conn, playlist_id);
        assert_eq!(order_after[0], last_song);
        assert_contiguous_positions(&conn, playlist_id, order_before.len());
        assert_eq!(
            reorder_item(&conn, playlist_id, 999_999, 1).expect("歌不在歌单里"),
            0
        );

        // 删歌单级联删条目
        delete(&conn, playlist_id).expect("删歌单");
        assert!(list_items(&conn, playlist_id).expect("条目").is_empty());
    }

    #[test]
    fn playlists_for_song_lists_every_owner_playlist() {
        let db = TestDb::new("playlists-by-song");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        let first = insert(&conn, &sample_playlist(user_id, "歌单甲")).expect("建甲");
        let second = insert(&conn, &sample_playlist(user_id, "歌单乙")).expect("建乙");
        let other = insert(&conn, &sample_playlist(user_id, "歌单丙")).expect("建丙");
        let song_a = seed_song(&conn, "/music/a.mp3", None);
        let song_b = seed_song(&conn, "/music/b.mp3", None);

        add_item(&conn, first, song_a, None).expect("甲加 a");
        add_item(&conn, second, song_a, None).expect("乙加 a");
        add_item(&conn, other, song_b, None).expect("丙加 b");

        let owners = playlists_for_song(&conn, song_a).expect("查所属歌单");
        let ids: Vec<i64> = owners.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![first, second]);
        assert_eq!(
            playlists_for_song(&conn, song_b).expect("查 b"),
            vec![get(&conn, other).expect("查").expect("存在")]
        );
        assert!(playlists_for_song(&conn, 999_999)
            .expect("没人收的歌")
            .is_empty());
    }

    #[test]
    fn item_ops_inside_a_caller_transaction_roll_back_together() {
        let db = TestDb::new("playlists-tx");
        let mut conn = db.conn();
        let user_id = seed_user(&conn, "dave");
        let song_a = seed_song(&conn, "/music/a.mp3", None);
        let song_b = seed_song(&conn, "/music/b.mp3", None);
        let playlist_id =
            insert(&conn, &sample_playlist(user_id, "事务歌单")).expect("建歌单");

        {
            let tx = conn.transaction().expect("开事务");
            // add_item 内部会开 SAVEPOINT，必须能在外层事务里正确嵌套
            add_item(&tx, playlist_id, song_a, None).expect("事务内加 A");
            add_item(&tx, playlist_id, song_b, None).expect("事务内加 B");
            assert_eq!(list_items(&tx, playlist_id).expect("事务内可见").len(), 2);
            tx.rollback().expect("回滚");
        }
        assert!(
            list_items(&conn, playlist_id).expect("回滚后条目").is_empty(),
            "外层回滚后条目都不该在"
        );
    }

    fn item_order(conn: &Connection, playlist_id: i64) -> Vec<i64> {
        list_items(conn, playlist_id)
            .expect("读条目")
            .into_iter()
            .map(|item| item.song_id)
            .collect()
    }

    fn assert_contiguous_positions(conn: &Connection, playlist_id: i64, expected: usize) {
        let items = list_items(conn, playlist_id).expect("读条目");
        assert_eq!(items.len(), expected, "条目数不对");
        for (index, item) in items.iter().enumerate() {
            assert_eq!(
                item.position,
                index as i64 + 1,
                "position 必须是连续的 1..n，实际 {items:?}"
            );
        }
    }
}
