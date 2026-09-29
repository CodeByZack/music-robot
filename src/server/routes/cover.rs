//! S20 · 封面：GET /api/songs/{id}/cover。
//!
//! 前端只要有歌曲 id 就能拿到封面图，不必知道服务器上的绝对路径（曲库接口刻意
//! 不暴露 file_path）。
//!
//! # 数据来源：先专辑表，空了回退到文件内嵌封面
//!
//! | 顺序 | 来源 | 条件 |
//! |------|------|------|
//! | 1 | albums.cover_data / cover_mime（刮削写进去的） | 歌曲有 album_id 且该列非空 |
//! | 2 | 音频文件内嵌封面（read_tags 的 pictures） | 第 1 步没命中 |
//!
//! 第 2 步命中后会把字节与 MIME **顺手回填**进 albums 表，下次同专辑的歌直接走第 1 步。
//! 为什么必须有第 2 步（今天 albums.cover_data 恒为空）写在 crate::audio::cover 的
//! 模块文档里，动手删之前先读那一段。
//!
//! # 状态码
//!
//! | 情况 | 响应 |
//! |------|------|
//! | 命中封面 | 200 + 图片字节 + 真实 Content-Type + Cache-Control |
//! | 歌曲不存在 / 已软删 | 404（统一错误形状） |
//! | 歌曲存在但没有任何封面（文件里也没有） | 404（画布 UT 明确要求，不是 204 也不是空图） |
//! | 封面超过上限（[crate::audio::cover::MAX_COVER_BYTES]） | 413 + COVER_TOO_LARGE（明确拒绝，不截断） |
//! | 未登录 | 401（受保护子 Router 的 require_auth + handler 里的 AuthUser） |
//! | id 不是整数 | 400 |
//!
//! # 内存特性（如实写明）
//!
//! 封面整段读进内存再写进响应体（图片要 sniff MIME、要回填，本来就得整段拿到）。
//! 上限由 crate::audio::cover 统一把关：文件内嵌封面在提取后检查，专辑表里的封面在
//! 组装响应前检查。**已知残余**：从 albums 表读时走的是仓库层的 read_cover，它一次
//! 把整个 BLOB 读出来，检查发生在读之后 —— 正常情况下库里的封面都是本接口回填的、
//! 必然 <= 上限；只有被外部直接写坏的库才可能触发。要彻底避免得给 repo 加一个只查
//! LENGTH(cover_data) 的接口，那要动 src/db，超出本步范围。
//!
//! # 路径安全
//!
//! file_path 虽然来自数据库，仍一律经 FileStorage / PathSandbox 解析（StoragePath::abs
//! + stat 拿到已 canonicalize 的路径），绝不拿它直接 std::fs::read：数据库被投毒或有
//! 历史脏数据时，越界的路径必须被沙箱挡住，而不是变成任意文件读取。
//!
//! # 阻塞调用
//!
//! 读盘与解标签都是同步阻塞的（S14 铁律），整段包在 tokio::task::spawn_blocking 里；
//! 查库走 run_db（它自己就是 spawn_blocking）。

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::audio::cover::{self, CoverError, CoverImage};
use crate::config::Config;
use crate::db::repos::{albums, songs};
use crate::server::auth::AuthUser;
use crate::server::error::{ApiError, ApiResult};
use crate::server::state::AppState;
use crate::storage::{FileStorage, StorageBackend, StorageError, StoragePath};
use crate::tag::read::{read_tags, ReadError};

use super::library::{parse_id, run_db};

/// 封面的缓存策略。
///
/// 封面在同一个 URL 下内容基本不变（回填之后同专辑的歌拿到的是同一张图），但它**不是
/// 绝对不变** —— 重新打标签 / 重新刮削都会换掉它。所以给一天的长缓存：列表滚动时不
/// 反复回源，换图之后客户端也不会长时间看不到新封面。
///
/// 用 private 而不是 public：这是受鉴权保护的接口，响应不该被共享缓存跨用户复用。
const CACHE_CONTROL: &str = "private, max-age=86400";

