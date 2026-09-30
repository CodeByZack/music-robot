//! S18 / S19 · 流式播放：`GET /api/stream/{id}`（直传原文件 + 可选转码）。
//!
//! 画布接口清单里播放走这里，前端只要有歌曲 id 就能拿到音频字节（不必知道
//! 服务器上的绝对路径 —— 曲库接口也刻意不暴露 `file_path`）。
//!
//! # 协议行为
//!
//! | 请求 | 响应 |
//! |------|------|
//! | 无 `Range` | 200 + 全量文件 + `Accept-Ranges: bytes` |
//! | 可满足的 `Range` | 206 + `Content-Range: bytes s-e/len` + `Accept-Ranges: bytes` |
//! | `Range` 起点越过文件尾 / 后缀为 0 | 416 + `Content-Range: bytes */len` |
//! | 多段 `Range`（`0-1,3-4`） | 416（本步明确拒绝，理由见 [`crate::audio::stream`] 模块文档）|
//! | `Range` 语法非法（非数字等） | 400 + 统一错误形状 |
//! | 未登录 | 401（受保护子 Router 的 require_auth + handler 里的 [`AuthUser`]）|
//! | 歌曲不存在 / 已软删 / 文件不在磁盘 | 404 |
//!
//! # S19 · 转码分支：`?format=mp3`
//!
//! | 请求 | 响应 |
//! |------|------|
//! | `?format=mp3`，缓存命中 | 200 + MP3 字节 + `Content-Type: audio/mpeg`（**不调 ffmpeg**）|
//! | `?format=mp3`，缓存未命中 | 同上，但先调 ffmpeg 转码并写缓存 |
//! | `?format=mp3` 且 ffmpeg 缺失 / 非零退出 / 超时 / 没产出文件 | 503 `SERVICE_UNAVAILABLE`（中文文案，不回传 stderr）|
//! | `?format=mp3` 且 `audio.transcode = false` | 503（转码能力被配置关掉了）|
//! | `format` 不是 mp3 | 400 `BAD_REQUEST` |
//! | `format` 缺省 | **与 S18 完全一致**：直传原文件 + 完整 Range 支持 |
//!
//! 转码缓存键、ffmpeg 参数、临时文件与原子落盘都在 [`crate::audio::transcode`]，
//! 这里只做 HTTP 层的事（参数解析、沙箱解析源路径、状态码映射、响应头）。
//!
//! ## 为什么用显式参数，而不是猜 `User-Agent`
//!
//! Safari 对 FLAC 支持差、需要一个 MP3 版本，Chrome 能直接播 FLAC —— 这是真实需求。
//! 但「按 `User-Agent` 猜」来满足它有两个硬伤：
//!
//! * **不可测**：UA 是外部输入，要覆盖 Safari / Chrome / 各种伪装 UA 就得写一串易漂移的
//!   匹配规则，测试只能对着这些规则自证，无法证明「真实浏览器拿到了什么」；
//! * **易误判**：UA 可以随便伪造，代理 / 播放器 / 自动化工具常把 UA 写成别的浏览器。
//!   猜错的后果要么是给 Chrome 白转一次码（浪费 CPU），要么是给 Safari 发一个它放不了的
//!   FLAC（用户直接看到播放失败）。
//!
//! 显式 `?format=mp3` 把决定权交给**真正知道自己在什么环境里运行的前端**：Safari 播放
//! 时带上它，Chrome 不带。服务端逻辑因此是确定性的、可单测的。
//!
//! ## 转码响应的 Range
//!
//! 转码分支**不支持 Range**：请求带 `Range` 也会被忽略，返回 200 + 完整 MP3，且**不带**
//! `Accept-Ranges`（不带这个头就是「本响应不承诺支持区间」，比声称支持却不实现诚实）。
//! 理由：MP3 比无损源小一个数量级，整段返回的代价可以接受；而按 `Range` 切片需要先把
//! 整个缓存文件读出来再切，收益很小、语义却要和 206 / `Content-Range` 对齐一遍。
//! RFC 9110 允许服务端忽略 `Range`，客户端会拿到完整响应而不是错误。
//!
//! # 内存特性（如实写明，见 `crate::audio::stream` 的完整说明）
//!
//! 直传走 `StorageBackend::read_range`，它返回 `Vec<u8>`：**请求多大就读多大进内存**。
//! 播放器常见的几十 KB ~ 几 MB 没问题；`Range: bytes=0-` 会把整个文件读进内存
//! （一首 FLAC 可能几十 MB）。本步不动 `src/storage.rs`；**要真正零缓冲，需要给
//! `StorageBackend` 加一个返回 `impl Read` 的流式接口**再接成响应体。
//! 这里不偷改客户端请求的区间 —— 那会破坏 HTTP 语义。
//!
//! # 路径安全
//!
//! `file_path` 虽然来自数据库，仍一律经 [`FileStorage`] / `PathSandbox` 解析
//! （`StoragePath::abs`），绝不拿它直接 `std::fs::read`：数据库被投毒 / 有历史脏数据时，
//! 越界的路径必须被沙箱挡住，而不是变成任意文件读取。
//!
//! 转码分支同样先 `stat` 拿到**沙箱解析后的真实路径**再交给 ffmpeg —— 绝不把数据库里的
//! 原始串直接塞进子进程参数。
//!
//! # 阻塞调用
//!
//! 读盘是同步阻塞的（S14 铁律），整段（构造存储后端 + stat + read_range）都包在
//! `tokio::task::spawn_blocking` 里。转码更重：它要 spawn 一个 ffmpeg 子进程并等它结束，
//! 同样整段进 `spawn_blocking`，绝不占着 tokio 的工作线程跑子进程。

use std::path::Path as FsPath;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::audio::stream::{content_type_for, parse_range, RangeError};
use crate::audio::transcode::{self, TranscodeError};
use crate::config::AudioConfig;
use crate::db::repos::songs;
use crate::server::auth::AuthUser;
use crate::server::error::{ApiError, ApiResult};
use crate::server::state::AppState;
use crate::storage::{FileStorage, StorageBackend, StorageError, StoragePath};

