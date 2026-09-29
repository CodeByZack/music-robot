//! song_requests / request_votes 表读写。
//!
//! 点歌去重：dedup_key 上有唯一索引，[find_or_create] 命中即复用、不新建；
//! 投票用复合主键 (request_id, user_id) 保证一人一票，重复投票是幂等的。

use rusqlite::{params, Connection, Row};

use crate::db::models::{RequestStatus, RequestVote, SongRequest};

use super::{RepoError, RepoResult};

const COLUMNS: &str = "id, user_id, title, artist, album, note, dedup_key, status, \
     reject_reason, song_id, created_at, updated_at";

/// 插入点歌请求，返回新行 id；created_at / updated_at 由本函数盖章。
///
/// dedup_key 重复会返回 [RepoError::Conflict] —— 想要「命中即复用」请用
/// [find_or_create]。
pub fn insert(conn: &Connection, request: &SongRequest) -> RepoResult<i64> {
    let now = crate::db::now_unix_ms();
    conn.execute(
        "INSERT INTO song_requests (
             user_id, title, artist, album, note, dedup_key, status,
             reject_reason, song_id, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            request.user_id,
            request.title,
            request.artist,
            request.album,
            request.note,
            request.dedup_key,
            request.status.as_str(),
            request.reject_reason,
            request.song_id,
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 按主键查，查不到返回 None。
pub fn get(conn: &Connection, id: i64) -> RepoResult<Option<SongRequest>> {
    Ok(query_requests(conn, "WHERE id = ?1", params![id])?
        .into_iter()
        .next())
}

/// 按去重键查（有唯一索引，最多一条）。
pub fn find_by_dedup_key(
    conn: &Connection,
    dedup_key: &str,
) -> RepoResult<Option<SongRequest>> {
    Ok(query_requests(conn, "WHERE dedup_key = ?1", params![dedup_key])?
        .into_iter()
        .next())
}

/// 命中已有请求就复用，否则新建。返回 (请求, 是否新建)。
///
/// 并发点歌时两个人可能同时走到 INSERT：唯一索引会把后到的那次顶成
/// [RepoError::Conflict]，这里接住它再查一次，语义仍然是「复用已有请求」，
/// 不会把并发写成两条。
pub fn find_or_create(conn: &Connection, request: &SongRequest) -> RepoResult<(SongRequest, bool)> {
    if let Some(existing) = find_by_dedup_key(conn, &request.dedup_key)? {
        return Ok((existing, false));
    }
    match insert(conn, request) {
        Ok(_) => match find_by_dedup_key(conn, &request.dedup_key)? {
            Some(created) => Ok((created, true)),
            None => Err(RepoError::Invariant {
                message: format!(
                    "刚插入的点歌请求（dedup_key={}）又查不到",
                    request.dedup_key
                ),
            }),
        },
        Err(RepoError::Conflict { .. }) => match find_by_dedup_key(conn, &request.dedup_key)? {
            Some(existing) => Ok((existing, false)),
            None => Err(RepoError::Invariant {
                message: format!(
                    "dedup_key={} 报唯一冲突却查不到对应请求",
                    request.dedup_key
                ),
            }),
        },
        Err(other) => Err(other),
    }
}

/// 状态流转：写 status 与 reject_reason（非 rejected 时传 None 把原因清掉），
/// updated_at 重新盖章。
pub fn update_status(
    conn: &Connection,
    id: i64,
    status: RequestStatus,
    reject_reason: Option<&str>,
) -> RepoResult<usize> {
    let changed = conn.execute(
        "UPDATE song_requests
            SET status = ?2, reject_reason = ?3, updated_at = ?4
          WHERE id = ?1",
        params![id, status.as_str(), reject_reason, crate::db::now_unix_ms()],
    )?;
    Ok(changed)
}

/// 把请求关联到最终入库的歌曲。
pub fn attach_song(conn: &Connection, id: i64, song_id: i64) -> RepoResult<usize> {
    let changed = conn.execute(
        "UPDATE song_requests SET song_id = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, song_id, crate::db::now_unix_ms()],
    )?;
    Ok(changed)
}

/// 列出请求，最新的在前。
pub fn list(conn: &Connection, limit: i64, offset: i64) -> RepoResult<Vec<SongRequest>> {
    query_requests(
        conn,
        "ORDER BY created_at DESC, id DESC LIMIT ?1 OFFSET ?2",
        params![limit, offset],
    )
}

/// 按状态列出请求，最新的在前。
pub fn list_by_status(
    conn: &Connection,
    status: RequestStatus,
    limit: i64,
    offset: i64,
) -> RepoResult<Vec<SongRequest>> {
    query_requests(
        conn,
        "WHERE status = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2 OFFSET ?3",
        params![status.as_str(), limit, offset],
    )
}

/// 全表行数。
pub fn count(conn: &Connection) -> RepoResult<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM song_requests", [], |row| row.get(0))?)
}

/// 删请求；request_votes 会在外键级联下一起删。
pub fn delete(conn: &Connection, id: i64) -> RepoResult<usize> {
    let removed = conn.execute("DELETE FROM song_requests WHERE id = ?1", params![id])?;
    Ok(removed)
}

/// 投一票。已经投过返回 Ok(false)（幂等），新投票返回 Ok(true)。
pub fn add_vote(conn: &Connection, request_id: i64, user_id: i64) -> RepoResult<bool> {
    match conn.execute(
        "INSERT INTO request_votes (request_id, user_id, created_at) VALUES (?1, ?2, ?3)",
        params![request_id, user_id, crate::db::now_unix_ms()],
    ) {
        Ok(_) => Ok(true),
        Err(err) => match RepoError::from(err) {
            // 主键冲突就是「这票已经投过了」，属于正常业务分支
            RepoError::Conflict { .. } => Ok(false),
            other => Err(other),
        },
    }
}

/// 撤票，返回删掉的行数（没投过就是 0）。
pub fn remove_vote(conn: &Connection, request_id: i64, user_id: i64) -> RepoResult<usize> {
    let removed = conn.execute(
        "DELETE FROM request_votes WHERE request_id = ?1 AND user_id = ?2",
        params![request_id, user_id],
    )?;
    Ok(removed)
}

/// 某人是否已投过票。
pub fn has_voted(conn: &Connection, request_id: i64, user_id: i64) -> RepoResult<bool> {
    let found: i64 = conn.query_row(
        "SELECT COUNT(*) FROM request_votes WHERE request_id = ?1 AND user_id = ?2",
        params![request_id, user_id],
        |row| row.get(0),
    )?;
    Ok(found > 0)
}

/// 票数。
pub fn vote_count(conn: &Connection, request_id: i64) -> RepoResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM request_votes WHERE request_id = ?1",
        params![request_id],
        |row| row.get(0),
    )?)
}

