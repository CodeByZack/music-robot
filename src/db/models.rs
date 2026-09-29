//! S2 · 表模型 —— 画布 ⑨ 的 10 张表对应的 Rust 结构体。
//!
//! 这里只有**数据结构**，没有任何 SQL：读写全部归 S3 的 repos。
//! 字段与列一一对应，类型选择遵循几条约定：
//!
//!   * 可空列 -> Option<T>；NOT NULL 列不用 Option，避免把「空串 / 0」当成空值；
//!   * 时间列 -> i64（Unix 毫秒，见 crate::db::now_unix_ms）；
//!   * 布尔列（is_public）-> bool，由 repo 层负责 0 / 1 互转；
//!   * SQLite 的整数一律 i64，包括 file_size 这类看起来像 u64 的量 ——
//!     SQLite 的 INTEGER 就是 i64，在模型里假装成 u64 只会在边界上多出一次转换。
//!
//! 不引 serde derive（项目规范）：需要 JSON 的地方手写 serde_json::Value。
//!
//! 枚举值（role / scrape_status / status）一律用枚举 + as_str / parse 互转，
//! 不允许裸字符串散落在各模块里。

/// 用户角色，对应 users.role。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 管理员
    Admin,
    /// 普通用户
    User,
}

impl Role {
    /// 写进数据库的字符串
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }

    /// 从数据库读出的字符串还原；未知取值报错，不静默降级成 User
    pub fn parse(value: &str) -> Result<Role, ModelError> {
        match value {
            "admin" => Ok(Role::Admin),
            "user" => Ok(Role::User),
            other => Err(ModelError::UnknownValue {
                field: "role",
                value: other.to_string(),
                allowed: "admin | user",
            }),
        }
    }
}

/// 刮削状态，对应 songs.scrape_status。
///
/// 语义：pending = 待刮；processing = 已入槽正在刮；done = 已命中并落库；
/// failed = 全部插件都没命中，等人工重刮（画布原文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrapeStatus {
    /// 待刮削
    Pending,
    /// 正在刮削
    Processing,
    /// 刮削完成
    Done,
    /// 全部插件未命中
    Failed,
}

impl ScrapeStatus {
    /// 写进数据库的字符串
    pub fn as_str(self) -> &'static str {
        match self {
            ScrapeStatus::Pending => "pending",
            ScrapeStatus::Processing => "processing",
            ScrapeStatus::Done => "done",
            ScrapeStatus::Failed => "failed",
        }
    }

    /// 从数据库读出的字符串还原
    pub fn parse(value: &str) -> Result<ScrapeStatus, ModelError> {
        match value {
            "pending" => Ok(ScrapeStatus::Pending),
            "processing" => Ok(ScrapeStatus::Processing),
            "done" => Ok(ScrapeStatus::Done),
            "failed" => Ok(ScrapeStatus::Failed),
            other => Err(ModelError::UnknownValue {
                field: "scrape_status",
                value: other.to_string(),
                allowed: "pending | processing | done | failed",
            }),
        }
    }
}

/// 点歌请求状态，对应 song_requests.status。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    /// 待处理
    Pending,
    /// 处理中
    Processing,
    /// 已完成（关联到 song_id）
    Done,
    /// 已拒绝（reject_reason 说明原因）
    Rejected,
}

impl RequestStatus {
    /// 写进数据库的字符串
    pub fn as_str(self) -> &'static str {
        match self {
            RequestStatus::Pending => "pending",
            RequestStatus::Processing => "processing",
            RequestStatus::Done => "done",
            RequestStatus::Rejected => "rejected",
        }
    }

    /// 从数据库读出的字符串还原
    pub fn parse(value: &str) -> Result<RequestStatus, ModelError> {
        match value {
            "pending" => Ok(RequestStatus::Pending),
            "processing" => Ok(RequestStatus::Processing),
            "done" => Ok(RequestStatus::Done),
            "rejected" => Ok(RequestStatus::Rejected),
            other => Err(ModelError::UnknownValue {
                field: "status",
                value: other.to_string(),
                allowed: "pending | processing | done | rejected",
            }),
        }
    }
}