/// GET /api/songs/{id}/cover —— 返回这首歌的封面图片。
///
/// handler 只负责把错误收敛成统一形状，真正的流程在 [cover_inner]。
pub async fn cover(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> Response {
    match cover_inner(state, raw_id).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 取歌曲 → 第 1 步查专辑表 → 第 2 步回退到文件内嵌封面 → 回填。
///
/// 拆出来是为了让错误能沿着 [ApiError] 用问号冒泡；成功响应在 [cover_response] 组装。
async fn cover_inner(state: AppState, raw_id: String) -> ApiResult<Response> {
    let id = parse_id(&raw_id)?;

    // 软删的歌视同不存在（include_deleted = false）→ 下面 404。
    let found = run_db(Arc::clone(&state.db), move |conn| {
        Ok(songs::get(conn, id, false)?)
    })
    .await?;
    let Some(song) = found else {
        return Err(ApiError::not_found("请求的歌曲不存在"));
    };

    // 歌曲可能没挂专辑（单曲 / 扫描时没匹配上），那就直接走第 2 步。
    let album_id = song.album_id;

    // ── 第 1 步：专辑表里现成的封面（刮削写过就有）────────────────────────
    if let Some(album_id) = album_id {
        let stored = run_db(Arc::clone(&state.db), move |conn| {
            Ok(albums::read_cover(conn, album_id)?)
        })
        .await?;
        if let Some(stored) = stored {
            let image =
                cover::from_stored(stored.data, stored.mime.as_deref()).map_err(map_cover_error)?;
            return cover_response(image);
        }
    }

    // ── 第 2 步：回退到文件内嵌封面（为什么必须有这一步见 crate::audio::cover）──
    let config = Arc::clone(&state.config);
    let file_path = song.file_path;
    let extracted = tokio::task::spawn_blocking(move || extract_from_file(&config, &file_path))
        .await
        .map_err(|join| ApiError::internal(format!("封面提取任务异常退出：{join}")))??;
    let Some(image) = extracted else {
        return Err(ApiError::not_found("请求的封面不存在"));
    };

    // 顺手回填：下次同专辑直接命中第 1 步。回填失败只记日志，**绝不影响本次响应**
    // —— 封面已经拿到手了，回填只是优化，不能让一次写库失败把 200 变成 500。
    if let Some(album_id) = album_id {
        let data = image.data.clone();
        let mime = image.mime.clone();
        let backfilled: ApiResult<()> = run_db(Arc::clone(&state.db), move |conn| {
            albums::update_cover(conn, album_id, Some(&data), Some(&mime))?;
            Ok(())
        })
        .await;
        if let Err(error) = backfilled {
            eprintln!("[server] 封面回填专辑 {album_id} 失败（不影响本次响应）：{error:?}");
        }
    }

    cover_response(image)
}

/// 同步的「从音频文件里取封面」：沙箱解析路径 → read_tags → 挑图 → 上限检查 → 定 MIME。
///
/// 只接收已构造好的配置，整个函数在调用方的 spawn_blocking 里跑。
/// 没有可用封面返回 Ok(None)（404）；存储 / 标签 IO 故障按 [map_storage_error] /
/// [map_read_error] 收敛。
fn extract_from_file(config: &Config, file_path: &str) -> ApiResult<Option<CoverImage>> {
    // 每次请求从配置构造一次 FileStorage：它内部对每个库根做一次 canonicalize
    // （fail-closed，见 storage.rs）。封面是低频请求，这点开销可以忽略，换来的是
    // 不必给 AppState 加状态、也不会缓存住一份过期的库根快照。
    let storage = FileStorage::from_config(&config.storage).map_err(map_storage_error)?;
    let path = StoragePath::abs(file_path);
    let stat = storage.stat(&path).map_err(map_storage_error)?;
    if !stat.is_file {
        return Err(ApiError::not_found("请求的音频文件不存在"));
    }

    // stat.path 已经过沙箱 resolve（symlink 展开 + 落库根内校验）。read_tags 内部虽然
    // 用 std::fs::read，但它读的是这个已验证的规范路径，不可能再逃逸。
    let metadata = read_tags(&stat.path).map_err(map_read_error)?;
    cover::extract(&metadata.pictures).map_err(map_cover_error)
}

/// 组装 200 成功响应：图片字节 + 真实 MIME + Content-Length + 缓存策略。
fn cover_response(image: CoverImage) -> ApiResult<Response> {
    let length = image.data.len();
    let content_type = HeaderValue::from_str(&image.mime)
        .map_err(|e| ApiError::internal(format!("构造封面 Content-Type 失败：{e}")))?;

    let mut response = Response::new(Body::from(image.data));
    *response.status_mut() = StatusCode::OK;

    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, content_type);
    // 显式给出 Content-Length：与 body 字节数严格一致。
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL));
    Ok(response)
}