/// 列出某请求的全部投票。
pub fn list_votes(conn: &Connection, request_id: i64) -> RepoResult<Vec<RequestVote>> {
    let mut stmt = conn.prepare(
        "SELECT request_id, user_id, created_at FROM request_votes
          WHERE request_id = ?1 ORDER BY created_at, user_id",
    )?;
    let mut rows = stmt.query(params![request_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(RequestVote {
            request_id: row.get("request_id")?,
            user_id: row.get("user_id")?,
            created_at: row.get("created_at")?,
        });
    }
    Ok(out)
}

/// where_clause 只由本模块的常量传入，参数一律走 params 绑定。
fn query_requests<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> RepoResult<Vec<SongRequest>> {
    let sql = format!("SELECT {COLUMNS} FROM song_requests {where_clause}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_request(row)?);
    }
    Ok(out)
}

fn row_to_request(row: &Row<'_>) -> RepoResult<SongRequest> {
    let status_text: String = row.get("status")?;
    let status = RequestStatus::parse(&status_text)?;
    Ok(SongRequest {
        id: row.get("id")?,
        user_id: row.get("user_id")?,
        title: row.get("title")?,
        artist: row.get("artist")?,
        album: row.get("album")?,
        note: row.get("note")?,
        dedup_key: row.get("dedup_key")?,
        status,
        reject_reason: row.get("reject_reason")?,
        song_id: row.get("song_id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::test_support::{seed_song, seed_user, TestDb};

    fn sample_request(user_id: i64, dedup_key: &str) -> SongRequest {
        SongRequest {
            id: 0,
            user_id,
            title: dedup_key.to_string(),
            artist: None,
            album: None,
            note: None,
            dedup_key: dedup_key.to_string(),
            status: RequestStatus::Pending,
            reject_reason: None,
            song_id: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn request_crud_and_status_flow() {
        let db = TestDb::new("requests-crud");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        let song_id = seed_song(&conn, "/music/wanted.mp3", None);

        let mut request = sample_request(user_id, "想听的歌|某人");
        request.title = "想听的歌".to_string();
        request.artist = Some("某人".to_string());
        request.note = Some("麻烦快点".to_string());
        let id = insert(&conn, &request).expect("插请求");

        let loaded = get(&conn, id).expect("按 id 查").expect("应能查到");
        assert_eq!(loaded.title, "想听的歌");
        assert_eq!(loaded.artist.as_deref(), Some("某人"));
        assert_eq!(loaded.status, RequestStatus::Pending);
        assert!(loaded.created_at > 0, "created_at 应由 repo 盖章");
        assert!(loaded.song_id.is_none());

        let by_key = find_by_dedup_key(&conn, "想听的歌|某人")
            .expect("按 dedup_key 查")
            .expect("应能查到");
        assert_eq!(by_key.id, id);

        // 状态流转：pending -> processing -> done（并关联歌曲）
        assert_eq!(
            update_status(&conn, id, RequestStatus::Processing, None).expect("转处理中"),
            1
        );
        assert_eq!(
            get(&conn, id).expect("查").expect("在").status,
            RequestStatus::Processing
        );
        assert_eq!(attach_song(&conn, id, song_id).expect("关联歌曲"), 1);
        assert_eq!(
            update_status(&conn, id, RequestStatus::Done, None).expect("转完成"),
            1
        );
        let done = get(&conn, id).expect("查").expect("在");
        assert_eq!(done.status, RequestStatus::Done);
        assert_eq!(done.song_id, Some(song_id));

        // 另一条请求走拒绝分支，并写上原因
        let rejected = insert(&conn, &sample_request(user_id, "不存在的歌")).expect("插第二条");
        assert_eq!(
            update_status(
                &conn,
                rejected,
                RequestStatus::Rejected,
                Some("版权原因")
            )
            .expect("拒绝"),
            1
        );
        let rejected_row = get(&conn, rejected).expect("查").expect("在");
        assert_eq!(rejected_row.status, RequestStatus::Rejected);
        assert_eq!(rejected_row.reject_reason.as_deref(), Some("版权原因"));

        // 列表：最新的在前
        let all = list(&conn, 10, 0).expect("列出请求");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, rejected, "最新插入的排在前面");
        assert_eq!(
            list_by_status(&conn, RequestStatus::Done, 10, 0)
                .expect("按状态查")
                .len(),
            1
        );
        assert_eq!(
            list_by_status(&conn, RequestStatus::Pending, 10, 0)
                .expect("按状态查")
                .len(),
            0
        );
        assert_eq!(count(&conn).expect("计数"), 2);

        // 删
        assert_eq!(delete(&conn, id).expect("删请求"), 1);
        assert!(get(&conn, id).expect("查").is_none());
        assert_eq!(count(&conn).expect("计数"), 1);
    }

    #[test]
    fn find_or_create_reuses_the_existing_request() {
        let db = TestDb::new("requests-find-or-create");
        let conn = db.conn();
        let alice = seed_user(&conn, "alice");
        let bob = seed_user(&conn, "bob");

        // 第一个人点歌 -> 新建
        let (first, created) =
            find_or_create(&conn, &sample_request(alice, "同一首歌")).expect("首次点歌");
        assert!(created, "第一次应新建");
        assert_eq!(first.user_id, alice);

        // 第二个人点同一首歌 -> 复用第一条，不新建
        let (second, created_again) =
            find_or_create(&conn, &sample_request(bob, "同一首歌")).expect("再次点歌");
        assert!(!created_again, "命中已有请求不该新建");
        assert_eq!(second.id, first.id, "复用同一条请求");
        assert_eq!(second.user_id, alice, "发起人仍是第一个人");
        assert_eq!(count(&conn).expect("计数"), 1);

        // 换个 dedup_key 才会新建
        let (_third, created_new) =
            find_or_create(&conn, &sample_request(bob, "另一首歌")).expect("换一首");
        assert!(created_new);
        assert_eq!(count(&conn).expect("计数"), 2);
    }

    #[test]
    fn duplicate_dedup_key_is_reported_as_a_conflict() {
        let db = TestDb::new("requests-dup-key");
        let conn = db.conn();
        let user_id = seed_user(&conn, "alice");
        insert(&conn, &sample_request(user_id, "同一首歌")).expect("插第一条");
        let err = insert(&conn, &sample_request(user_id, "同一首歌")).expect_err("同键必须冲突");
        assert!(
            matches!(err, RepoError::Conflict { .. }),
            "应是可区分的 Conflict，实际 {err:?}"
        );
        assert_eq!(count(&conn).expect("计数"), 1);
    }

    #[test]
    fn votes_are_idempotent_per_user() {
        let db = TestDb::new("requests-votes");
        let conn = db.conn();
        let alice = seed_user(&conn, "alice");
        let bob = seed_user(&conn, "bob");
        let request_id =
            insert(&conn, &sample_request(alice, "投票的歌")).expect("插请求");

        assert!(add_vote(&conn, request_id, alice).expect("alice 投票"), "首投应成功");
        assert!(
            !add_vote(&conn, request_id, alice).expect("alice 重复投票"),
            "重复投票必须是幂等的 no-op"
        );
        assert!(add_vote(&conn, request_id, bob).expect("bob 投票"));
        assert_eq!(vote_count(&conn, request_id).expect("票数"), 2);
        assert!(has_voted(&conn, request_id, alice).expect("查 alice"));
        assert!(!has_voted(&conn, request_id, 999_999).expect("查路人"));
        assert_eq!(list_votes(&conn, request_id).expect("投票列表").len(), 2);

        // 撤票
        assert_eq!(remove_vote(&conn, request_id, alice).expect("撤票"), 1);
        assert_eq!(remove_vote(&conn, request_id, alice).expect("重复撤票"), 0);
        assert!(!has_voted(&conn, request_id, alice).expect("查 alice"));
        assert_eq!(vote_count(&conn, request_id).expect("票数"), 1);

        // 删请求级联删票
        delete(&conn, request_id).expect("删请求");
        assert_eq!(vote_count(&conn, request_id).expect("票数"), 0);
    }
}