/// 模型层的枚举转换错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// 枚举列出现了数据库约束之外的取值（手改库、或被别的程序写入）
    UnknownValue {
        /// 列 / 字段名
        field: &'static str,
        /// 实际读到的值
        value: String,
        /// 允许的取值，写进错误消息方便定位
        allowed: &'static str,
    },
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::UnknownValue {
                field,
                value,
                allowed,
            } => write!(f, "字段 {field} 出现未知取值 {value:?}（允许：{allowed}）"),
        }
    }
}

impl std::error::Error for ModelError {}

/// 用户，对应 users 表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// 主键
    pub id: i64,
    /// 登录名（UNIQUE）
    pub username: String,
    /// 口令哈希（绝不放明文）
    pub password_hash: String,
    /// 角色
    pub role: Role,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 最后登录时间（Unix 毫秒），从未登录为 None
    pub last_login: Option<i64>,
}

/// 专辑，对应 albums 表。
///
/// 不存 song_count：曲目数是派生字段，查询时 GROUP BY 现算（画布明确）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    /// 主键
    pub id: i64,
    /// 专辑名
    pub name: String,
    /// 专辑艺术家；未知统一为空串（保证 UNIQUE(name, album_artist) 生效）
    pub album_artist: String,
    /// 发行年份
    pub year: Option<i64>,
    /// 封面原始字节
    pub cover_data: Option<Vec<u8>>,
    /// 封面 MIME
    pub cover_mime: Option<String>,
    /// 更新时间（Unix 毫秒）
    pub updated_at: i64,
}

/// 歌曲，对应 songs 表（核心表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    /// 主键
    pub id: i64,
    /// 文件路径（UNIQUE）
    pub file_path: String,
    /// 所属专辑；专辑被删了置空，不连带删歌
    pub album_id: Option<i64>,
    /// 标题
    pub title: Option<String>,
    /// 歌手（可多个，画布不建 artists 表，就存这里）
    pub artists: Option<String>,
    /// 专辑艺术家
    pub album_artist: Option<String>,
    /// 年份
    pub year: Option<i64>,
    /// 流派（可多个）
    pub genres: Option<String>,
    /// 音轨号
    pub track: Option<i64>,
    /// 碟号
    pub disc: Option<i64>,
    /// 时长（毫秒）
    pub duration_ms: Option<i64>,
    /// 码率（bps）
    pub bitrate_bps: Option<i64>,
    /// 容器 / 编码格式
    pub format: Option<String>,
    /// 裸音频哈希（与标签无关，用于判重与转码缓存共享）
    pub audio_hash: Option<String>,
    /// 文件大小（字节）
    pub file_size: Option<i64>,
    /// 文件 mtime（Unix 毫秒）
    pub file_mtime: Option<i64>,
    /// 搜索用拼装文本（派生字段的例外：它是查询加速列，不是业务派生）
    pub search_text: Option<String>,
    /// 歌词全文
    pub lyrics: Option<String>,
    /// 刮削状态
    pub scrape_status: ScrapeStatus,
    /// 最近一次刮削错误
    pub scrape_error: Option<String>,
    /// 最近一次刮削时间（Unix 毫秒）
    pub scrape_at: Option<i64>,
    /// 入库时间（Unix 毫秒）
    pub added_at: i64,
    /// 更新时间（Unix 毫秒）
    pub updated_at: i64,
    /// 软删除标记：`None` = 在库；`Some(ts)` = 该时刻发现磁盘上已消失。
    ///
    /// 为什么不真删：playlist_items / favorites / play_history 都指向本表，
    /// 真删会把用户的歌单、收藏、播放历史一并 CASCADE 掉。文件回来时把本列
    /// 置回 NULL 即可复原。**所有面向用户的查询都要带 `deleted_at IS NULL`。**
    pub deleted_at: Option<i64>,
}

