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

// ─────────────────────────────────────────────────────────────────────────────
// 播放统计（「历史：最近播放 · 播放统计」的后一半）
//
// 三条口径上的约定，改之前先读：
//
// 1. **只统计调用者自己的行**（`WHERE user_id = ?`）——与 recent / count_by_user 一致。
// 2. `duration_listened_ms` **可能为 NULL**（2026-10-02 起前端才上报）。
//    SUM 会把 NULL 当 0 累加，**不是**把整行丢掉 —— 所以「播放次数」是准的，
//    「累计时长」是「已知的那部分」，会偏小。这不是 bug，是数据的历史。
// 3. 榜单（歌 / 歌手）**跳过已软删的歌**：那些行留着是为了「播放次数」不失真，
//    但一首不在库里的歌放不进榜也没法播（与收藏列表同一口径）。
//    ⇒ 所以：总播放次数 ≥ 榜单里各条之和，两者**本来就不相等**，别去「修」。
// ─────────────────────────────────────────────────────────────────────────────

/// 总量：播放次数 / 累计收听毫秒 / 涉及多少首不同的歌。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayTotals {
    pub plays: i64,
    /// 见上方第 2 条：**可能偏小**（早于上报时长的历史行按 0 计）。
    pub listened_ms: i64,
    /// 不同的 song_id 个数。**含已软删的歌** —— 它回答的是「听过多少首」，
    /// 而不是「库里还剩多少首能播的」。
    pub songs: i64,
}

/// 榜上的一首歌（只有 id 与计数，曲目信息由上层 join）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopSong {
    pub song_id: i64,
    pub plays: i64,
    pub listened_ms: i64,
}

/// 榜上的一位歌手。
///
/// `name` 是 `songs.artists` 的**整串**（多个歌手是 "A / B" 一整串），
/// 与歌手页 `songs::list_artists` 同一口径 —— 那边是按整串分组的，
/// 这里若按 " / " 拆开，统计出来的「歌手」和歌手页会对不上号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopArtist {
    pub name: String,
    pub plays: i64,
    pub listened_ms: i64,
}

/// 某一天的播放量。`day` 是 `YYYY-MM-DD`，**已按调用方给的时区偏移换算**（不是 UTC）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyPlay {
    pub day: String,
    pub plays: i64,
    pub listened_ms: i64,
}

/// 总量。一次查询拿三个数，不拆成三条 SQL（它们来自同一组行，拆开就可能不一致）。
///
/// `since_ms` 只算这个时刻之后的播放；传 **0 = 全部时间**（所有时间戳都 > 0）。
/// ⚠️ 它与 [daily] 用同一个 `since_ms` 时，**`sum(daily.plays) == totals.plays`**
/// —— 这是界面上「总量」与「柱状图」能对上的前提，别只给其中一边加过滤。
pub fn totals(conn: &Connection, user_id: i64, since_ms: i64) -> RepoResult<PlayTotals> {
    let row = conn.query_row(
        "SELECT COUNT(*) AS plays,
                COALESCE(SUM(duration_listened_ms), 0) AS listened_ms,
                COUNT(DISTINCT song_id) AS songs
           FROM play_history WHERE user_id = ?1 AND played_at >= ?2",
        params![user_id, since_ms],
        |row| {
            Ok(PlayTotals {
                plays: row.get("plays")?,
                listened_ms: row.get("listened_ms")?,
                songs: row.get("songs")?,
            })
        },
    )?;
    Ok(row)
}

/// 播放次数最多的歌。次数相同时按 song_id 升序兜底 —— 顺序必须确定，
/// 否则两次请求可能给出不同的榜单。
///
/// ⚠️ 这一条**不 join songs**（所以软删的歌也在榜里，只是上层拿不到曲目信息）；
/// 歌手榜则必须 join，因此那边会滤掉软删 —— 两者口径不同是故意的，见文件头的注释。
pub fn top_songs(
    conn: &Connection,
    user_id: i64,
    since_ms: i64,
    limit: i64,
) -> RepoResult<Vec<TopSong>> {
    let mut stmt = conn.prepare(
        "SELECT song_id,
                COUNT(*) AS plays,
                COALESCE(SUM(duration_listened_ms), 0) AS listened_ms
           FROM play_history
          WHERE user_id = ?1 AND played_at >= ?2
          GROUP BY song_id
          ORDER BY plays DESC, song_id ASC
          LIMIT ?3",
    )?;
    let mut rows = stmt.query(params![user_id, since_ms, limit])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(TopSong {
            song_id: row.get("song_id")?,
            plays: row.get("plays")?,
            listened_ms: row.get("listened_ms")?,
        });
    }
    Ok(out)
}