use super::library::{parse_id, run_db, QueryParams};

/// GET /api/stream/{id} —— 直传（S18）或转码后返回 MP3（S19）。
///
/// 把「查库 → 解析路径 → （转码 / stat → 判 Range → 读区间）」串起来；真正决定状态码的
/// 是 [`serve`] 与 [`serve_transcoded`]。handler 只负责把错误收敛成统一形状。
///
/// `?format=mp3` 走转码分支；不带 `format` 时行为与 S18 完全一致（直传 + Range）。
pub async fn stream(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
    params: QueryParams,
    headers: HeaderMap,
) -> Response {
    match stream_inner(state, raw_id, params, headers).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// 取歌曲、取 `Range` 头、在阻塞线程里读盘 / 转码。
///
/// 拆出来是为了让错误能用问号沿着 [`ApiError`] 冒泡；416 不走错误通道，因为它需要
/// 额外的 `Content-Range` 响应头，由 [`serve`] 直接构造完整响应。
async fn stream_inner(
    state: AppState,
    raw_id: String,
    params: QueryParams,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let id = parse_id(&raw_id)?;

    // `?format=`：缺省 / 空串 = 直传（S18 行为不变）；mp3 = 走 S19 转码缓存；
    // 其余取值 400。显式参数而不是猜 UA 的理由见模块文档。
    let transcode_requested = transcode::parse_format(params.get("format"))
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    if transcode_requested && !state.config.audio.transcode {
        // 配置把转码关掉了：源文件是好的，缺的是「转码」这个能力 → 503（不是 500/404）。
        return Err(ApiError::service_unavailable(
            "服务端未启用音频转码（audio.transcode = false）",
        ));
    }

    // 软删的歌视同不存在（include_deleted = false）→ 下面 404。
    let found = run_db(Arc::clone(&state.db), move |conn| {
        Ok(songs::get(conn, id, false)?)
    })
    .await?;
    let Some(song) = found else {
        return Err(ApiError::not_found("请求的歌曲不存在"));
    };

    // Range 头必须是 ASCII；非 ASCII 值按语法错误处理，不猜编码。
    // 转码分支不用它（Range 被忽略），但非法的头同样按 400 拒绝，两条路径口径一致。
    let range_header = match headers.get(header::RANGE) {
        Some(value) => Some(
            value
                .to_str()
                .map_err(|_| ApiError::bad_request("Range 请求头不是合法的 ASCII 文本"))?
                .to_string(),
        ),
        None => None,
    };

    let config = Arc::clone(&state.config);
    let file_path = song.file_path;
    let song_id = song.id;
    let audio_hash = song.audio_hash;
    let response = tokio::task::spawn_blocking(move || {
        // 每次请求从配置构造一次 FileStorage：它内部会对每个库根做一次
        // canonicalize（fail-closed，见 storage.rs）。代价是每请求 O(库根数) 次
        // canonicalize 系统调用 —— 对播放这种低频请求可以忽略，换来的是不必给
        // AppState 加状态、也不会缓存住一份过期的库根快照。
        let storage = FileStorage::from_config(&config.storage).map_err(map_storage_error)?;
        if transcode_requested {
            serve_transcoded(
                &storage,
                &file_path,
                song_id,
                audio_hash.as_deref(),
                &config.audio,
            )
        } else {
            serve(&storage, &file_path, range_header.as_deref())
        }
    })
    .await
    .map_err(|join| ApiError::internal(format!("音频流读取任务异常退出：{join}")))??;

    Ok(response)
}

/// 同步的读取核心：stat 拿长度 → 判 Range → 读区间 → 组装响应。
///
/// 只接收已构造好的存储后端，方便推理；所有 IO 都在调用方的 spawn_blocking 里发生。
fn serve(
    storage: &FileStorage,
    file_path: &str,
    range_header: Option<&str>,
) -> Result<Response, ApiError> {
    let path = StoragePath::abs(file_path);

    // 先 stat：Content-Length / Content-Range / 区间判界都需要文件总长度。
    // read_range 内部还会再 stat 一次；多一次 metadata 换取「长度一定和这次判界一致」，
    // 对一次播放请求可以忽略。
    let stat = storage.stat(&path).map_err(map_storage_error)?;
    if !stat.is_file {
        return Err(ApiError::not_found("请求的音频文件不存在"));
    }
    let file_len = stat.len;
    let content_type = content_type_for(file_path);

    // 没有 Range → None（200 全量）；有 Range → 解析，失败按错误分类直接回响应。
    let selected = match range_header {
        None => None,
        Some(raw) => match parse_range(raw, file_len) {
            Ok(range) => Some(range),
            Err(error) => return Ok(reject_range(error, file_len)),
        },
    };
    let partial = selected.is_some();

    let (offset, want, content_range) = match selected {
        None => (0, file_len, None),
        Some(range) => (
            range.start,
            range.byte_len(),
            Some(format!("bytes {}-{}/{}", range.start, range.end, file_len)),
        ),
    };

    let body = match storage.read_range(&path, offset, want) {
        Ok(bytes) => bytes,
        // stat 与 read_range 之间文件被替换 / 截断：如实回 416 + 新的文件长度，
        // 而不是回一个短了的 200/206。
        Err(StorageError::RangeOutOfBounds { file_len: actual, .. }) => {
            return Ok(unsatisfiable_response(actual));
        }
        Err(error) => return Err(map_storage_error(error)),
    };

    let status = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    success_response(status, body, content_type, content_range)
}

// ─────────────────────────────────────────────────────────────────────────────
// S19 · 转码分支
// ─────────────────────────────────────────────────────────────────────────────

/// 转码分支：把源文件转成 MP3（优先命中缓存）并组装响应。
///
/// 与 [`serve`] 的区别：
///
/// * 先经 [`FileStorage`] stat 一次，拿到**沙箱解析后的真实路径**再交给 ffmpeg ——
///   绝不把数据库里的原始 `file_path` 直接传给子进程；
/// * Range 被忽略（理由见模块文档），响应固定 200；
/// * 转码错误在这里收敛成 503 / 500，stderr 只进日志、不回客户端。
///
/// 所有 IO（stat + 子进程 + 读写缓存）都在调用方的 `spawn_blocking` 里发生。
fn serve_transcoded(
    storage: &FileStorage,
    file_path: &str,
    song_id: i64,
    audio_hash: Option<&str>,
    audio: &AudioConfig,
) -> Result<Response, ApiError> {
    let path = StoragePath::abs(file_path);
    let stat = storage.stat(&path).map_err(map_storage_error)?;
    if !stat.is_file {
        return Err(ApiError::not_found("请求的音频文件不存在"));
    }

    let bytes = transcode::transcode_to_mp3(
        &stat.path,
        song_id,
        audio_hash,
        &audio.ffmpeg_path,
        FsPath::new(&audio.cache_dir),
    )
    .map_err(map_transcode_error)?;

    Ok(mp3_response(bytes))
}

/// 组装转码成功的响应：200 + 完整 MP3。
///
/// **不带 `Accept-Ranges`**：本响应不支持区间请求，带这个头就是撒谎（见模块文档）。
fn mp3_response(body: Vec<u8>) -> Response {
    let length = body.len();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;

    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/mpeg"));
    // 显式给出 Content-Length，与 body 字节数一致。
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    response
}

/// 转码错误 → API 错误。
///
/// * 转码能力不可用（ffmpeg 缺失 / 非零退出 / 超时 / 没产出文件）→ **503**：
///   源文件是好的，坏的是「转码」这个依赖。用 500 会把它说成服务端 bug，用 404 会让
///   人以为这首歌的源文件没了；503（依赖不可用，可重试）才是准确的那一个。
/// * 缓存目录 / 读写故障 → **500**：那是部署 / 磁盘问题，[`ApiError::internal`] 自己打日志。
///
/// 两种情况都**不把 stderr 回给客户端**：它可能含服务器绝对路径。完整原因只进 stderr
/// 日志，响应体是固定中文文案。
fn map_transcode_error(error: TranscodeError) -> ApiError {
    match error {
        // 500 分支：internal 会把完整原因（含路径）写进日志，对外只给通用文案。
        TranscodeError::Io { .. } => ApiError::internal(error),
        other => {
            crate::serverlog::error("stream", format!("音频转码失败：{other}"));
            ApiError::service_unavailable("音频转码暂时不可用（ffmpeg 缺失或执行失败）")
        }
    }
}

/// 组装 200 / 206 的成功响应。
///
/// 响应头按画布要求齐备：`Accept-Ranges`（200 与 206 都带）、`Content-Length`、
/// `Content-Type`，206 另带 `Content-Range`。
fn success_response(
    status: StatusCode,
    body: Vec<u8>,
    content_type: &'static str,
    content_range: Option<String>,
) -> Result<Response, ApiError> {
    let length = body.len();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;

    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    // 显式给出 Content-Length：与 body 字节数一致（206 时也等于 Content-Range 的长度）。
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if let Some(value) = content_range {
        let parsed = HeaderValue::from_str(&value)
            .map_err(|e| ApiError::internal(format!("构造 Content-Range 响应头失败：{e}")))?;
        headers.insert(header::CONTENT_RANGE, parsed);
    }
    Ok(response)
}

/// 把非法的 `Range` 映射成响应：不可满足 → 416，语法非法 → 400。
fn reject_range(error: RangeError, file_len: u64) -> Response {
    if error.is_unsatisfiable() {
        return unsatisfiable_response(file_len);
    }
    // RFC 9110 允许服务端「忽略或拒绝」语法非法的 Range；这里选拒绝（400），
    // 因为静默当成「没有 Range」返回 200 全量，会让客户端以为自己的 Range 生效了。
    ApiError::bad_request(format!("Range 请求头不合法：{error}")).into_response()
}

/// 416：`Content-Range: bytes */<file_len>`（RFC 9110 对 416 的硬要求）。
fn unsatisfiable_response(file_len: u64) -> Response {
    let error = ApiError::new(
        StatusCode::RANGE_NOT_SATISFIABLE,
        "RANGE_NOT_SATISFIABLE",
        "请求的字节区间无法满足",
    );
    let mut response = error.into_response();
    let headers = response.headers_mut();
    // 顺带声明本服务支持 Range，客户端据此知道该按区间重试而不是放弃。
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(value) = HeaderValue::from_str(&format!("bytes */{file_len}")) {
        headers.insert(header::CONTENT_RANGE, value);
    }
    response
}

/// 存储层错误 → API 错误。
///
/// * 文件不在 / 解析不到库根内 / 不是普通文件 → **404**：对客户端都是「这首曲子取不到」。
///   刻意不区分「越界」与「不存在」—— 区分开等于把部署目录结构透给调用方。
/// * 区间越界 → 416（正常路径由 [`serve`] 提前处理，这里是兜底）。
/// * 配置故障（库根不存在 / 为空）与 IO 故障 → 500，完整原因只进日志。
fn map_storage_error(error: StorageError) -> ApiError {
    match error {
        StorageError::NotFound { .. }
        | StorageError::NotAFile { .. }
        | StorageError::NotADirectory { .. }
        | StorageError::Escape { .. }
        | StorageError::OutsideAnyRoot { .. }
        | StorageError::UnknownRoot { .. }
        | StorageError::RelativePathRequired { .. }
        | StorageError::AbsolutePathRequired { .. } => {
            ApiError::not_found("请求的音频文件不存在或不可访问")
        }
        StorageError::RangeOutOfBounds { .. } => ApiError::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "RANGE_NOT_SATISFIABLE",
            "请求的字节区间无法满足",
        ),
        StorageError::EmptyRoots | StorageError::RootMissing { .. } | StorageError::Io { .. } => {
            ApiError::internal(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    use axum::body::Body;
    use axum::http::{header, HeaderMap, Request, StatusCode};
    use rusqlite::params;
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::db::migrations;
    use crate::db::models::{Role, User};
    use crate::db::pool::{DbPool, TempDb};
    use crate::server::auth::sign_token_with_ttl;
    use crate::server::routes::build_router;

    /// 测试用 JWT 密钥。
    const SECRET: &str = "s18-secret";

    /// 测试音频内容：16 字节，值就是下标，方便逐字节断言。
    const AUDIO: &[u8] = b"0123456789abcdef";

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
        let dir = std::env::temp_dir().join(format!(
            "music-robot-s18-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建 S18 测试临时目录失败");
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

    /// 插一行歌曲，返回 id；deleted 为 true 时写成软删行。
    fn seed_song(state: &AppState, file_path: &str, deleted: bool) -> i64 {
        let conn = state.db.acquire().expect("借连接");
        conn.execute(
            "INSERT INTO songs (
                 file_path, title, search_text, scrape_status, added_at, updated_at, deleted_at
             ) VALUES (?1, '测试曲目', '测试曲目', 'pending', 1, 1, ?2)",
            params![file_path, if deleted { Some(1i64) } else { None }],
        )
        .expect("预置歌曲");
        conn.last_insert_rowid()
    }

    /// 写一个测试音频文件并返回路径字符串。
    fn write_audio(root: &Path, name: &str, bytes: &[u8]) -> String {
        let path = root.join(name);
        std::fs::write(&path, bytes).expect("写测试音频失败");
        path.to_str().expect("测试路径是 UTF-8").to_string()
    }

    /// tower oneshot 直调 Router；返回 (状态码, 响应头, 原始 body)。
    async fn call(
        state: &AppState,
        uri: &str,
        token: Option<&str>,
        range: Option<&str>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut builder = Request::builder().uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(range) = range {
            builder = builder.header(header::RANGE, range);
        }
        let request = builder.body(Body::empty()).expect("构造请求");
        let response = build_router(state.clone())
            .oneshot(request)
            .await
            .expect("oneshot");
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
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

    /// 建好「一个 16 字节 mp3 + 一条库记录」的测试环境。
    fn fixture(tag: &str) -> (AppState, TempRoot, TempDb, String, i64) {
        let root = temp_root(tag);
        let (state, temp) = test_state(tag, &root);
        let token = issue_token(&state);
        let path = write_audio(&root, "song.mp3", AUDIO);
        let id = seed_song(&state, &path, false);
        (state, root, temp, token, id)
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 1. 无 Range → 200 全量
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：没有 Range → 200 + 整个文件 + Accept-Ranges。
    #[tokio::test]
    async fn no_range_returns_the_whole_file_as_200() {
        let (state, _root, _temp, token, id) = fixture("s18-full");

        let (status, headers, body) =
            call(&state, &format!("/api/stream/{id}"), Some(&token), None).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, AUDIO, "body 必须与文件字节完全一致");
        assert_eq!(
            header_str(&headers, header::CONTENT_LENGTH),
            Some(AUDIO.len().to_string()),
            "Content-Length 必须等于文件大小"
        );
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE).as_deref(),
            Some("audio/mpeg")
        );
        assert_eq!(
            header_str(&headers, header::ACCEPT_RANGES).as_deref(),
            Some("bytes"),
            "200 也必须带 Accept-Ranges"
        );
        assert!(
            headers.get(header::CONTENT_RANGE).is_none(),
            "200 不该出现 Content-Range"
        );
    }

    /// Content-Type 按扩展名给：flac / wav / 认不出的都各有归属。
    #[tokio::test]
    async fn content_type_follows_the_file_extension() {
        let root = temp_root("s18-mime");
        let (state, temp) = test_state("s18-mime", &root);
        let token = issue_token(&state);

        let cases: [(&str, &str); 4] = [
            ("a.flac", "audio/flac"),
            ("b.wav", "audio/wav"),
            ("c.MP3", "audio/mpeg"),
            ("d.ogg", "application/octet-stream"),
        ];
        for (name, expected) in cases {
            let path = write_audio(&root, name, AUDIO);
            let id = seed_song(&state, &path, false);
            let (status, headers, _) =
                call(&state, &format!("/api/stream/{id}"), Some(&token), None).await;
            assert_eq!(status, StatusCode::OK, "{name}");
            assert_eq!(
                header_str(&headers, header::CONTENT_TYPE).as_deref(),
                Some(expected),
                "{name}"
            );
        }
        drop(temp);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 2. 有 Range → 206 + Content-Range
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：bytes=2-5 精确切片；另测 bytes=3-（到尾）与 bytes=-2（最后两字节）。
    #[tokio::test]
    async fn range_returns_206_with_exact_slice_and_content_range() {
        let (state, _root, _temp, token, id) = fixture("s18-206");
        let uri = format!("/api/stream/{id}");

        // (Range 头, 期望 body, 期望 Content-Range)；文件长 16。
        let cases: [(&str, &[u8], &str); 3] = [
            ("bytes=2-5", &AUDIO[2..=5], "bytes 2-5/16"),
            ("bytes=3-", &AUDIO[3..], "bytes 3-15/16"),
            ("bytes=-2", &AUDIO[14..], "bytes 14-15/16"),
        ];
        for (range, expected_body, expected_range) in cases {
            let (status, headers, body) = call(&state, &uri, Some(&token), Some(range)).await;
            assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{range} 应 206");
            assert_eq!(
                header_str(&headers, header::CONTENT_RANGE).as_deref(),
                Some(expected_range),
                "{range} 的 Content-Range 不对"
            );
            assert_eq!(body, expected_body, "{range} 的 body 不对");
            assert_eq!(
                header_str(&headers, header::CONTENT_LENGTH).map(|v| v.parse::<usize>().ok()),
                Some(Some(expected_body.len())),
                "{range} 的 Content-Length 应等于切片长度"
            );
            assert_eq!(
                header_str(&headers, header::ACCEPT_RANGES).as_deref(),
                Some("bytes"),
                "206 必须带 Accept-Ranges"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 3. 越界 → 416；正好整个文件 → 206
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：起点越过文件尾 → 416 + Content-Range: bytes */len；
    /// 边界 bytes=0-15（正好整个文件）必须是 206 而不是 416。
    #[tokio::test]
    async fn out_of_bounds_is_416_and_the_exact_whole_file_is_206() {
        let (state, _root, _temp, token, id) = fixture("s18-416");
        let uri = format!("/api/stream/{id}");

        for range in ["bytes=16-", "bytes=26-36"] {
            let (status, headers, body) = call(&state, &uri, Some(&token), Some(range)).await;
            assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE, "{range} 应 416");
            assert_eq!(
                header_str(&headers, header::CONTENT_RANGE).as_deref(),
                Some("bytes */16"),
                "{range} 必须带 Content-Range: bytes */16（RFC 9110）"
            );
            // 错误体保持统一形状
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("416 的 body 是统一 JSON 形状");
            assert_eq!(value["error"]["code"], "RANGE_NOT_SATISFIABLE");
        }

        // 边界：正好整个文件 → 206（不是 416）
        let (status, headers, body) =
            call(&state, &uri, Some(&token), Some("bytes=0-15")).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT, "正好整个文件必须 206");
        assert_eq!(
            header_str(&headers, header::CONTENT_RANGE).as_deref(),
            Some("bytes 0-15/16")
        );
        assert_eq!(body, AUDIO);
    }

    /// 语法非法的 Range → 400 + 统一错误形状（明确拒绝，不静默当没看见）。
    #[tokio::test]
    async fn malformed_range_is_400() {
        let (state, _root, _temp, token, id) = fixture("s18-badrange");
        let uri = format!("/api/stream/{id}");

        for range in ["", "bytes=", "bytes=abc", "bytes=5-3", "items=0-1"] {
            let (status, _headers, body) = call(&state, &uri, Some(&token), Some(range)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{range:?} 应 400");
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("400 的 body 是统一 JSON 形状");
            assert_eq!(value["error"]["code"], "BAD_REQUEST", "{range:?}");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 4. 多段 Range 拒绝
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布 UT：多段 Range（bytes=0-1,3-4）→ 416，并带 unsatisfied-range。
    /// 不实现 multipart/byteranges 的理由写在 crate::audio::stream 模块文档里。
    #[tokio::test]
    async fn multi_range_is_rejected_with_416() {
        let (state, _root, _temp, token, id) = fixture("s18-multirange");
        let uri = format!("/api/stream/{id}");

        for range in ["bytes=0-1,3-4", "bytes=0-1, 3-4", "bytes=0-,-1"] {
            let (status, headers, body) = call(&state, &uri, Some(&token), Some(range)).await;
            assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
            assert_eq!(
                header_str(&headers, header::CONTENT_RANGE).as_deref(),
                Some("bytes */16"),
                "{range}"
            );
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("416 的 body 是统一 JSON 形状");
            assert_eq!(value["error"]["code"], "RANGE_NOT_SATISFIABLE", "{range}");
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 6. 404
    // ─────────────────────────────────────────────────────────────────────────

    /// 不存在的 id / 软删的歌 / 文件在库里但磁盘上没有 → 都是 404（不是 500）。
    #[tokio::test]
    async fn missing_soft_deleted_and_vanished_files_are_404() {
        let root = temp_root("s18-404");
        let (state, temp) = test_state("s18-404", &root);
        let token = issue_token(&state);

        // 库里有记录，但磁盘上没有这个文件
        let vanished = root.join("vanished.mp3");
        let vanished_id = seed_song(
            &state,
            vanished.to_str().expect("测试路径是 UTF-8"),
            false,
        );
        // 磁盘上有文件，但记录已软删
        let alive = write_audio(&root, "alive.mp3", AUDIO);
        let deleted_id = seed_song(&state, &alive, true);

        let uris = [
            "/api/stream/999999".to_string(),
            format!("/api/stream/{vanished_id}"),
            format!("/api/stream/{deleted_id}"),
        ];
        for uri in uris {
            let (status, _headers, body) = call(&state, &uri, Some(&token), None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri} 必须 404");
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("404 的 body 是统一 JSON 形状");
            assert_eq!(value["error"]["code"], "NOT_FOUND", "{uri}");
        }

        // 非整数 id → 400（不是 404）
        let (status, _headers, body) = call(&state, "/api/stream/abc", Some(&token), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let value: serde_json::Value = serde_json::from_slice(&body).expect("400 是 JSON");
        assert_eq!(value["error"]["code"], "BAD_REQUEST");

        // 路径逃出库根的记录也必须 404（绝不能被当成任意文件读取）
        let outside = root.join("..").join("s18-outside.mp3");
        let escape_id = seed_song(
            &state,
            outside.to_str().expect("测试路径是 UTF-8"),
            false,
        );
        let (status, _headers, _body) = call(
            &state,
            &format!("/api/stream/{escape_id}"),
            Some(&token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "越界路径必须是 404");

        drop(temp);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // 7. 未登录 → 401
    // ─────────────────────────────────────────────────────────────────────────

    /// 画布要求：/api/stream 需要登录；不带令牌一律 401。
    #[tokio::test]
    async fn unauthenticated_requests_are_401() {
        let (state, _root, _temp, _token, id) = fixture("s18-401");
        let uri = format!("/api/stream/{id}");

        let (status, _headers, body) = call(&state, &uri, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("401 的 body 是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "UNAUTHORIZED");

        // 伪造 / 乱写的令牌同样 401
        let (status, _headers, _body) = call(&state, &uri, Some("not-a-real-token"), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// 空文件：无 Range → 200 + Content-Length: 0；带 Range → 416（没有任何字节可满足）。
    #[tokio::test]
    async fn empty_file_is_200_without_range_and_416_with_range() {
        let root = temp_root("s18-empty");
        let (state, temp) = test_state("s18-empty", &root);
        let token = issue_token(&state);
        let path = write_audio(&root, "empty.mp3", b"");
        let id = seed_song(&state, &path, false);
        let uri = format!("/api/stream/{id}");

        let (status, headers, body) = call(&state, &uri, Some(&token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_empty());
        assert_eq!(
            header_str(&headers, header::CONTENT_LENGTH),
            Some("0".to_string())
        );

        let (status, headers, _body) = call(&state, &uri, Some(&token), Some("bytes=0-")).await;
        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            header_str(&headers, header::CONTENT_RANGE).as_deref(),
            Some("bytes */0")
        );

        drop(temp);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // S19 · 转码缓存
    //
    // 关键：**不用真实 ffmpeg**。临时目录里写一个 sh 脚本，每被调用一次就往计数文件
    // 追加一行，然后 cp 输入当输出（它不需要真转码，只要产出文件）。脚本路径写进
    // config.audio.ffmpeg_path —— 计数文件就是「到底调没调 ffmpeg」的直接证据。
    // ─────────────────────────────────────────────────────────────────────────

    /// 假 ffmpeg 脚本：调用计数 + 把输入原样拷成输出。
    ///
    /// fail = true 时 exit 1 且不产出任何文件（模拟执行失败）。
    /// 返回脚本的绝对路径，供写进 config.audio.ffmpeg_path。
    fn write_fake_ffmpeg(bin_dir: &Path, counter: &Path, fail: bool) -> String {
        use std::os::unix::fs::PermissionsExt;

        let script = bin_dir.join("fake-ffmpeg.sh");
        let counter = counter.to_string_lossy().into_owned();
        // 参数形如：-i <src> ... -f mp3 <out>；取 -i 的下一项当输入、最后一项当输出。
        let body = if fail {
            format!("#!/bin/sh\necho x >> '{counter}'\nexit 1\n")
        } else {
            format!(
                "#!/bin/sh\n\
                 echo x >> '{counter}'\n\
                 src=''\n\
                 out=''\n\
                 prev=''\n\
                 for a in \"$@\"; do\n\
                   if [ \"$prev\" = '-i' ]; then src=\"$a\"; fi\n\
                   prev=\"$a\"\n\
                   out=\"$a\"\n\
                 done\n\
                 cp \"$src\" \"$out\"\n"
            )
        };
        std::fs::write(&script, body).expect("写假 ffmpeg 脚本失败");
        let mut perms = std::fs::metadata(&script)
            .expect("读假 ffmpeg 脚本元数据失败")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("给假 ffmpeg 脚本可执行位失败");
        script.to_str().expect("脚本路径是 UTF-8").to_string()
    }

    /// 读假 ffmpeg 的调用计数（文件不存在 = 0 次）。
    fn ffmpeg_calls(counter: &Path) -> usize {
        match std::fs::read_to_string(counter) {
            Ok(text) => text.lines().filter(|line| !line.trim().is_empty()).count(),
            Err(_) => 0,
        }
    }

    /// S19 测试环境：库根 / 缓存目录 / 假 ffmpeg 三者齐全。
    struct TranscodeEnv {
        state: AppState,
        root: TempRoot,
        /// 缓存目录（与 cache_dir 同路径；用 TempRoot 保活 + Drop 清理）
        _cache: TempRoot,
        /// 假 ffmpeg 脚本所在目录
        _bin: TempRoot,
        _temp: TempDb,
        token: String,
        id: i64,
        counter: PathBuf,
        cache_dir: PathBuf,
    }

    impl TranscodeEnv {
        /// 这首歌在给定 audio_hash 下的缓存文件路径。
        fn cache_file(&self, hash: &str) -> PathBuf {
            self.cache_dir.join(format!("{}-{hash}.mp3", self.id))
        }

        /// 缓存目录里的文件名（排序后），用来断言「有没有半截产物」。
        fn cache_entries(&self) -> Vec<String> {
            let mut names: Vec<String> = match std::fs::read_dir(&self.cache_dir) {
                Ok(entries) => entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect(),
                Err(_) => Vec::new(),
            };
            names.sort();
            names
        }
    }

    /// 建一个跑完迁移、库根 / 缓存目录 / ffmpeg 路径都可控的 state。
    fn transcode_state(
        tag: &str,
        root: &Path,
        cache: &Path,
        ffmpeg: &str,
        enabled: bool,
    ) -> (AppState, TempDb) {
        let (pool, temp) = DbPool::open_temp(tag).expect("建临时文件库池");
        {
            let mut guard = pool.acquire().expect("借连接");
            migrations::apply(&mut guard).expect("应用迁移");
        }
        let mut config = Config::defaults();
        config.server.jwt_secret = SECRET.to_string();
        config.storage.library_roots = vec![root.to_string_lossy().into_owned()];
        config.audio.transcode = enabled;
        config.audio.cache_dir = cache.to_string_lossy().into_owned();
        config.audio.ffmpeg_path = ffmpeg.to_string();
        (AppState::new(Arc::new(pool), Arc::new(config)), temp)
    }

    /// 给歌曲写 audio_hash（seed_song 不写这一列）。
    fn set_audio_hash(state: &AppState, id: i64, hash: Option<&str>) {
        let conn = state.db.acquire().expect("借连接");
        conn.execute(
            "UPDATE songs SET audio_hash = ?1 WHERE id = ?2",
            params![hash, id],
        )
        .expect("更新 audio_hash");
    }

    /// 组装 TranscodeEnv；ffmpeg 可执行文件路径与计数文件由调用方给。
    fn transcode_env_from(
        tag: &str,
        bin: TempRoot,
        counter: PathBuf,
        ffmpeg: String,
        enabled: bool,
    ) -> TranscodeEnv {
        let root = temp_root(&format!("{tag}-lib"));
        let cache = temp_root(&format!("{tag}-cache"));
        let (state, temp) = transcode_state(tag, &root, &cache, &ffmpeg, enabled);
        let token = issue_token(&state);
        let path = write_audio(&root, "song.flac", AUDIO);
        let id = seed_song(&state, &path, false);
        set_audio_hash(&state, id, Some("hash-a"));
        // 先取路径再 move（TempRoot 不实现 Clone）。
        let cache_dir = cache.to_path_buf();
        TranscodeEnv {
            state,
            root,
            _cache: cache,
            _bin: bin,
            _temp: temp,
            token,
            id,
            counter,
            cache_dir,
        }
    }

    /// 带假 ffmpeg 脚本的环境。
    fn transcode_env(tag: &str, fail_script: bool) -> TranscodeEnv {
        let bin = temp_root(&format!("{tag}-bin"));
        let counter = bin.join("calls.txt");
        let ffmpeg = write_fake_ffmpeg(&bin, &counter, fail_script);
        transcode_env_from(tag, bin, counter, ffmpeg, true)
    }

    /// ffmpeg 路径指向不存在的文件（模拟没装 ffmpeg）。
    fn missing_ffmpeg_env(tag: &str) -> TranscodeEnv {
        let bin = temp_root(&format!("{tag}-bin"));
        let counter = bin.join("calls.txt");
        transcode_env_from(
            tag,
            bin,
            counter,
            "/no/such/ffmpeg-does-not-exist".to_string(),
            true,
        )
    }

    /// 配置里关掉转码（audio.transcode = false）。
    fn disabled_transcode_env(tag: &str) -> TranscodeEnv {
        let bin = temp_root(&format!("{tag}-bin"));
        let counter = bin.join("calls.txt");
        let ffmpeg = write_fake_ffmpeg(&bin, &counter, false);
        transcode_env_from(tag, bin, counter, ffmpeg, false)
    }

    /// 画布 UT 1：未命中 → 转码 → 写缓存，响应是 MP3 字节。
    #[tokio::test]
    async fn transcode_miss_writes_cache_and_returns_mp3() {
        let env = transcode_env("s19-miss", false);
        let uri = format!("/api/stream/{}?format=mp3", env.id);

        let (status, headers, body) = call(&env.state, &uri, Some(&env.token), None).await;

        assert_eq!(status, StatusCode::OK, "转码请求应 200");
        assert_eq!(body, AUDIO, "假 ffmpeg 把源文件原样拷成输出");
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE).as_deref(),
            Some("audio/mpeg"),
            "转码响应必须是 audio/mpeg"
        );
        assert_eq!(
            header_str(&headers, header::CONTENT_LENGTH),
            Some(AUDIO.len().to_string())
        );
        assert!(
            headers.get(header::ACCEPT_RANGES).is_none(),
            "转码响应不承诺 Range 支持，不能带 Accept-Ranges"
        );
        assert_eq!(ffmpeg_calls(&env.counter), 1, "未命中必须调一次 ffmpeg");

        let cache_file = env.cache_file("hash-a");
        assert!(
            cache_file.is_file(),
            "未命中后必须写出缓存文件，实际目录内容：{:?}",
            env.cache_entries()
        );
        assert_eq!(std::fs::read(&cache_file).expect("读缓存失败"), AUDIO);
    }

    /// 画布 UT 2：二次命中**不调 ffmpeg** —— 计数仍为 1 是硬证据。
    #[tokio::test]
    async fn second_request_hits_cache_without_calling_ffmpeg() {
        let env = transcode_env("s19-hit", false);
        let uri = format!("/api/stream/{}?format=mp3", env.id);

        let (first_status, _h, first_body) = call(&env.state, &uri, Some(&env.token), None).await;
        let (second_status, _h, second_body) = call(&env.state, &uri, Some(&env.token), None).await;

        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(first_body, second_body, "两次响应字节必须一致");
        assert_eq!(
            ffmpeg_calls(&env.counter),
            1,
            "二次命中绝不能再调 ffmpeg（这是硬证据，不是只看响应成功）"
        );
    }

    /// 画布 UT 3：audio_hash 变化 → 缓存失效（产生新文件、重新转码、旧文件仍在）。
    #[tokio::test]
    async fn audio_hash_change_produces_a_new_cache_file() {
        let env = transcode_env("s19-rehash", false);
        let uri = format!("/api/stream/{}?format=mp3", env.id);

        let (status, _h, _body) = call(&env.state, &uri, Some(&env.token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ffmpeg_calls(&env.counter), 1, "第一次未命中要转码");
        let old_cache = env.cache_file("hash-a");
        assert!(old_cache.is_file());

        // 改库里那行的 audio_hash → 缓存键跟着变。
        set_audio_hash(&env.state, env.id, Some("hash-b"));
        let (status, _h, body) = call(&env.state, &uri, Some(&env.token), None).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, AUDIO);
        assert_eq!(
            ffmpeg_calls(&env.counter),
            2,
            "换哈希后缓存键变了，必须重新转码"
        );
        assert!(
            env.cache_file("hash-b").is_file(),
            "必须产生含新哈希的缓存文件，实际目录：{:?}",
            env.cache_entries()
        );
        assert!(
            old_cache.is_file(),
            "旧缓存文件必须还在 —— 失效是靠换键，不是删文件"
        );
    }

    /// 画布 UT 4：ffmpeg 缺失 → 503 + 统一错误形状 + 中文消息。
    #[tokio::test]
    async fn missing_ffmpeg_is_503_with_uniform_shape() {
        let env = missing_ffmpeg_env("s19-noffmpeg");
        let uri = format!("/api/stream/{}?format=mp3", env.id);

        let (status, _headers, body) = call(&env.state, &uri, Some(&env.token), None).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "ffmpeg 缺失必须是 503");
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("503 的 body 是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "SERVICE_UNAVAILABLE");
        assert!(
            value["error"].get("details").is_some(),
            "统一形状必须带 details"
        );
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "错误信息必须是中文：{message}"
        );
        assert!(
            !message.contains("no/such/ffmpeg"),
            "不能把服务器上的 ffmpeg 路径回给客户端：{message}"
        );
        assert!(
            env.cache_entries().is_empty(),
            "失败不能留下任何缓存文件：{:?}",
            env.cache_entries()
        );
    }

    /// 我加的 UT 5：ffmpeg 执行失败（exit 1 且无输出）→ 503，且无半截缓存。
    #[tokio::test]
    async fn failing_ffmpeg_is_503_and_leaves_no_cache_file() {
        let env = transcode_env("s19-fail", true);
        let uri = format!("/api/stream/{}?format=mp3", env.id);

        let (status, _headers, body) = call(&env.state, &uri, Some(&env.token), None).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("503 的 body 是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "SERVICE_UNAVAILABLE");
        assert_eq!(ffmpeg_calls(&env.counter), 1, "脚本确实被调用过一次");
        assert!(
            !env.cache_file("hash-a").exists(),
            "失败绝不能留下缓存文件"
        );
        assert!(
            env.cache_entries().is_empty(),
            "缓存目录里不该有半截产物：{:?}",
            env.cache_entries()
        );
    }

    /// 我加的 UT 6（回归）：不带 format 时行为不变，且绝不调 ffmpeg。
    #[tokio::test]
    async fn direct_stream_without_format_is_unchanged_and_never_transcodes() {
        let env = transcode_env("s19-direct", false);
        let uri = format!("/api/stream/{}", env.id);

        // 无 Range → 200 全量原文件（不是转码产物）
        let (status, headers, body) = call(&env.state, &uri, Some(&env.token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, AUDIO, "直传必须原样返回源文件");
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE).as_deref(),
            Some("audio/flac"),
            "直传按源文件扩展名给 Content-Type"
        );
        assert_eq!(
            header_str(&headers, header::ACCEPT_RANGES).as_deref(),
            Some("bytes")
        );

        // Range → 206 + Content-Range（S18 语义不变）
        let (status, headers, body) =
            call(&env.state, &uri, Some(&env.token), Some("bytes=2-5")).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            header_str(&headers, header::CONTENT_RANGE).as_deref(),
            Some("bytes 2-5/16")
        );
        assert_eq!(body, &AUDIO[2..=5]);

        assert_eq!(ffmpeg_calls(&env.counter), 0, "直传分支绝不该调 ffmpeg");
        assert!(
            env.cache_entries().is_empty(),
            "直传不该产生任何缓存：{:?}",
            env.cache_entries()
        );
    }

    /// 我加的 UT 7：未登录 → 401；不存在 / 软删 → 404；两者都不调 ffmpeg。
    #[tokio::test]
    async fn transcode_requires_auth_and_maps_missing_songs_to_404() {
        let env = transcode_env("s19-auth", false);
        let uri = format!("/api/stream/{}?format=mp3", env.id);

        // 未登录 → 401
        let (status, _headers, body) = call(&env.state, &uri, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("401 的 body 是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "UNAUTHORIZED");

        // 不存在 → 404
        let (status, _headers, body) =
            call(&env.state, "/api/stream/999999?format=mp3", Some(&env.token), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("404 的 body 是统一 JSON 形状");
        assert_eq!(value["error"]["code"], "NOT_FOUND");

        // 软删 → 404
        let gone_path = write_audio(&env.root, "gone.flac", AUDIO);
        let gone_id = seed_song(&env.state, &gone_path, true);
        let (status, _headers, body) = call(
            &env.state,
            &format!("/api/stream/{gone_id}?format=mp3"),
            Some(&env.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "软删的歌必须 404");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("404 是 JSON");
        assert_eq!(value["error"]["code"], "NOT_FOUND");

        assert_eq!(
            ffmpeg_calls(&env.counter),
            0,
            "鉴权失败 / 歌不存在都不该慢到去调 ffmpeg"
        );
    }

    /// 我加的 UT 8：audio_hash 为 NULL → 照常转码，但不落缓存（没有稳定键）。
    #[tokio::test]
    async fn null_audio_hash_transcodes_without_caching() {
        let env = transcode_env("s19-nohash", false);
        set_audio_hash(&env.state, env.id, None);
        let uri = format!("/api/stream/{}?format=mp3", env.id);

        let (status, _headers, body) = call(&env.state, &uri, Some(&env.token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, AUDIO);
        assert_eq!(ffmpeg_calls(&env.counter), 1, "没有缓存键时仍要转码");
        assert!(
            env.cache_entries().is_empty(),
            "没有稳定键就不该落缓存：{:?}",
            env.cache_entries()
        );

        // 没有缓存可命中，所以再来一次还会转码。
        let (status, _headers, _body) = call(&env.state, &uri, Some(&env.token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ffmpeg_calls(&env.counter), 2);
    }

    /// 我加的 UT 9：不支持的 format → 400；配置关掉转码 → 503。两者都不调 ffmpeg。
    #[tokio::test]
    async fn unsupported_format_is_400_and_disabled_transcode_is_503() {
        let env = transcode_env("s19-fmt", false);
        let (status, _headers, body) = call(
            &env.state,
            &format!("/api/stream/{}?format=flac", env.id),
            Some(&env.token),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "不支持的格式必须 400");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("400 是 JSON");
        assert_eq!(value["error"]["code"], "BAD_REQUEST");
        assert_eq!(ffmpeg_calls(&env.counter), 0);

        let disabled = disabled_transcode_env("s19-disabled");
        let (status, _headers, body) = call(
            &disabled.state,
            &format!("/api/stream/{}?format=mp3", disabled.id),
            Some(&disabled.token),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "配置关掉转码必须 503"
        );
        let value: serde_json::Value = serde_json::from_slice(&body).expect("503 是 JSON");
        assert_eq!(value["error"]["code"], "SERVICE_UNAVAILABLE");
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "错误信息必须是中文：{message}"
        );
        assert_eq!(ffmpeg_calls(&disabled.counter), 0);
    }
}