/// 歌单，对应 playlists 表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    /// 主键
    pub id: i64,
    /// 属主
    pub user_id: i64,
    /// 歌单名
    pub name: String,
    /// 描述
    pub description: Option<String>,
    /// 是否公开（0 / 1 由 repo 层映射）
    pub is_public: bool,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 更新时间（Unix 毫秒）
    pub updated_at: i64,
}

/// 歌单条目，对应 playlist_items 表。
///
/// 复合主键 (playlist_id, song_id)：同一首歌在同一个歌单里只出现一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistItem {
    /// 所属歌单
    pub playlist_id: i64,
    /// 歌曲
    pub song_id: i64,
    /// 排序位置
    pub position: i64,
    /// 加入时间（Unix 毫秒）
    pub added_at: i64,
}

/// 收藏，对应 favorites 表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Favorite {
    /// 主键
    pub id: i64,
    /// 用户
    pub user_id: i64,
    /// 歌曲
    pub song_id: i64,
    /// 收藏时间（Unix 毫秒）
    pub created_at: i64,
}

/// 播放历史，对应 play_history 表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayHistory {
    /// 主键
    pub id: i64,
    /// 用户
    pub user_id: i64,
    /// 歌曲
    pub song_id: i64,
    /// 播放时间（Unix 毫秒）
    pub played_at: i64,
    /// 实际听了多久（毫秒），用于统计
    pub duration_listened_ms: Option<i64>,
}

/// 用户设置，对应 user_settings 表。
///
/// 复合主键 (user_id, key)。断点续播 position_ms、音量、播放模式都放这里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSetting {
    /// 用户
    pub user_id: i64,
    /// 设置项
    pub key: String,
    /// 设置值
    pub value: Option<String>,
}

/// 点歌请求，对应 song_requests 表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongRequest {
    /// 主键
    pub id: i64,
    /// 首个发起人
    pub user_id: i64,
    /// 想点的标题
    pub title: String,
    /// 歌手
    pub artist: Option<String>,
    /// 专辑
    pub album: Option<String>,
    /// 备注
    pub note: Option<String>,
    /// 去重键 = normalize(title) + normalize(artist)，库里有唯一索引
    pub dedup_key: String,
    /// 处理状态
    pub status: RequestStatus,
    /// 拒绝原因（status = rejected 时）
    pub reject_reason: Option<String>,
    /// 完成后关联到的歌曲
    pub song_id: Option<i64>,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
    /// 更新时间（Unix 毫秒）
    pub updated_at: i64,
}