/// 存储层错误 → API 错误。
///
/// * 文件不在 / 解析不到库根内 / 不是普通文件 → **404**：对客户端都是「这首曲子取不到」。
///   刻意不区分「越界」与「不存在」—— 区分开等于把部署目录结构透给调用方。
/// * 配置故障（库根不存在 / 为空）与 IO 故障 → 500，完整原因只进日志。
///
/// 封面不做区间读取，RangeOutOfBounds 正常不会出现；真出现就按内部故障兜底。
fn map_storage_error(error: StorageError) -> ApiError {
    match error {
        StorageError::NotFound { .. }
        | StorageError::NotAFile { .. }
        | StorageError::NotADirectory { .. }
        | StorageError::Escape { .. }
        | StorageError::OutsideAnyRoot { .. }
        | StorageError::UnknownRoot { .. }
        | StorageError::RelativePathRequired { .. }
        | StorageError::AbsolutePathRequired { .. } => ApiError::not_found("请求的封面不存在"),
        StorageError::RangeOutOfBounds { .. }
        | StorageError::EmptyRoots
        | StorageError::RootMissing { .. }
        | StorageError::Io { .. } => ApiError::internal(error),
    }
}

/// 标签读取错误 → API 错误。
///
/// 对「要一张封面」这个诉求来说，文件存在但解析不出标签 = 这首歌没有封面，一律 404；
/// 只有真正的 IO 故障（且不是「文件没了」）才算 500。
fn map_read_error(error: ReadError) -> ApiError {
    match error {
        // 认不出的容器 / 已知的不支持形态：文件在，但没有可用的封面。
        ReadError::Unrecognized
        | ReadError::Id3PrefixedReal(_)
        | ReadError::Id3PrefixedUnknown => ApiError::not_found("请求的封面不存在"),
        // 越界路径当 404，不泄漏部署目录结构（与 S18 同口径）。
        ReadError::Escape(_) => ApiError::not_found("请求的封面不存在"),
        // stat 之后文件被删掉 → 404；其余 IO 故障才是 500。
        ReadError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            ApiError::not_found("请求的封面不存在")
        }
        ReadError::Io(source) => ApiError::internal(format!("读取音频标签失败：{source}")),
    }
}

