//! songs 表读写 —— 曲库的核心表。
//!
//! ## 软删除（迁移 v2 的 deleted_at）
//!
//! 磁盘上的文件消失只写 deleted_at（[mark_deleted]），**绝不硬删**：硬删会按
//! 外键动作 CASCADE 掉用户的歌单条目 / 收藏 / 播放历史，移动硬盘临时没挂载就会
//! 造成不可逆的数据损失。于是这里的读写约定是：
//!
//!   * 面向用户的查询（[get] / [list] / [search] / [count]）默认只返回
//!     deleted_at IS NULL 的行；需要连软删行一起看时传 include_deleted = true，
//!     管理端「回收站」另有 [list_deleted]；
//!   * [find_by_file_path] **刻意不过滤**软删行：扫描发现文件又回来了，要能拿到
//!     那一行去 [restore]，而不是当新歌重插（重插会撞 file_path 的唯一约束）；
//!   * 真正物理删除只有 [purge]，它的文档里写明了连带后果。

use rusqlite::{params, Connection, Row};

use crate::db::models::{ScrapeStatus, Song};

use super::RepoResult;

/// 歌手列表的一行 —— 「歌手」在后端就是 `songs.artists` 这个**整串**。
///
/// ⚠️ 为什么按整串分组、而不是按 ` / ` 拆开：歌手详情（`GET /api/artists/{name}`）
/// 用的就是 `artists = ?` 的**精确匹配**。拆开分组的话，列表里点「银临」会进到
/// 一个详情页里又找不到歌 —— 列表与详情口径必须一致。
/// 这是既有设计，不是这里偷懒。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistSummary {
    /// 歌手串（就是 `songs.artists`）
    pub name: String,
    /// 该歌手的曲目数
    pub song_count: i64,
    /// 该歌手出现过的专辑数（去重）
    pub album_count: i64,
}

/// 歌手列表，按曲目数降序（同数目按名字，保证分页稳定）。
pub fn list_artists(
    conn: &Connection,
    limit: i64,
    offset: i64,
) -> RepoResult<Vec<ArtistSummary>> {
    let mut stmt = conn.prepare(
        "SELECT artists AS name,
                COUNT(*) AS song_count,
                COUNT(DISTINCT album_id) AS album_count
           FROM songs
          WHERE deleted_at IS NULL AND artists IS NOT NULL AND TRIM(artists) <> ''
          GROUP BY artists
          ORDER BY song_count DESC, name
          LIMIT ?1 OFFSET ?2",
    )?;
    let mut rows = stmt.query(params![limit, offset])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(ArtistSummary {
            name: row.get("name")?,
            song_count: row.get("song_count")?,
            album_count: row.get("album_count")?,
        });
    }
    Ok(out)
}

/// 歌手总数（去重后的 `artists` 串数量）。
pub fn count_artists(conn: &Connection) -> RepoResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(DISTINCT artists) FROM songs
          WHERE deleted_at IS NULL AND artists IS NOT NULL AND TRIM(artists) <> ''",
        [],
        |row| row.get(0),
    )?)
}

/// 按 id 批量取曲目。给「历史 + 曲目摘要」这类**一次拿一批**的场景用，
/// 避免每条历史各打一次查询。
pub fn get_many(conn: &Connection, ids: &[i64]) -> RepoResult<Vec<Song>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // 用占位符拼 IN —— 参数照旧走绑定，不把值拼进 SQL
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT {COLUMNS} FROM songs WHERE deleted_at IS NULL AND id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(ids))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_song(row)?);
    }
    Ok(out)
}

/// 查询 / 返回统一使用的列清单，与 [row_to_song] 一一对应。
const COLUMNS: &str = "id, file_path, album_id, title, artists, album_artist, year, genres, \
     track, disc, duration_ms, bitrate_bps, format, audio_hash, file_size, file_mtime, \
     search_text, lyrics, scrape_status, scrape_error, scrape_at, deleted_at, added_at, updated_at";