/// 点歌投票，对应 request_votes 表。
///
/// 复合主键 (request_id, user_id)：同一用户对同一请求只能投一票。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestVote {
    /// 请求
    pub request_id: i64,
    /// 投票用户
    pub user_id: i64,
    /// 投票时间（Unix 毫秒）
    pub created_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_values_round_trip_through_the_database_strings() {
        for role in [Role::Admin, Role::User] {
            assert_eq!(Role::parse(role.as_str()).expect("role 回环"), role);
        }
        for status in [
            ScrapeStatus::Pending,
            ScrapeStatus::Processing,
            ScrapeStatus::Done,
            ScrapeStatus::Failed,
        ] {
            assert_eq!(
                ScrapeStatus::parse(status.as_str()).expect("scrape_status 回环"),
                status
            );
        }
        for status in [
            RequestStatus::Pending,
            RequestStatus::Processing,
            RequestStatus::Done,
            RequestStatus::Rejected,
        ] {
            assert_eq!(
                RequestStatus::parse(status.as_str()).expect("status 回环"),
                status
            );
        }
    }

    #[test]
    fn enum_strings_match_the_schema_check_constraints() {
        // 这些字面量必须与 migrations.rs 里的 CHECK (...) 完全一致
        assert_eq!(Role::Admin.as_str(), "admin");
        assert_eq!(Role::User.as_str(), "user");
        assert_eq!(ScrapeStatus::Pending.as_str(), "pending");
        assert_eq!(ScrapeStatus::Failed.as_str(), "failed");
        assert_eq!(RequestStatus::Pending.as_str(), "pending");
        assert_eq!(RequestStatus::Rejected.as_str(), "rejected");
    }

    #[test]
    fn unknown_enum_values_are_errors_with_chinese_messages() {
        let role = Role::parse("root").expect_err("未知角色必须报错");
        assert!(matches!(
            role,
            ModelError::UnknownValue { field: "role", .. }
        ));
        let text = role.to_string();
        assert!(
            text.contains("role") && text.contains("root"),
            "错误消息要带字段名与取值：{text}"
        );
        assert!(ScrapeStatus::parse("weird").is_err());
        assert!(RequestStatus::parse("weird").is_err());
        let _boxed: Box<dyn std::error::Error> = Box::new(ModelError::UnknownValue {
            field: "status",
            value: "x".to_string(),
            allowed: "pending | processing | done | rejected",
        });
    }

    #[test]
    fn every_canvas_table_has_a_struct_with_the_expected_shape() {
        let user = User {
            id: 1,
            username: "u".to_string(),
            password_hash: "h".to_string(),
            role: Role::Admin,
            created_at: 0,
            last_login: None,
        };
        let album = Album {
            id: 1,
            name: "专辑".to_string(),
            album_artist: "歌手".to_string(),
            year: Some(2024),
            cover_data: Some(vec![1, 2, 3]),
            cover_mime: Some("image/jpeg".to_string()),
            updated_at: 0,
        };
        let song = Song {
            id: 1,
            file_path: "/music/a.mp3".to_string(),
            album_id: Some(album.id),
            title: Some("标题".to_string()),
            artists: Some("歌手".to_string()),
            album_artist: Some("歌手".to_string()),
            year: Some(2024),
            genres: Some("流行".to_string()),
            track: Some(1),
            disc: Some(1),
            duration_ms: Some(180_000),
            bitrate_bps: Some(320_000),
            format: Some("mp3".to_string()),
            audio_hash: Some("deadbeef".to_string()),
            file_size: Some(7_200_000),
            file_mtime: Some(1_700_000_000_000),
            search_text: Some("标题 歌手".to_string()),
            lyrics: None,
            scrape_status: ScrapeStatus::Pending,
            scrape_error: None,
            scrape_at: None,
            added_at: 0,
            updated_at: 0,
            deleted_at: None,
        };
        let playlist = Playlist {
            id: 1,
            user_id: user.id,
            name: "列表".to_string(),
            description: None,
            is_public: true,
            created_at: 0,
            updated_at: 0,
        };
        let item = PlaylistItem {
            playlist_id: playlist.id,
            song_id: song.id,
            position: 1,
            added_at: 0,
        };
        let favorite = Favorite {
            id: 1,
            user_id: user.id,
            song_id: song.id,
            created_at: 0,
        };
        let history = PlayHistory {
            id: 1,
            user_id: user.id,
            song_id: song.id,
            played_at: 0,
            duration_listened_ms: Some(30_000),
        };
        let setting = UserSetting {
            user_id: user.id,
            key: "position_ms".to_string(),
            value: Some("12345".to_string()),
        };
        let request = SongRequest {
            id: 1,
            user_id: user.id,
            title: "想听的歌".to_string(),
            artist: Some("某人".to_string()),
            album: None,
            note: None,
            dedup_key: "想听的歌|某人".to_string(),
            status: RequestStatus::Pending,
            reject_reason: None,
            song_id: None,
            created_at: 0,
            updated_at: 0,
        };
        let vote = RequestVote {
            request_id: request.id,
            user_id: user.id,
            created_at: 0,
        };

        assert_eq!(user.role, Role::Admin);
        assert_eq!(album.cover_data.as_deref(), Some(&[1u8, 2, 3][..]));
        assert!(song.album_id.is_some() && song.lyrics.is_none());
        assert!(playlist.is_public);
        assert_eq!(item.position, 1);
        assert_eq!(favorite.song_id, song.id);
        assert_eq!(history.duration_listened_ms, Some(30_000));
        assert_eq!(setting.key, "position_ms");
        assert_eq!(request.status, RequestStatus::Pending);
        assert_eq!(vote.request_id, request.id);
    }
}