/// 播放次数最多的歌手（`songs.artists` 整串分组，跳过软删的歌与空歌手）。
pub fn top_artists(
    conn: &Connection,
    user_id: i64,
    since_ms: i64,
    limit: i64,
) -> RepoResult<Vec<TopArtist>> {
    let mut stmt = conn.prepare(
        "SELECT s.artists AS name,
                COUNT(*) AS plays,
                COALESCE(SUM(h.duration_listened_ms), 0) AS listened_ms
           FROM play_history h
           JOIN songs s ON s.id = h.song_id
          WHERE h.user_id = ?1
            AND h.played_at >= ?2
            AND s.deleted_at IS NULL
            AND s.artists IS NOT NULL
            AND TRIM(s.artists) <> ''
          GROUP BY s.artists
          ORDER BY plays DESC, s.artists ASC
          LIMIT ?3",
    )?;
    let mut rows = stmt.query(params![user_id, since_ms, limit])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(TopArtist {
            name: row.get("name")?,
            plays: row.get("plays")?,
            listened_ms: row.get("listened_ms")?,
        });
    }
    Ok(out)
}

/// 按天聚合（`played_at >= since_ms`，最新的天在前）。
///
/// `tz_offset_minutes` 与 UTC 的偏移分钟数（北京 = **480**）。**必须由调用方给**：
/// SQLite 的 `date()` 只会按 UTC 切天，直接用 UTC 的话「凌晨 0:30 听的那一首」
/// 会被算进前一天 —— 统计页上那是显而易见的错。
///
/// 参数以绑定方式传（SQLite 允许修饰符是绑定值），拼字符串只是把整数变成
/// `"+480 minutes"` 这种形状，不经过任何外部文本。
pub fn daily(
    conn: &Connection,
    user_id: i64,
    since_ms: i64,
    tz_offset_minutes: i64,
) -> RepoResult<Vec<DailyPlay>> {
    let modifier = format!(
        "{}{} minutes",
        if tz_offset_minutes < 0 { "-" } else { "+" },
        tz_offset_minutes.abs()
    );
    let mut stmt = conn.prepare(
        "SELECT date(played_at / 1000, 'unixepoch', ?2) AS day,
                COUNT(*) AS plays,
                COALESCE(SUM(duration_listened_ms), 0) AS listened_ms
           FROM play_history
          WHERE user_id = ?1 AND played_at >= ?3
          GROUP BY day
          ORDER BY day DESC",
    )?;
    let mut rows = stmt.query(params![user_id, modifier, since_ms])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(DailyPlay {
            day: row.get("day")?,
            plays: row.get("plays")?,
            listened_ms: row.get("listened_ms")?,
        });
    }
    Ok(out)
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

    /// 统计：总量 / 榜单 / 按天。**全部只看自己的行**。
    #[test]
    fn stats_aggregate_only_the_callers_rows() {
        let db = TestDb::new("history-stats");
        let conn = db.conn();
        let alice = seed_user(&conn, "alice");
        let bob = seed_user(&conn, "bob");
        let a1 = seed_song(&conn, "/music/a1.mp3", None);
        let a2 = seed_song(&conn, "/music/a2.mp3", None);
        // 歌手整串（多个歌手用 " / " 拼，见 songs::list_artists 的注释）
        conn.execute("UPDATE songs SET artists = '周杰伦' WHERE id = ?1", params![a1])
            .expect("设歌手");
        conn.execute("UPDATE songs SET artists = '周杰伦' WHERE id = ?1", params![a2])
            .expect("设歌手");

        // alice：a1 听两次（10s / 20s）、a2 听一次（没有时长）
        record(&conn, alice, a1, Some(10_000)).expect("记");
        record(&conn, alice, a1, Some(20_000)).expect("记");
        record(&conn, alice, a2, None).expect("记");
        // bob 的行必须完全不出现在 alice 的统计里
        record(&conn, bob, a2, Some(999_999)).expect("记");

        let t = totals(&conn, alice, 0).expect("总量");
        assert_eq!(t.plays, 3, "三次播放");
        assert_eq!(
            t.listened_ms, 30_000,
            "NULL 时长当 0 累加，不是把整行丢掉"
        );
        assert_eq!(t.songs, 2, "两首不同的歌");

        let top = top_songs(&conn, alice, 0, 10).expect("歌榜");
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].song_id, a1);
        assert_eq!(top[0].plays, 2);
        assert_eq!(top[0].listened_ms, 30_000);
        assert_eq!(top[1].song_id, a2);
        assert_eq!(top[1].plays, 1);
        assert_eq!(top[1].listened_ms, 0, "该行时长为 NULL → 0");

        // 次数相同时按 song_id 升序兜底（顺序必须确定）
        record(&conn, alice, a2, None).expect("把 a2 也凑成两次");
        let tied = top_songs(&conn, alice, 0, 10).expect("并列");
        assert_eq!(tied[0].song_id, a1);
        assert_eq!(tied[1].song_id, a2);
        // limit 生效
        assert_eq!(top_songs(&conn, alice, 0, 1).expect("限量").len(), 1);

        let artists = top_artists(&conn, alice, 0, 10).expect("歌手榜");
        assert_eq!(artists.len(), 1, "两首歌同一位歌手 → 合并成一行");
        assert_eq!(artists[0].name, "周杰伦");
        assert_eq!(artists[0].plays, 4);
        assert_eq!(artists[0].listened_ms, 30_000);

        // bob 自己的统计（999_999 是他的，不该串到 alice 那边）
        assert_eq!(totals(&conn, bob, 0).expect("bob 总量").plays, 1);
        assert_eq!(totals(&conn, bob, 0).expect("bob 总量").listened_ms, 999_999);
    }

    /// 软删的歌：**总量照算**（历史是既成事实），但**不进榜单**（放不了、也不在库里）。
    #[test]
    fn stats_count_soft_deleted_songs_in_totals_but_not_in_rankings() {
        let db = TestDb::new("history-stats-softdel");
        let conn = db.conn();
        let alice = seed_user(&conn, "alice");
        let gone = seed_song(&conn, "/music/gone.mp3", None);
        let alive = seed_song(&conn, "/music/alive.mp3", None);
        conn.execute("UPDATE songs SET artists = '在库歌手' WHERE id = ?1", params![alive])
            .expect("设歌手");
        conn.execute("UPDATE songs SET artists = '已删歌手' WHERE id = ?1", params![gone])
            .expect("设歌手");

        record(&conn, alice, gone, Some(5_000)).expect("记");
        record(&conn, alice, alive, Some(7_000)).expect("记");
        // 软删：songs 行还在（外键也还指向它），只是 deleted_at 非空
        conn.execute(
            "UPDATE songs SET deleted_at = ?1 WHERE id = ?2",
            params![crate::db::now_unix_ms(), gone],
        )
        .expect("软删");

        let t = totals(&conn, alice, 0).expect("总量");
        assert_eq!(t.plays, 2, "软删的歌也算播放次数");
        assert_eq!(t.songs, 2, "「听过几首」不受软删影响");

        let top = top_songs(&conn, alice, 0, 10).expect("歌榜");
        assert_eq!(top.len(), 2, "歌曲榜按 song_id 聚合，软删与否都在——由上层决定是否展示");

        let artists = top_artists(&conn, alice, 0, 10).expect("歌手榜");
        assert_eq!(artists.len(), 1, "软删那首的歌手不该出现在榜上");
        assert_eq!(artists[0].name, "在库歌手");
        assert_eq!(artists[0].plays, 1);
    }

    /// 按天聚合 + **时区切天**：这是这一组里唯一容易悄悄错的地方。
    #[test]
    fn daily_splits_days_by_the_given_timezone_not_utc() {
        let db = TestDb::new("history-daily");
        let conn = db.conn();
        let alice = seed_user(&conn, "alice");
        let song = seed_song(&conn, "/music/a.mp3", None);

        // 2026-10-01 16:30 UTC → 北京（UTC+8）是 2026-10-02 00:30
        let ts = 1_790_872_200_000_i64;
        for offset in [0_i64, 1] {
            conn.execute(
                "INSERT INTO play_history (user_id, song_id, played_at, duration_listened_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![alice, song, ts + offset, 60_000],
            )
            .expect("插历史");
        }

        // UTC 视角：两条都在 10-01
        let utc = daily(&conn, alice, 0, 0).expect("UTC 天");
        assert_eq!(utc.len(), 1);
        assert_eq!(utc[0].day, "2026-10-01");
        assert_eq!(utc[0].plays, 2);
        assert_eq!(utc[0].listened_ms, 120_000);

        // 北京视角：两条都在 10-02（**不是 10-01** —— 差这一天就是这里的意义）
        let bj = daily(&conn, alice, 0, 480).expect("北京 天");
        assert_eq!(bj.len(), 1);
        assert_eq!(bj[0].day, "2026-10-02");
        assert_eq!(bj[0].plays, 2);

        // 西五区：2026-10-01 11:30 → 还是 10-01
        let ny = daily(&conn, alice, 0, -300).expect("西五区天");
        assert_eq!(ny[0].day, "2026-10-01");

        // since_ms 真的在过滤
        let none = daily(&conn, alice, ts + 1_000, 480).expect("更晚的起点");
        assert!(none.is_empty(), "起点晚于所有播放 → 空");

        // 别人的天不进我的
        let bob = seed_user(&conn, "bob");
        assert!(daily(&conn, bob, 0, 480).expect("bob").is_empty());
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