/// 插入一首歌，返回新行 id。
///
/// added_at / updated_at 由本函数盖章，song 结构体里的同名字段被忽略；
/// deleted_at 一律写 NULL —— 新入库的歌必然是在库状态。
pub fn insert(conn: &Connection, song: &Song) -> RepoResult<i64> {
    let now = crate::db::now_unix_ms();
    conn.execute(
        "INSERT INTO songs (
             file_path, album_id, title, artists, album_artist, year, genres, track, disc,
             duration_ms, bitrate_bps, format, audio_hash, file_size, file_mtime,
             search_text, lyrics, scrape_status, scrape_error, scrape_at, added_at, updated_at
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
             ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22
         )",
        params![
            song.file_path,
            song.album_id,
            song.title,
            song.artists,
            song.album_artist,
            song.year,
            song.genres,
            song.track,
            song.disc,
            song.duration_ms,
            song.bitrate_bps,
            song.format,
            song.audio_hash,
            song.file_size,
            song.file_mtime,
            song.search_text,
            song.lyrics,
            song.scrape_status.as_str(),
            song.scrape_error,
            song.scrape_at,
            now,
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 按主键查。
///
/// include_deleted = false 时软删行视同不存在；扫描 / 刮削这类内部流程要拿
/// 软删行请传 true。
pub fn get(conn: &Connection, id: i64, include_deleted: bool) -> RepoResult<Option<Song>> {
    let filter = if include_deleted {
        "WHERE id = ?1"
    } else {
        "WHERE id = ?1 AND deleted_at IS NULL"
    };
    Ok(query_songs(conn, filter, params![id])?.into_iter().next())
}

/// 按文件路径查（file_path 有 UNIQUE 约束，最多一条）。
///
/// **刻意不过滤 deleted_at**：文件消失被标记软删之后，扫描又看到同一个路径时
/// 必须能查到那一行，才能走 [restore] 复原；当成新歌重插会直接撞唯一约束。
pub fn find_by_file_path(conn: &Connection, file_path: &str) -> RepoResult<Option<Song>> {
    Ok(query_songs(conn, "WHERE file_path = ?1", params![file_path])?
        .into_iter()
        .next())
}

/// 按裸音频哈希查。
///
/// audio_hash 上没有唯一索引（同一份音频可能被复制成多个文件），所以返回列表：
/// 调用方要么全用，要么自己挑第一条。默认不含软删行。
pub fn find_by_audio_hash(
    conn: &Connection,
    audio_hash: &str,
    include_deleted: bool,
) -> RepoResult<Vec<Song>> {
    let filter = if include_deleted {
        "WHERE audio_hash = ?1 ORDER BY id"
    } else {
        "WHERE audio_hash = ?1 AND deleted_at IS NULL ORDER BY id"
    };
    query_songs(conn, filter, params![audio_hash])
}

/// 分页列表，按 id 升序（= 入库顺序）；默认不含软删行。
pub fn list(
    conn: &Connection,
    limit: i64,
    offset: i64,
    include_deleted: bool,
) -> RepoResult<Vec<Song>> {
    let filter = if include_deleted {
        "ORDER BY id LIMIT ?1 OFFSET ?2"
    } else {
        "WHERE deleted_at IS NULL ORDER BY id LIMIT ?1 OFFSET ?2"
    };
    query_songs(conn, filter, params![limit, offset])
}

/// 只列已标记软删的歌（管理端「回收站」），按 id 升序。
pub fn list_deleted(conn: &Connection, limit: i64, offset: i64) -> RepoResult<Vec<Song>> {
    query_songs(
        conn,
        "WHERE deleted_at IS NOT NULL ORDER BY id LIMIT ?1 OFFSET ?2",
        params![limit, offset],
    )
}

/// 行数；include_deleted = false 时只数在库的歌。
pub fn count(conn: &Connection, include_deleted: bool) -> RepoResult<i64> {
    let sql = if include_deleted {
        "SELECT COUNT(*) FROM songs"
    } else {
        "SELECT COUNT(*) FROM songs WHERE deleted_at IS NULL"
    };
    Ok(conn.query_row(sql, [], |row| row.get(0))?)
}

/// 已标记软删的行数。
pub fn count_deleted(conn: &Connection) -> RepoResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM songs WHERE deleted_at IS NOT NULL",
        [],
        |row| row.get(0),
    )?)
}

