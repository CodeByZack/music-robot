//! albums 表读写。
//!
//! 曲目数**不落库**：列表用 LEFT JOIN + GROUP BY 现算，所以删掉一首歌之后
//! 计数会立刻跟着变（见 [list_with_song_count] 的测试）。

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::db::models::Album;

use super::RepoResult;

const COLUMNS: &str = "id, name, album_artist, year, cover_data, cover_mime, updated_at";

/// 专辑 + 现算的曲目数。
///
/// 列表专用：刻意不带封面 BLOB，免得翻一页专辑就把所有封面读进内存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumSummary {
    /// 主键
    pub id: i64,
    /// 专辑名
    pub name: String,
    /// 专辑艺术家
    pub album_artist: String,
    /// 发行年份
    pub year: Option<i64>,
    /// 现算的曲目数
    pub song_count: i64,
    /// 更新时间（Unix 毫秒）
    pub updated_at: i64,
}

/// 封面原始字节 + MIME。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    /// 封面字节
    pub data: Vec<u8>,
    /// 封面 MIME
    pub mime: Option<String>,
}

/// 插入专辑，返回新行 id；updated_at 由本函数盖章。
pub fn insert(conn: &Connection, album: &Album) -> RepoResult<i64> {
    conn.execute(
        "INSERT INTO albums (name, album_artist, year, cover_data, cover_mime, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            album.name,
            album.album_artist,
            album.year,
            album.cover_data,
            album.cover_mime,
            crate::db::now_unix_ms(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 按主键查，查不到返回 None（会带上封面 BLOB）。
pub fn get(conn: &Connection, id: i64) -> RepoResult<Option<Album>> {
    Ok(query_albums(conn, "WHERE id = ?1", params![id])?
        .into_iter()
        .next())
}

/// 按 (name, album_artist) 查（有 UNIQUE 约束，最多一条）。
///
/// album_artist 未知时统一传空串：SQLite 的 UNIQUE 不把多个 NULL 当冲突，
/// 用 NULL 会让同一张专辑被反复插入。
pub fn find_by_name_artist(
    conn: &Connection,
    name: &str,
    album_artist: &str,
) -> RepoResult<Option<Album>> {
    Ok(query_albums(
        conn,
        "WHERE name = ?1 AND album_artist = ?2",
        params![name, album_artist],
    )?
    .into_iter()
    .next())
}

/// 覆盖专辑字段（含封面），updated_at 重新盖章。
pub fn update(conn: &Connection, album: &Album) -> RepoResult<usize> {
    let changed = conn.execute(
        "UPDATE albums
            SET name = ?2, album_artist = ?3, year = ?4, cover_data = ?5, cover_mime = ?6,
                updated_at = ?7
          WHERE id = ?1",
        params![
            album.id,
            album.name,
            album.album_artist,
            album.year,
            album.cover_data,
            album.cover_mime,
            crate::db::now_unix_ms(),
        ],
    )?;
    Ok(changed)
}

/// 只写封面（data 传 None 表示清空封面，同时把 MIME 也清掉）。
pub fn update_cover(
    conn: &Connection,
    id: i64,
    data: Option<&[u8]>,
    mime: Option<&str>,
) -> RepoResult<usize> {
    let changed = conn.execute(
        "UPDATE albums SET cover_data = ?2, cover_mime = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, data, mime, crate::db::now_unix_ms()],
    )?;
    Ok(changed)
}

/// 读封面。
///
/// 返回 None 有两种情况：专辑不存在，或者专辑存在但没存封面 —— 两者对调用方
/// 是同一件事（没有封面可发）。需要区分请先用 [get]。
pub fn read_cover(conn: &Connection, id: i64) -> RepoResult<Option<Cover>> {
    let row = conn
        .query_row(
            "SELECT cover_data, cover_mime FROM albums WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, Option<Vec<u8>>>("cover_data")?,
                    row.get::<_, Option<String>>("cover_mime")?,
                ))
            },
        )
        .optional()?;
    match row {
        Some((Some(data), mime)) => Ok(Some(Cover { data, mime })),
        _ => Ok(None),
    }
}