/// 封面超限 → **413 + COVER_TOO_LARGE**（明确拒绝，绝不截断）。
///
/// 用 413 的理由：这是标准状态码里「实体因体积被拒」最接近的一个，且是 4xx —— 客户端
/// 一眼能看出不是服务端崩溃。code 用稳定串 COVER_TOO_LARGE，前端据此换默认封面图，
/// 而不是把错误响应体当图片解析。**不静默截断**：截出来的图是坏的，客户端会当成成功
/// 却渲染失败，比直接报错更难排查。
fn map_cover_error(error: CoverError) -> ApiError {
    match error {
        CoverError::TooLarge { len } => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "COVER_TOO_LARGE",
            format!(
                "封面 {len} 字节，超过上限 {} 字节，拒绝返回",
                cover::MAX_COVER_BYTES
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    use axum::http::{header, HeaderMap, Request, StatusCode};
    use rusqlite::params;
    use tower::ServiceExt;

    use crate::db::migrations;
    use crate::db::models::{Role, User};
    use crate::db::pool::{DbPool, TempDb};
    use crate::server::auth::sign_token_with_ttl;
    use crate::server::routes::build_router;

    /// 测试用 JWT 密钥。
    const SECRET: &str = "s20-secret";

    /// 带封面的真实样本（见 tests/read.rs：has_cover = true，APIC type = 3）。
    const WITH_COVER: &str = "最美情侣-白小白.mp3";

    /// 没有封面的真实样本（has_cover = false）。
    const WITHOUT_COVER: &str = "华夏传说 - 凤凰传奇.mp3";

    /// 临时曲库根；Drop 时整棵删掉，避免在 /tmp 里漏文件。
    struct TempRoot(PathBuf);

    impl std::ops::Deref for TempRoot {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_root(tag: &str) -> TempRoot {
        // tag 里带进程号：并行跑 cargo test 时不同测试互不撞目录。
        let dir =
            std::env::temp_dir().join(format!("music-robot-s20-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建 S20 测试临时目录失败");
        TempRoot(dir)
    }

    /// 跑完迁移的临时库 + 一个真实存在的库根（FileStorage 构造要求根存在）。
    fn test_state(tag: &str, root: &Path) -> (AppState, TempDb) {
        let (pool, temp) = DbPool::open_temp(tag).expect("建临时文件库池");
        {
            let mut guard = pool.acquire().expect("借连接");
            migrations::apply(&mut guard).expect("应用迁移");
        }
        let mut config = Config::defaults();
        config.server.jwt_secret = SECRET.to_string();
        config.storage.library_roots = vec![root.to_string_lossy().into_owned()];
        (AppState::new(Arc::new(pool), Arc::new(config)), temp)
    }

    /// 预置一个用户并签一个可用令牌（不走注册接口，省掉 argon2 的开销）。
    fn issue_token(state: &AppState) -> String {
        let id = {
            let conn = state.db.acquire().expect("借连接");
            conn.execute(
                "INSERT INTO users (username, password_hash, role, created_at)
                 VALUES ('tester', 'hash', 'admin', ?1)",
                params![crate::db::now_unix_ms()],
            )
            .expect("预置用户");
            conn.last_insert_rowid()
        };
        let user = User {
            id,
            username: "tester".to_string(),
            password_hash: "hash".to_string(),
            role: Role::Admin,
            created_at: 0,
            last_login: None,
        };
        sign_token_with_ttl(SECRET, &user, 3600).expect("签发令牌")
    }

    /// fixtures 目录下的真实样本。
    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    /// 把样本拷进测试库根，返回落库用的规范路径（与沙箱 resolve 同口径）。
    fn copy_fixture(root: &Path, name: &str, dst_name: &str) -> String {
        let dst = root.join(dst_name);
        std::fs::copy(fixture_path(name), &dst).expect("拷贝样本");
        std::fs::canonicalize(&dst)
            .expect("canonicalize 样本路径")
            .to_string_lossy()
            .into_owned()
    }

    /// 预置一张专辑（可选带封面），返回 id。
    fn seed_album(state: &AppState, cover: Option<(&[u8], &str)>) -> i64 {
        let conn = state.db.acquire().expect("借连接");
        match cover {
            Some((data, mime)) => {
                conn.execute(
                    "INSERT INTO albums (name, album_artist, cover_data, cover_mime, updated_at)
                     VALUES ('测试专辑', '测试歌手', ?1, ?2, 1)",
                    params![data, mime],
                )
                .expect("预置带封面的专辑");
            }
            None => {
                conn.execute(
                    "INSERT INTO albums (name, album_artist, updated_at)
                     VALUES ('测试专辑', '测试歌手', 1)",
                    [],
                )
                .expect("预置空封面专辑");
            }
        }
        conn.last_insert_rowid()
    }

    /// 预置一首歌，返回 id；deleted 为 true 时写成软删行。
    fn seed_song(state: &AppState, file_path: &str, album_id: Option<i64>, deleted: bool) -> i64 {
        let conn = state.db.acquire().expect("借连接");
        conn.execute(
            "INSERT INTO songs (
                 file_path, album_id, title, search_text, scrape_status, added_at, updated_at,
                 deleted_at
             ) VALUES (?1, ?2, '测试曲目', '测试曲目', 'pending', 1, 1, ?3)",
            params![file_path, album_id, if deleted { Some(1i64) } else { None }],
        )
        .expect("预置歌曲");
        conn.last_insert_rowid()
    }

    /// 直接读专辑表里的封面，用来验证回填。
    fn stored_cover(state: &AppState, album_id: i64) -> Option<(Vec<u8>, Option<String>)> {
        let conn = state.db.acquire().expect("借连接");
        albums::read_cover(&conn, album_id)
            .expect("读专辑封面")
            .map(|c| (c.data, c.mime))
    }

    /// 从样本文件本身读出期望的 front cover 字节与 MIME（不是靠假设）。
    fn sample_front_cover(name: &str) -> (Vec<u8>, String) {
        let metadata = read_tags(&fixture_path(name)).expect("读样本标签");
        let picture = metadata
            .pictures
            .iter()
            .find(|p| p.pic_type == 3)
            .expect("样本应有 pic_type=3 的 front cover");
        (picture.data.clone(), picture.mime_type.clone())
    }

    /// tower oneshot 直调 Router；返回 (状态码, 响应头, 原始 body)。
    async fn call(
        state: &AppState,
        uri: &str,
        token: Option<&str>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut builder = Request::builder().uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = builder.body(Body::empty()).expect("构造请求");
        let response = build_router(state.clone())
            .oneshot(request)
            .await
            .expect("oneshot");
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), 32 * 1024 * 1024)
            .await
            .expect("读响应体")
            .to_vec();
        (status, headers, body)
    }

    /// 取响应头的字符串值。
    fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|text| text.to_string())
    }

    /// 断言两个 body 一致，但失败时不打印几十万字节。
    fn assert_same_bytes(actual: &[u8], expected: &[u8]) {
        assert_eq!(
            actual.len(),
            expected.len(),
            "body 字节数应与内嵌封面一致"
        );
        assert!(actual == expected, "body 必须与内嵌封面的字节完全一致");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 1. 有封面 → 200 + 字节 + MIME
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：有封面返回字节 + MIME；body 必须与直接从文件 read_tags 拿到的
    /// Picture.data 逐字节一致（不是只断言非空）。
    #[tokio::test]
    async fn embedded_cover_is_served_with_its_real_mime() {
        let root = temp_root("s20-bytes");
        let (state, temp) = test_state("s20-bytes", &root);
        let token = issue_token(&state);
        let path = copy_fixture(&root, WITH_COVER, "with-cover.mp3");
        // 库里没有封面 → 必须走第 2 步回退
        let album_id = seed_album(&state, None);
        let id = seed_song(&state, &path, Some(album_id), false);

        let (expected_data, expected_mime) = sample_front_cover(WITH_COVER);

        let (status, headers, body) =
            call(&state, &format!("/api/songs/{id}/cover"), Some(&token)).await;

        assert_eq!(status, StatusCode::OK);
        assert!(!body.is_empty(), "body 不能为空");
        assert_same_bytes(&body, &expected_data);

        let content_type = header_str(&headers, header::CONTENT_TYPE).expect("必须有 Content-Type");
        assert!(
            content_type.starts_with("image/"),
            "Content-Type 必须是图片类型，实际 {content_type}"
        );
        assert_eq!(content_type, expected_mime, "MIME 应与文件里的声明一致");
        assert_eq!(
            header_str(&headers, header::CONTENT_LENGTH),
            Some(body.len().to_string()),
            "Content-Length 必须等于 body 字节数"
        );
        assert!(
            header_str(&headers, header::CACHE_CONTROL).is_some(),
            "封面应带缓存策略"
        );

        drop(temp);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 2. 歌存在但没有任何封面 → 404
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：无封面 404（不是 204、也不是空图），且用统一错误形状。
    #[tokio::test]
    async fn song_without_any_cover_is_404() {
        let root = temp_root("s20-nocover");
        let (state, temp) = test_state("s20-nocover", &root);
        let token = issue_token(&state);
        let path = copy_fixture(&root, WITHOUT_COVER, "no-cover.mp3");
        let album_id = seed_album(&state, None);
        let id = seed_song(&state, &path, Some(album_id), false);

        // 先证明这个样本确实没有封面（不靠假设）
        let metadata = read_tags(&fixture_path(WITHOUT_COVER)).expect("读无封面样本");
        assert!(metadata.pictures.is_empty(), "该样本应确实没有内嵌封面");

        let (status, _headers, body) =
            call(&state, &format!("/api/songs/{id}/cover"), Some(&token)).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("404 的 body 是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "NOT_FOUND");
        assert!(!body.is_empty(), "错误响应也应有 body");

        drop(temp);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 3. 大图不 OOM → 明确拒绝，不静默截断
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：超过上限的封面必须被**明确拒绝**（413 + COVER_TOO_LARGE），
    /// 响应体是错误 JSON 而不是被截断的半张图；服务不 panic（能走到断言就没 panic）。
    #[tokio::test]
    async fn oversized_cover_is_rejected_instead_of_truncated() {
        let root = temp_root("s20-toolarge");
        let (state, temp) = test_state("s20-toolarge", &root);
        let token = issue_token(&state);

        // 直接往专辑表塞一张比上限大 1 字节的假封面，走第 1 步。
        let oversized = vec![0u8; cover::MAX_COVER_BYTES + 1];
        let album_id = seed_album(&state, Some((oversized.as_slice(), "image/jpeg")));
        let id = seed_song(&state, "/music/never-read.mp3", Some(album_id), false);

        let (status, headers, body) =
            call(&state, &format!("/api/songs/{id}/cover"), Some(&token)).await;

        assert_eq!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "超限必须明确拒绝，而不是返回被截断的图片"
        );
        let content_type =
            header_str(&headers, header::CONTENT_TYPE).unwrap_or_else(|| String::from(""));
        assert!(
            content_type.starts_with("application/json"),
            "超限响应必须是错误 JSON（证明没有返回图片字节），实际 {content_type}"
        );
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("超限响应是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "COVER_TOO_LARGE");

        drop(temp);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 4. 回退与回填
    // ─────────────────────────────────────────────────────────────────────────

    /// 库里 albums.cover_data 为空时：第一次请求能拿到封面，且请求之后被回填；
    /// 第二次请求走表里的数据（把磁盘文件删掉仍能返回同样的字节即证明）。
    #[tokio::test]
    async fn fallback_backfills_the_album_table_and_the_second_request_uses_it() {
        let root = temp_root("s20-backfill");
        let (state, temp) = test_state("s20-backfill", &root);
        let token = issue_token(&state);
        let path = copy_fixture(&root, WITH_COVER, "backfill.mp3");
        let album_id = seed_album(&state, None);
        let id = seed_song(&state, &path, Some(album_id), false);
        let uri = format!("/api/songs/{id}/cover");

        let (expected_data, expected_mime) = sample_front_cover(WITH_COVER);

        // 请求前：专辑表里确认为空（这就是画布标注的现状）
        assert!(
            stored_cover(&state, album_id).is_none(),
            "初始必须是空封面，否则测不到回退"
        );

        let (status, _headers, body) = call(&state, &uri, Some(&token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_same_bytes(&body, &expected_data);

        // 请求后：回填成功，字节与 MIME 都与文件里的一致
        let (stored_data, stored_mime) =
            stored_cover(&state, album_id).expect("第一次请求后应已回填封面");
        assert_same_bytes(&stored_data, &expected_data);
        assert_eq!(stored_mime.as_deref(), Some(expected_mime.as_str()));

        // 把音频文件删掉：第二次请求若还去读文件就会 404；能拿到同样的字节，
        // 就证明走的是专辑表。
        std::fs::remove_file(&path).expect("删掉音频文件");
        let (status, _headers, body) = call(&state, &uri, Some(&token)).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "第二次请求必须命中专辑表，不再依赖磁盘文件"
        );
        assert_same_bytes(&body, &expected_data);

        drop(temp);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 5. 不存在 / 软删 → 404；未登录 → 401
    // ─────────────────────────────────────────────────────────────────────────

    /// 不存在的 id、软删的歌都 404；未登录 / 伪造令牌都 401；非整数 id 是 400。
    #[tokio::test]
    async fn missing_soft_deleted_and_unauthenticated_requests() {
        let root = temp_root("s20-404-401");
        let (state, temp) = test_state("s20-404-401", &root);
        let token = issue_token(&state);
        let path = copy_fixture(&root, WITH_COVER, "auth.mp3");
        let album_id = seed_album(&state, None);
        // 磁盘上有文件、专辑也挂上了，但这一行是软删的
        let deleted_id = seed_song(&state, &path, Some(album_id), true);

        // 不存在的 id → 404
        let (status, _headers, body) = call(&state, "/api/songs/999999/cover", Some(&token)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("404 的 body 是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "NOT_FOUND");

        // 软删的歌 → 404
        let (status, _headers, body) = call(
            &state,
            &format!("/api/songs/{deleted_id}/cover"),
            Some(&token),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "软删的歌必须 404");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("404 的 body 是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "NOT_FOUND");

        // 未登录 → 401
        let (status, _headers, body) = call(
            &state,
            &format!("/api/songs/{deleted_id}/cover"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("401 的 body 是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "UNAUTHORIZED");

        // 伪造令牌 → 401
        let (status, _headers, _body) = call(
            &state,
            &format!("/api/songs/{deleted_id}/cover"),
            Some("not-a-real-token"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // 非整数 id → 400（不是 404）
        let (status, _headers, _body) = call(&state, "/api/songs/abc/cover", Some(&token)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        drop(temp);
    }
}