/// 按 search_text 做子串搜索；默认不含软删行。
///
/// 行为约定（都与测试一一对应）：
///   * 部分匹配：关键词是 search_text 的子串即命中；
///   * 大小写：走 SQLite 的 LIKE，**ASCII 字母大小写不敏感**；中文没有大小写概念，
///     按子串精确匹配；
///   * 关键词里的 % 和 _ 会被转义成普通字符，不当通配符用；
///   * 搜不到返回空列表，不报错；
///   * 空白关键词不是「搜索全部」，直接返回空 —— 要列全部请用 [list]。
pub fn search(
    conn: &Connection,
    keyword: &str,
    limit: i64,
    offset: i64,
    include_deleted: bool,
) -> RepoResult<Vec<Song>> {
    let trimmed = keyword.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = format!("%{}%", escape_like(trimmed));
    let filter = if include_deleted {
        "WHERE search_text LIKE ?1 ESCAPE '\\' ORDER BY id LIMIT ?2 OFFSET ?3"
    } else {
        "WHERE deleted_at IS NULL AND search_text LIKE ?1 ESCAPE '\\' \
         ORDER BY id LIMIT ?2 OFFSET ?3"
    };
    query_songs(conn, filter, params![pattern, limit, offset])
}

/// 用 song 里的标签字段覆盖库里对应的行；added_at / deleted_at 不动，
/// updated_at 重新盖章。软删行也能改标签（文件回来时标签已经顺手读好了）。
pub fn update_tags(conn: &Connection, song: &Song) -> RepoResult<usize> {
    let now = crate::db::now_unix_ms();
    let changed = conn.execute(
        "UPDATE songs SET
             album_id = ?2, title = ?3, artists = ?4, album_artist = ?5, year = ?6,
             genres = ?7, track = ?8, disc = ?9, duration_ms = ?10, bitrate_bps = ?11,
             format = ?12, audio_hash = ?13, file_size = ?14, file_mtime = ?15,
             search_text = ?16, lyrics = ?17, updated_at = ?18
         WHERE id = ?1",
        params![
            song.id,
            song.album_id,
            song.title,
            song.artists,
            song.album_artist,
            song.year,
            song.genres,
            song.track,
            song.disc,
            song.duration_ms,
            song.bitrate_bps,
            song.format,
            song.audio_hash,
            song.file_size,
            song.file_mtime,
            song.search_text,
            song.lyrics,
            now,
        ],
    )?;
    Ok(changed)
}

/// 改刮削状态（可选带错误信息），同时把 scrape_at / updated_at 盖成当前时间。
pub fn update_scrape_status(
    conn: &Connection,
    id: i64,
    status: ScrapeStatus,
    scrape_error: Option<&str>,
) -> RepoResult<usize> {
    let now = crate::db::now_unix_ms();
    let changed = conn.execute(
        "UPDATE songs
            SET scrape_status = ?2, scrape_error = ?3, scrape_at = ?4, updated_at = ?4
          WHERE id = ?1",
        params![id, status.as_str(), scrape_error, now],
    )?;
    Ok(changed)
}

/// 标记删除：写 deleted_at = 当前时间。
///
/// 幂等 —— 已经是删除态时保持原时间戳、连 updated_at 也不动，返回 0；
/// 这次真正标记了才返回 1。行不存在同样返回 0。
pub fn mark_deleted(conn: &Connection, song_id: i64) -> RepoResult<usize> {
    let now = crate::db::now_unix_ms();
    let changed = conn.execute(
        "UPDATE songs SET deleted_at = ?2, updated_at = ?2
          WHERE id = ?1 AND deleted_at IS NULL",
        params![song_id, now],
    )?;
    Ok(changed)
}

/// 复原：把 deleted_at 置回 NULL（文件又回来了）。
///
/// 幂等 —— 本来就在库的行返回 0，这次真的复原了才返回 1。
pub fn restore(conn: &Connection, song_id: i64) -> RepoResult<usize> {
    let changed = conn.execute(
        "UPDATE songs SET deleted_at = NULL, updated_at = ?2
          WHERE id = ?1 AND deleted_at IS NOT NULL",
        params![song_id, crate::db::now_unix_ms()],
    )?;
    Ok(changed)
}

/// 物理删除一行（管理端「彻底清除」专用，**不是**扫描的默认动作）。
///
/// 警告：这会触发外键动作，连带删掉该歌曲的 playlist_items / favorites /
/// play_history —— 用户的歌单条目、收藏、播放历史一起消失，不可恢复。
/// 「磁盘上文件不见了」请一律用 [mark_deleted]。
pub fn purge(conn: &Connection, song_id: i64) -> RepoResult<usize> {
    let removed = conn.execute("DELETE FROM songs WHERE id = ?1", params![song_id])?;
    Ok(removed)
}