/// 专辑列表 + 现算曲目数。
///
/// 用 LEFT JOIN 而不是 INNER JOIN：一张还没入库歌曲的专辑也应该出现在列表里，
/// 曲目数为 0。GROUP BY 现算意味着歌曲增减之后计数立刻变，不需要任何维护。
///
/// JOIN 条件里带 deleted_at IS NULL：软删（磁盘上文件消失）的歌不该再算进
/// 用户的专辑曲目数。
pub fn list_with_song_count(
    conn: &Connection,
    limit: i64,
    offset: i64,
) -> RepoResult<Vec<AlbumSummary>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.name, a.album_artist, a.year, a.updated_at, COUNT(s.id) AS song_count
           FROM albums a
           LEFT JOIN songs s ON s.album_id = a.id AND s.deleted_at IS NULL
          GROUP BY a.id, a.name, a.album_artist, a.year, a.updated_at
          ORDER BY a.name, a.album_artist, a.id
          LIMIT ?1 OFFSET ?2",
    )?;
    let mut rows = stmt.query(params![limit, offset])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(AlbumSummary {
            id: row.get("id")?,
            name: row.get("name")?,
            album_artist: row.get("album_artist")?,
            year: row.get("year")?,
            song_count: row.get("song_count")?,
            updated_at: row.get("updated_at")?,
        });
    }
    Ok(out)
}

/// 全表行数。
pub fn count(conn: &Connection) -> RepoResult<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM albums", [], |row| row.get(0))?)
}

/// 删专辑；songs.album_id 是 ON DELETE SET NULL，歌还在，只是不再挂这张专辑。
pub fn delete(conn: &Connection, id: i64) -> RepoResult<usize> {
    let removed = conn.execute("DELETE FROM albums WHERE id = ?1", params![id])?;
    Ok(removed)
}

/// where_clause 只由本模块的常量传入，参数一律走 params 绑定。
fn query_albums<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> RepoResult<Vec<Album>> {
    let sql = format!("SELECT {COLUMNS} FROM albums {where_clause}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_album(row)?);
    }
    Ok(out)
}