/// where_clause 只由本模块的常量传入，参数一律走 params 绑定。
fn query_songs<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> RepoResult<Vec<Song>> {
    let sql = format!("SELECT {COLUMNS} FROM songs {where_clause}");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_song(row)?);
    }
    Ok(out)
}

/// 把一行还原成 Song；scrape_status 的非法取值会变成 RepoError::Model 而不是静默降级。
fn row_to_song(row: &Row<'_>) -> RepoResult<Song> {
    let status_text: String = row.get("scrape_status")?;
    let scrape_status = ScrapeStatus::parse(&status_text)?;
    Ok(Song {
        id: row.get("id")?,
        file_path: row.get("file_path")?,
        album_id: row.get("album_id")?,
        title: row.get("title")?,
        artists: row.get("artists")?,
        album_artist: row.get("album_artist")?,
        year: row.get("year")?,
        genres: row.get("genres")?,
        track: row.get("track")?,
        disc: row.get("disc")?,
        duration_ms: row.get("duration_ms")?,
        bitrate_bps: row.get("bitrate_bps")?,
        format: row.get("format")?,
        audio_hash: row.get("audio_hash")?,
        file_size: row.get("file_size")?,
        file_mtime: row.get("file_mtime")?,
        search_text: row.get("search_text")?,
        lyrics: row.get("lyrics")?,
        scrape_status,
        scrape_error: row.get("scrape_error")?,
        scrape_at: row.get("scrape_at")?,
        deleted_at: row.get("deleted_at")?,
        added_at: row.get("added_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// 转义 LIKE 的通配符，让用户输入里的 % 和 _ 只当普通字符。
fn escape_like(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

// ─────────────────────────────────────────────────────────────────────────────
// S16 · 曲库 API 的只读查询（排序白名单）
//
// 这一节是**新增**的：既有的 insert / get / list / search 等函数保持原样 ——
// 扫描与刮削这些内部流程依赖它们各自的排序与分页语义，不能为了 API 去改。
// ─────────────────────────────────────────────────────────────────────────────

/// 曲库 API 允许的排序字段（白名单）。
///
/// 安全约定：**用户传进来的字符串绝不进入 SQL 文本**。字符串只经过下面的
/// SongSort::parse，命中白名单后转成枚举；真正拼进 ORDER BY 的是
/// SongSort::column 返回的编译期常量。没命中的字段在 handler 里直接 400。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SongSort {
    /// 入库时间
    AddedAt,
    /// 标题
    Title,
    /// 歌手
    Artists,
    /// 专辑艺术家
    AlbumArtist,
    /// 年份
    Year,
    /// 时长（毫秒）
    DurationMs,
    /// 码率（bps）
    BitrateBps,
}

impl SongSort {
    /// 全部允许的取值。parse / allowed_fields / 文档共用这一份，避免漂移。
    const ALL: [SongSort; 7] = [
        SongSort::AddedAt,
        SongSort::Title,
        SongSort::Artists,
        SongSort::AlbumArtist,
        SongSort::Year,
        SongSort::DurationMs,
        SongSort::BitrateBps,
    ];

    /// 该排序字段对应的**固定 SQL 片段**。
    ///
    /// 白名单解析与 SQL 拼接共用同一个常量：允许的字段名与真正进 SQL 的列名
    /// 不可能出现「白名单放行、SQL 里却拼了别的东西」这种不一致。
    fn column(self) -> &'static str {
        match self {
            SongSort::AddedAt => "added_at",
            SongSort::Title => "title",
            SongSort::Artists => "artists",
            SongSort::AlbumArtist => "album_artist",
            SongSort::Year => "year",
            SongSort::DurationMs => "duration_ms",
            SongSort::BitrateBps => "bitrate_bps",
        }
    }

    /// 把外部字符串解析成白名单里的排序字段；不在表里的一律返回 None。
    ///
    /// **这是排序字段唯一的外部字符串入口**，调用方拿到 None 就回 400，
    /// 绝不允许「解析失败就用默认值」—— 那会让注入串静默通过。
    pub fn parse(field: &str) -> Option<SongSort> {
        Self::ALL.into_iter().find(|sort| sort.column() == field)
    }

    /// 允许的字段名（错误信息里列举给调用方，与 parse 同源）。
    pub fn allowed_fields() -> Vec<&'static str> {
        Self::ALL.iter().map(|sort| sort.column()).collect()
    }
}

/// 曲库 API 的过滤条件；字段为 None 表示不按该维度过滤。
///
/// 过滤值一律走 SQL 参数绑定（问号占位符），不做任何字符串拼接。
#[derive(Debug, Clone, Copy, Default)]
pub struct SongFilter<'a> {
    /// 关键词：对 search_text 做子串匹配（与 search 同一口径：
    /// ASCII 大小写不敏感、% 与 _ 按字面量、空白关键词 = 空结果而不是全部）。
    pub keyword: Option<&'a str>,
    /// 歌手：对 songs.artists 做子串匹配。
    ///
    /// 匹配口径是**子串**（不是分隔符切分后的精确相等），原因见 list_page 文档；
    /// % 与 _ 与关键词一样按字面量转义。
    pub artists: Option<&'a str>,
    /// 专辑主键：只取挂在这张专辑下的歌。
    pub album_id: Option<i64>,
}

/// 一页查询结果：当页的行 + 满足过滤条件的总行数。
///
/// total 用与取行**完全相同**的 WHERE 现算（同一个函数里跑 COUNT + SELECT），
/// 所以分页元信息不会和 items 对不上。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongPage {
    /// 当页歌曲
    pub songs: Vec<Song>,
    /// 满足过滤条件的总行数（不受 limit / offset 影响）
    pub total: i64,
}

/// 曲库 API 的分页查询（S16）：过滤 + 排序白名单 + 现算 total。
///
/// 与 list / search 的分工：既有函数服务扫描 / 刮削等内部流程，签名与排序语义
/// 保持原样；本函数是面向 API 的只读入口，额外提供三件事：
///
///   1. **排序白名单**：ORDER BY 的列名只能来自 SongSort，请求里的字节从不进入
///      SQL 文本（防注入，见 SongSort）；
///   2. **组合过滤**：关键词 / 歌手 / 专辑，参数全部走绑定；
///   3. **total 现算**：同一套 WHERE 跑一次 COUNT，供分页元信息使用。
///
/// 软删行为沿用模块约定：include_deleted = false（默认）时软删行不可见。
///
/// ## 歌手匹配口径（子串）
///
/// 画布不建 artists 表，歌手名存在 songs.artists 文本列里，扫描入库时由
/// join_non_empty(&meta.artists, " / ") 拼装（多个歌手用 " / " 分隔），
/// 但插件刮削路径可能写入任意形式的整串。因此这里选择**子串匹配**：
///
///   * 好处：artists = "A / B" 时按 "B" 也能命中，不依赖分隔符写法；
///   * 代价：包含关系会误命中（按 "周杰伦" 查也会命中 "小周杰伦"）。
///
/// 要精确到「分隔符切分后的 token 相等」，得先规定唯一分隔符，而库里并不保证
/// 只有 " / " 一种（刮削插件可直接写整串），所以本步骤不做切分。
///
/// ## 空白过滤值
///
/// 空白关键词 / 空白歌手名一律返回**空结果**（total = 0），与 search 一致；
/// 绝不能退化成 LIKE 两个百分号把整库捞出来。
pub fn list_page(
    conn: &Connection,
    filter: SongFilter<'_>,
    sort: SongSort,
    descending: bool,
    limit: i64,
    offset: i64,
    include_deleted: bool,
) -> RepoResult<SongPage> {
    if filter.keyword.is_some_and(|value| value.trim().is_empty())
        || filter.artists.is_some_and(|value| value.trim().is_empty())
    {
        return Ok(SongPage {
            songs: Vec::new(),
            total: 0,
        });
    }

    // conditions 里只放本函数写死的片段（占位符是问号）；用户值全部进 values。
    let mut conditions: Vec<&'static str> = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if !include_deleted {
        conditions.push("deleted_at IS NULL");
    }
    if let Some(keyword) = filter.keyword {
        conditions.push("search_text LIKE ? ESCAPE '\\'");
        values.push(Box::new(format!("%{}%", escape_like(keyword.trim()))));
    }
    if let Some(artists) = filter.artists {
        conditions.push("artists LIKE ? ESCAPE '\\'");
        values.push(Box::new(format!("%{}%", escape_like(artists.trim()))));
    }
    if let Some(album_id) = filter.album_id {
        conditions.push("album_id = ?");
        values.push(Box::new(album_id));
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    // COUNT 与 SELECT 共用同一套 WHERE / 参数；limit / offset 只在 SELECT 上追加。
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM songs {where_clause}"),
        rusqlite::params_from_iter(values.iter()),
        |row| row.get(0),
    )?;

    // 只有白名单常量与下面二选一的 ASC / DESC 参与拼接；请求里的任何字节都不会进来。
    let direction = if descending { "DESC" } else { "ASC" };
    let sql = format!(
        "SELECT {COLUMNS} FROM songs {where_clause} \
         ORDER BY {} {direction}, id {direction} LIMIT ? OFFSET ?",
        sort.column()
    );

    values.push(Box::new(limit));
    values.push(Box::new(offset));
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(values.iter()))?;
    let mut songs = Vec::new();
    while let Some(row) = rows.next()? {
        songs.push(row_to_song(row)?);
    }
    Ok(SongPage { songs, total })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::test_support::TestDb;
    use crate::db::repos::RepoError;

    fn sample_song(file_path: &str) -> Song {
        Song {
            id: 0,
            file_path: file_path.to_string(),
            album_id: None,
            title: None,
            artists: None,
            album_artist: None,
            year: None,
            genres: None,
            track: None,
            disc: None,
            duration_ms: None,
            bitrate_bps: None,
            format: None,
            audio_hash: None,
            file_size: None,
            file_mtime: None,
            search_text: None,
            lyrics: None,
            scrape_status: ScrapeStatus::Pending,
            scrape_error: None,
            scrape_at: None,
            deleted_at: None,
            added_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn song_crud_round_trip() {
        let db = TestDb::new("songs-crud");
        let conn = db.conn();

        let mut song = sample_song("/music/a.mp3");
        song.title = Some("旧标题".to_string());
        song.search_text = Some("旧标题 某歌手".to_string());
        let id = insert(&conn, &song).expect("插歌");

        let loaded = get(&conn, id, false).expect("按 id 查").expect("应能查到");
        assert_eq!(loaded.file_path, "/music/a.mp3");
        assert_eq!(loaded.title.as_deref(), Some("旧标题"));
        assert_eq!(loaded.scrape_status, ScrapeStatus::Pending);
        assert!(loaded.deleted_at.is_none(), "新歌必须是在库状态");
        assert!(loaded.added_at > 0, "added_at 应由 repo 盖章");
        assert!(loaded.updated_at > 0, "updated_at 应由 repo 盖章");

        let by_path = find_by_file_path(&conn, "/music/a.mp3")
            .expect("按路径查")
            .expect("应能查到");
        assert_eq!(by_path.id, id);
        assert!(find_by_file_path(&conn, "/music/没有.mp3")
            .expect("按路径查")
            .is_none());
        assert_eq!(count(&conn, false).expect("计数"), 1);
        assert_eq!(list(&conn, 10, 0, false).expect("分页列表").len(), 1);

        // 改：标签字段
        let mut edited = loaded.clone();
        edited.title = Some("新标题".to_string());
        edited.artists = Some("某歌手".to_string());
        edited.audio_hash = Some("hash-1".to_string());
        edited.search_text = Some("新标题 某歌手".to_string());
        assert_eq!(update_tags(&conn, &edited).expect("改标签"), 1);
        let reloaded = get(&conn, id, false).expect("重查").expect("应能查到");
        assert_eq!(reloaded.title.as_deref(), Some("新标题"));
        assert_eq!(reloaded.artists.as_deref(), Some("某歌手"));

        // 改：按 audio_hash 能查到
        let by_hash = find_by_audio_hash(&conn, "hash-1", false).expect("按哈希查");
        assert_eq!(by_hash.len(), 1);
        assert_eq!(by_hash[0].id, id);

        // 改：刮削状态
        assert_eq!(
            update_scrape_status(&conn, id, ScrapeStatus::Done, None).expect("改状态"),
            1
        );
        let scraped = get(&conn, id, false).expect("重查").expect("应能查到");
        assert_eq!(scraped.scrape_status, ScrapeStatus::Done);
        assert!(scraped.scrape_at.is_some(), "刮削时间应被盖章");
        assert_eq!(
            update_scrape_status(&conn, id, ScrapeStatus::Failed, Some("全部插件未命中"))
                .expect("改失败状态"),
            1
        );
        let failed = get(&conn, id, false).expect("重查").expect("应能查到");
        assert_eq!(failed.scrape_status, ScrapeStatus::Failed);
        assert_eq!(failed.scrape_error.as_deref(), Some("全部插件未命中"));

        // 删：先软删（默认查询看不到），再物理清除
        assert_eq!(mark_deleted(&conn, id).expect("标记删除"), 1);
        assert!(get(&conn, id, false).expect("默认查").is_none());
        assert!(get(&conn, id, true).expect("含软删查").is_some());
        assert_eq!(count(&conn, false).expect("默认计数"), 0);
        assert_eq!(count(&conn, true).expect("含软删计数"), 1);

        assert_eq!(purge(&conn, id).expect("物理删除"), 1);
        assert!(get(&conn, id, true).expect("重查").is_none());
        assert_eq!(count(&conn, true).expect("计数"), 0);
        assert_eq!(purge(&conn, id).expect("重复 purge 幂等"), 0);
    }

    #[test]
    fn duplicate_file_path_is_reported_as_a_conflict() {
        let db = TestDb::new("songs-dup-path");
        let conn = db.conn();
        insert(&conn, &sample_song("/music/a.mp3")).expect("插第一首");
        let err = insert(&conn, &sample_song("/music/a.mp3")).expect_err("同一路径必须冲突");
        assert!(
            matches!(err, RepoError::Conflict { .. }),
            "应是可区分的 Conflict，实际 {err:?}"
        );
        assert_eq!(count(&conn, true).expect("计数"), 1, "冲突不该写进第二行");
    }

    #[test]
    fn soft_delete_hides_the_song_from_default_queries_but_keeps_the_row() {
        let db = TestDb::new("songs-soft-delete");
        let conn = db.conn();

        let mut song = sample_song("/music/a.mp3");
        song.title = Some("会被标记删除的歌".to_string());
        song.search_text = Some("会被标记删除的歌".to_string());
        let id = insert(&conn, &song).expect("插歌");

        // 标记删除
        assert_eq!(mark_deleted(&conn, id).expect("标记删除"), 1);
        assert!(get(&conn, id, false).expect("默认查").is_none(), "软删后默认查不到");
        let deleted = get(&conn, id, true).expect("含软删查").expect("行还在");
        assert!(deleted.deleted_at.is_some(), "deleted_at 应被盖章");

        // 面向用户的查询默认都看不到
        assert!(list(&conn, 10, 0, false).expect("默认列表").is_empty());
        assert!(search(&conn, "标记删除", 10, 0, false)
            .expect("默认搜索")
            .is_empty());
        assert_eq!(count(&conn, false).expect("默认计数"), 0);

        // 但按路径仍能查到 —— 扫描发现文件还在时要靠它定位到这一行去复原
        let by_path = find_by_file_path(&conn, "/music/a.mp3")
            .expect("按路径查")
            .expect("软删行也必须能按路径查到");
        assert_eq!(by_path.id, id);
        assert!(by_path.deleted_at.is_some());

        // 管理端入口能连软删行一起看
        assert_eq!(list_deleted(&conn, 10, 0).expect("回收站").len(), 1);
        assert_eq!(count(&conn, true).expect("含软删计数"), 1);
        assert_eq!(count_deleted(&conn).expect("软删计数"), 1);
        assert_eq!(list(&conn, 10, 0, true).expect("含软删列表").len(), 1);

        // 幂等：再标一次不改时间戳
        let first_stamp = by_path.deleted_at;
        assert_eq!(
            mark_deleted(&conn, id).expect("重复标记"),
            0,
            "已经是删除态就该是 no-op"
        );
        let still = get(&conn, id, true).expect("查").expect("行还在");
        assert_eq!(still.deleted_at, first_stamp, "重复标记不该刷新时间戳");

        // 文件回来了 -> 复原
        assert_eq!(restore(&conn, id).expect("复原"), 1);
        let restored = get(&conn, id, false).expect("默认查").expect("应能查到");
        assert!(restored.deleted_at.is_none());
        assert_eq!(list(&conn, 10, 0, false).expect("默认列表").len(), 1);
        assert_eq!(count(&conn, false).expect("默认计数"), 1);
        assert!(list_deleted(&conn, 10, 0).expect("回收站").is_empty());
        assert_eq!(
            restore(&conn, id).expect("重复复原"),
            0,
            "在库的行复原是 no-op"
        );

        // 不存在 / 已软删之后被 purge 的行，两个操作都只是 no-op，不该报错
        assert_eq!(mark_deleted(&conn, 999_999).expect("标记不存在的行"), 0);
        assert_eq!(restore(&conn, 999_999).expect("复原不存在的行"), 0);
    }

    #[test]
    fn search_text_matches_partial_and_ascii_case_insensitively() {
        let db = TestDb::new("songs-search");
        let conn = db.conn();

        let mut english = sample_song("/music/1.mp3");
        english.search_text = Some("Hey Jude The Beatles".to_string());
        let mut other = sample_song("/music/2.mp3");
        other.search_text = Some("Bohemian Rhapsody Queen".to_string());
        let mut chinese = sample_song("/music/3.mp3");
        chinese.search_text = Some("中文标题 某歌手".to_string());
        // 没有 search_text 的歌不该被 LIKE 命中
        let blank = sample_song("/music/4.mp3");
        insert(&conn, &english).expect("插英文歌");
        insert(&conn, &other).expect("插第二首");
        insert(&conn, &chinese).expect("插中文歌");
        insert(&conn, &blank).expect("插无 search_text 的歌");

        // 部分匹配
        assert_eq!(search(&conn, "Rhapsody", 10, 0, false).expect("部分匹配").len(), 1);
        assert_eq!(search(&conn, "Jude", 10, 0, false).expect("部分匹配").len(), 1);
        // ASCII 大小写不敏感（SQLite LIKE 的既有行为）
        assert_eq!(search(&conn, "beatles", 10, 0, false).expect("小写搜大写").len(), 1);
        assert_eq!(search(&conn, "BEATLES", 10, 0, false).expect("大写搜大写").len(), 1);
        // 中文按子串精确匹配
        assert_eq!(search(&conn, "中文", 10, 0, false).expect("中文前缀").len(), 1);
        assert_eq!(search(&conn, "标题", 10, 0, false).expect("中文中缀").len(), 1);
        assert_eq!(search(&conn, "中文标题", 10, 0, false).expect("中文整词").len(), 1);
        assert_eq!(search(&conn, "某歌手", 10, 0, false).expect("中文后缀").len(), 1);

        // 搜不到返回空列表，不是错误
        assert!(search(&conn, "根本没有这首歌", 10, 0, false)
            .expect("搜不到不该报错")
            .is_empty());
        // 空关键词不是「搜索全部」
        assert!(search(&conn, "   ", 10, 0, false).expect("空白关键词").is_empty());
        assert!(search(&conn, "", 10, 0, false).expect("空关键词").is_empty());
        // % 和 _ 只当普通字符，不当通配符：搜 % 不该把全部歌捞出来
        assert!(search(&conn, "%", 10, 0, false).expect("百分号是字面量").is_empty());
        assert!(search(&conn, "_", 10, 0, false).expect("下划线是字面量").is_empty());
        // 分页在搜索结果上也生效
        assert_eq!(search(&conn, "e", 2, 0, false).expect("搜索分页").len(), 2);
    }

    #[test]
    fn list_paginates_in_insertion_order() {
        let db = TestDb::new("songs-page");
        let conn = db.conn();
        for index in 0..5 {
            insert(&conn, &sample_song(&format!("/music/{index}.mp3"))).expect("插歌");
        }
        let first = list(&conn, 2, 0, false).expect("第一页");
        let second = list(&conn, 2, 2, false).expect("第二页");
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 2);
        assert!(first[1].id < second[0].id, "分页必须按 id 递增不重不漏");
        assert_eq!(list(&conn, 10, 5, false).expect("越界页").len(), 0);
        assert_eq!(count(&conn, false).expect("计数"), 5);
    }

    #[test]
    fn unknown_scrape_status_in_the_row_is_a_model_error_not_a_silent_default() {
        let db = TestDb::new("songs-bad-status");
        let conn = db.conn();
        // 绕过 CHECK 约束造假，验证 row_to_song 不会把未知枚举值静默降级
        conn.execute_batch("PRAGMA ignore_check_constraints = ON;")
            .expect("临时忽略 CHECK");
        conn.execute(
            "INSERT INTO songs (file_path, scrape_status, added_at, updated_at)
             VALUES ('/music/bad.mp3', 'weird', 1, 1)",
            [],
        )
        .expect("插入非法状态");
        conn.execute_batch("PRAGMA ignore_check_constraints = OFF;")
            .expect("恢复 CHECK");

        let err = find_by_file_path(&conn, "/music/bad.mp3").expect_err("非法枚举值必须报错");
        assert!(
            matches!(err, RepoError::Model(_)),
            "应是 Model 错误而不是静默降级，实际 {err:?}"
        );
    }
}