fn row_to_album(row: &Row<'_>) -> RepoResult<Album> {
    Ok(Album {
        id: row.get("id")?,
        name: row.get("name")?,
        album_artist: row.get("album_artist")?,
        year: row.get("year")?,
        cover_data: row.get("cover_data")?,
        cover_mime: row.get("cover_mime")?,
        updated_at: row.get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::test_support::{seed_album, seed_song, TestDb};
    use crate::db::repos::RepoError;

    fn sample_album(name: &str, album_artist: &str) -> Album {
        Album {
            id: 0,
            name: name.to_string(),
            album_artist: album_artist.to_string(),
            year: None,
            cover_data: None,
            cover_mime: None,
            updated_at: 0,
        }
    }

    #[test]
    fn album_crud_and_cover_round_trip() {
        let db = TestDb::new("albums-crud");
        let conn = db.conn();

        let mut album = sample_album("夜曲", "周杰伦");
        album.year = Some(2005);
        let id = insert(&conn, &album).expect("插专辑");

        let loaded = get(&conn, id).expect("按 id 查").expect("应能查到");
        assert_eq!(loaded.name, "夜曲");
        assert_eq!(loaded.album_artist, "周杰伦");
        assert_eq!(loaded.year, Some(2005));
        assert!(loaded.updated_at > 0, "updated_at 应由 repo 盖章");
        assert!(loaded.cover_data.is_none());

        let by_key = find_by_name_artist(&conn, "夜曲", "周杰伦")
            .expect("按 (name, artist) 查")
            .expect("应能查到");
        assert_eq!(by_key.id, id);
        assert!(find_by_name_artist(&conn, "夜曲", "别人")
            .expect("按 (name, artist) 查")
            .is_none());

        // 写封面 / 读封面
        let bytes = vec![0xFFu8, 0xD8, 0xFF, 0xE0];
        assert_eq!(
            update_cover(&conn, id, Some(&bytes), Some("image/jpeg")).expect("写封面"),
            1
        );
        let cover = read_cover(&conn, id).expect("读封面").expect("应有封面");
        assert_eq!(cover.data, bytes);
        assert_eq!(cover.mime.as_deref(), Some("image/jpeg"));

        // 清封面
        assert_eq!(update_cover(&conn, id, None, None).expect("清封面"), 1);
        assert!(read_cover(&conn, id).expect("读封面").is_none());
        assert!(read_cover(&conn, 9999).expect("不存在的专辑读封面").is_none());

        // 改：整行覆盖
        let mut edited = loaded.clone();
        edited.name = "十一月的萧邦".to_string();
        edited.cover_data = Some(vec![1, 2, 3]);
        edited.cover_mime = Some("image/png".to_string());
        assert_eq!(update(&conn, &edited).expect("改专辑"), 1);
        let reloaded = get(&conn, id).expect("重查").expect("应能查到");
        assert_eq!(reloaded.name, "十一月的萧邦");
        assert_eq!(reloaded.cover_data.as_deref(), Some(&[1u8, 2, 3][..]));

        // 删
        assert_eq!(delete(&conn, id).expect("删专辑"), 1);
        assert!(get(&conn, id).expect("重查").is_none());
        assert_eq!(count(&conn).expect("计数"), 0);
    }

    #[test]
    fn duplicate_name_and_artist_is_reported_as_a_conflict() {
        let db = TestDb::new("albums-dup-key");
        let conn = db.conn();
        insert(&conn, &sample_album("夜曲", "周杰伦")).expect("插第一张");
        let err = insert(&conn, &sample_album("夜曲", "周杰伦")).expect_err("同键必须冲突");
        assert!(
            matches!(err, RepoError::Conflict { .. }),
            "应是可区分的 Conflict，实际 {err:?}"
        );
        assert_eq!(count(&conn).expect("计数"), 1);
        // 换个 album_artist 就不是同一张专辑
        insert(&conn, &sample_album("夜曲", "别人")).expect("不同艺术家可以同名");
        assert_eq!(count(&conn).expect("计数"), 2);
    }

    #[test]
    fn song_count_is_computed_by_group_by_and_follows_song_deletions() {
        let db = TestDb::new("albums-group-by");
        let conn = db.conn();

        let nocturne = seed_album(&conn, "夜曲", "周杰伦");
        let chopin = seed_album(&conn, "肖邦练习曲", "阿图尔");
        // 第三张：一首歌都没有，也必须出现在列表里且计数为 0
        let empty = seed_album(&conn, "空专辑", "无人");

        let a1 = seed_song(&conn, "/music/1.mp3", Some(nocturne));
        seed_song(&conn, "/music/2.mp3", Some(nocturne));
        seed_song(&conn, "/music/3.mp3", Some(chopin));
        // 不挂专辑的歌不该被算进任何一张
        seed_song(&conn, "/music/4.mp3", None);

        let summaries = list_with_song_count(&conn, 50, 0).expect("专辑列表");
        assert_eq!(summaries.len(), 3);
        let count_of = |name: &str| {
            summaries
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.song_count)
                .unwrap_or(-1)
        };
        assert_eq!(count_of("夜曲"), 2, "GROUP BY 现算的曲目数");
        assert_eq!(count_of("肖邦练习曲"), 1);
        let empty_row = summaries
            .iter()
            .find(|s| s.id == empty)
            .expect("没歌的专辑也必须出现在列表里");
        assert_eq!(empty_row.song_count, 0, "LEFT JOIN 下没歌的专辑计数为 0");

        // 标记删掉一首歌，计数必须立刻跟着变 —— 证明是现算而不是存下来的
        super::super::songs::mark_deleted(&conn, a1).expect("标记删掉一首歌");
        let after = list_with_song_count(&conn, 50, 0).expect("专辑列表");
        let nocturne_after = after
            .iter()
            .find(|s| s.id == nocturne)
            .expect("夜曲还在列表里");
        assert_eq!(nocturne_after.song_count, 1, "软删一首歌后计数必须减少");

        // 删专辑：歌不被连带删掉，只是 album_id 置空
        delete(&conn, nocturne).expect("删专辑");
        assert!(get(&conn, nocturne).expect("重查").is_none());
        assert_eq!(
            super::super::songs::count(&conn, false).expect("删专辑后在库歌数"),
            3,
            "songs.album_id 是 SET NULL，歌不该被连带删"
        );

        // 分页也生效
        let page = list_with_song_count(&conn, 2, 0).expect("第一页");
        assert_eq!(page.len(), 2);
    }
}
