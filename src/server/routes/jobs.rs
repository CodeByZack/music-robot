//! S17 · 扫描 / 刮削 API：触发 + 轮询状态。
//!
//! 路径（画布没给，由本步骤定下）：
//!
//! * POST /api/scan              触发一次曲库扫描 → 202 + { batch_id, kind, status }
//! * GET  /api/scan/{batch_id}   轮询扫描进度 → 200 + 状态对象
//! * POST /api/scrape            触发批量为 pending 的歌刮削 → 202 + { batch_id, kind, status }
//! * GET  /api/scrape/{batch_id} 轮询刮削进度 → 200 + 状态对象
//! * GET  /api/jobs              列出全部任务（管理端展示用）
//!
//! ## 权限策略：五个接口一律要求 admin
//!
//! 扫描要遍历整个曲库，刮削要写回文件标签、还可能访问外部站点 —— 都是重操作，
//! 也是磁盘 / 网络出口的消耗者。普通用户不该触发，所以统一用 AdminUser 提取器
//! （非 admin 403）；未登录由受保护子 Router 的 require_auth 中间件先拦成 401。
//! 这条策略写在路由组装处（routes::build_router）与本文件两处，改权限时别漏。
//!
//! ## 长任务不占 handler
//!
//! LibraryService::scan / BatchRunner::run 是同步阻塞的长任务（几秒到几分钟）。
//! handler 只做两件事：抢单例锁（注册表内的原子操作，微秒级）→ 把 guard 交给
//! 独立 OS 线程 → 立刻返回 202。**不用 spawn_blocking**：那是给短阻塞的，
//! 长任务会把 blocking 线程池占满，连鉴权查库都会跟着饿死。
//!
//! 进线程前把需要的句柄 Arc::clone 出来；线程闭包只捕获 Arc 与 guard，
//! 不捕获 &AppState（生命周期过不去）。线程不 join（JoinHandle 直接丢弃 = detach），
//! 释放单例锁只依赖 JobGuard 的 Drop。
//!
//! ## 单例冲突
//!
//! 同类任务已在跑 → JobRegistry::try_start 返回 AlreadyRunning → 这里转 409
//! （ApiError::conflict），details 里带上正在跑的 batch_id，方便前端提示。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::server::auth::AdminUser;
use crate::server::error::{ApiError, ApiResult};
use crate::server::jobs::{
    AlreadyRunning, Counters, JobKind, JobOutcome, JobStatus, ProgressSource,
};
use crate::server::state::AppState;
use crate::service::{BatchReport, BatchRunner, ScanReport};

/// POST /api/scan —— 触发一次曲库扫描（admin）。
///
/// 立刻返回 202 + batch_id；真正的扫描在独立线程里跑，进度用 GET 轮询。
pub async fn start_scan(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> ApiResult<(StatusCode, Json<Value>)> {
    // 抢单例锁：与「登记状态」在同一个临界区完成，并发触发只有一个能过。
    let guard = state
        .jobs
        .try_start(JobKind::Scan, None)
        .map_err(|active| conflict_error(JobKind::Scan, &active))?;
    let batch_id = guard.batch_id().to_string();
    let library = Arc::clone(&state.library);

    let spawned = std::thread::Builder::new()
        .name(format!("scan-{batch_id}"))
        .spawn(move || {
            guard.run_catching_panic(move || match library.scan() {
                Ok(report) => {
                    JobOutcome::done(scan_counters(&report), Some(report.to_string()))
                }
                Err(e) => JobOutcome::failed(e.to_string()),
            });
        });
    if let Err(e) = spawned {
        // spawn 失败时闭包已被丢弃 → guard 的 Drop 已经释放单例锁，这里如实报 500。
        return Err(ApiError::internal(format!("启动扫描线程失败：{e}")));
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(accepted_body(&batch_id, JobKind::Scan)),
    ))
}

/// POST /api/scrape —— 触发刮削（admin）。
///
/// 请求体**可选**，不给就是原来的行为（队列 = songs.scrape_status = 'pending'，画布区块 ③）：
///
/// | 请求体 | 队列 |
/// |---|---|
/// | 无 / `{}` / `{"mode":"pending"}` | `scrape_status = 'pending'`（新入库的歌） |
/// | `{"mode":"failed"}` | `scrape_status = 'failed'`（重刮失败项） |
/// | `{"song_ids":[1,2]}` | 显式指定曲目，**可含已经 done 的** —— 这才是「重新刮削」 |
///
/// 为什么要这个：`pending` 队列只收新入库的歌（`library.rs` 明确不重置已有行的状态），
/// 所以在那之前，一首刮过的歌（不管成功失败）**再没有任何接口能碰到它**，
/// 只能手改数据库。service 层的 `run_failed` / `run_ids` 早就写好了，一直没接路由。
pub async fn start_scrape(
    State(state): State<AppState>,
    _admin: AdminUser,
    body: Option<Json<Value>>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let queue = parse_scrape_body(body.as_ref().map(|Json(value)| value))?;
    // 实时进度源：内部读的是 BatchRunner 的原子计数，轮询时不会阻塞。
    let live: Arc<dyn ProgressSource> = Arc::new(RunnerProgress(Arc::clone(&state.scrape)));
    let guard = state
        .jobs
        .try_start(JobKind::Scrape, Some(live))
        .map_err(|active| conflict_error(JobKind::Scrape, &active))?;
    let batch_id = guard.batch_id().to_string();
    let runner = Arc::clone(&state.scrape);

    let spawned = std::thread::Builder::new()
        .name(format!("scrape-{batch_id}"))
        .spawn(move || {
            guard.run_catching_panic(move || {
                let report = match &queue {
                    ScrapeQueue::Pending => runner.run(),
                    ScrapeQueue::Failed => runner.run_failed(),
                    ScrapeQueue::Ids(ids) => runner.run_ids(ids),
                };
                match report {
                    Ok(report) => {
                        JobOutcome::done(batch_counters(&report), Some(report.to_string()))
                    }
                    Err(e) => JobOutcome::failed(e.to_string()),
                }
            });
        });
    if let Err(e) = spawned {
        return Err(ApiError::internal(format!("启动刮削线程失败：{e}")));
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(accepted_body(&batch_id, JobKind::Scrape)),
    ))
}

/// GET /api/scan/{batch_id} —— 轮询扫描进度（admin）。
pub async fn scan_status(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(batch_id): Path<String>,
) -> ApiResult<Json<Value>> {
    status_of(&state, &batch_id, JobKind::Scan)
}

/// GET /api/scrape/{batch_id} —— 轮询刮削进度（admin）。
pub async fn scrape_status(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(batch_id): Path<String>,
) -> ApiResult<Json<Value>> {
    status_of(&state, &batch_id, JobKind::Scrape)
}

/// GET /api/jobs —— 列出全部任务（admin）。
pub async fn list(State(state): State<AppState>, _admin: AdminUser) -> ApiResult<Json<Value>> {
    let items: Vec<Value> = state.jobs.list().iter().map(|job| job.to_json()).collect();
    let count = items.len();
    Ok(Json(json!({ "items": items, "count": count })))
}

// ─────────────────────────────────────────────────────────────────────────────
// 辅助
// ─────────────────────────────────────────────────────────────────────────────

/// 把 BatchRunner 的原子进度快照接进任务注册表。
struct RunnerProgress(Arc<BatchRunner>);

impl ProgressSource for RunnerProgress {
    fn counters(&self) -> Counters {
        let snapshot = self.0.progress();
        Counters {
            total: snapshot.total,
            done: snapshot.done,
            failed: snapshot.failed,
            skipped: snapshot.skipped,
        }
    }
}

/// `POST /api/scrape` 这次要刮哪些歌。
#[derive(Debug, PartialEq, Eq)]
enum ScrapeQueue {
    /// 默认：`scrape_status = 'pending'`（新入库的歌）。
    Pending,
    /// `{"mode":"failed"}`：只重刮失败项。
    Failed,
    /// `{"song_ids":[...]}`：显式指定，**可含已经 done 的**。
    Ids(Vec<i64>),
}

/// 解析 `POST /api/scrape` 的可选请求体。
///
/// **不给请求体 = pending 队列**，与加这个参数之前的行为一模一样，老客户端不受影响。
///
/// 字段名写错一律 400，**绝不静默退化成「刮全库」**—— 刮削会覆盖原文件，
/// 一个拼错的 `mode` 悄悄变成全库重刮，代价太大。（画布区块 ⑦ 的安全不变式同理：
/// 宁可报错，也不要做用户没要求的破坏性操作。）
fn parse_scrape_body(body: Option<&Value>) -> Result<ScrapeQueue, ApiError> {
    let Some(body) = body else {
        return Ok(ScrapeQueue::Pending);
    };
    let obj = body
        .as_object()
        .ok_or_else(|| ApiError::bad_request("请求体必须是 JSON 对象"))?;
    for key in obj.keys() {
        if key != "mode" && key != "song_ids" {
            return Err(ApiError::bad_request(format!(
                "不认识的字段「{key}」；POST /api/scrape 只支持 mode 与 song_ids"
            )));
        }
    }

    let ids = match obj.get("song_ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return Err(ApiError::bad_request(
                    "song_ids 不能是空数组；想刮 pending 队列就别传这个字段",
                ));
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                // is_i64 对 1.0 这类浮点返回 None，正好挡住「id 写成小数」。
                let id = item.as_i64().ok_or_else(|| {
                    ApiError::bad_request("song_ids 里必须都是整数 id")
                })?;
                if id <= 0 {
                    return Err(ApiError::bad_request(format!(
                        "song_ids 里的 id 必须是正整数，收到 {id}"
                    )));
                }
                out.push(id);
            }
            Some(out)
        }
        Some(_) => return Err(ApiError::bad_request("song_ids 必须是整数数组")),
    };

    let mode = match obj.get("mode") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.as_str()),
        Some(_) => return Err(ApiError::bad_request("mode 必须是字符串")),
    };

    // song_ids 优先：它是更具体的指定，同时给 mode 时以 ids 为准。
    if let Some(ids) = ids {
        return Ok(ScrapeQueue::Ids(ids));
    }
    match mode {
        None | Some("pending") => Ok(ScrapeQueue::Pending),
        Some("failed") => Ok(ScrapeQueue::Failed),
        Some(other) => Err(ApiError::bad_request(format!(
            "mode 只支持 \"pending\" 或 \"failed\"，收到「{other}」"
        ))),
    }
}

/// 202 响应体：只要触发成功，至少这几个字段是确定的。
fn accepted_body(batch_id: &str, kind: JobKind) -> Value {
    json!({
        "batch_id": batch_id,
        "kind": kind.as_str(),
        "status": JobStatus::Running.as_str(),
    })
}

/// 查状态：未知 batch_id、或 batch_id 属于另一种 kind，一律 404。
///
/// 用同一条 404 文案，不区分「不存在」与「种类不对」—— 免得变成探测接口。
fn status_of(state: &AppState, batch_id: &str, kind: JobKind) -> ApiResult<Json<Value>> {
    match state.jobs.get(batch_id) {
        Some(job) if job.kind == kind => Ok(Json(job.to_json())),
        _ => Err(ApiError::not_found(format!(
            "{}任务不存在：{batch_id}",
            kind_label(kind)
        ))),
    }
}

/// 单例锁冲突 → 409（details 里告诉前端是谁占着）。
fn conflict_error(kind: JobKind, active: &AlreadyRunning) -> ApiError {
    ApiError::conflict(format!(
        "已有{}任务正在运行（batch_id={}），请等它结束后再触发",
        kind_label(kind),
        active.batch_id
    ))
    .with_details(json!({
        "batch_id": active.batch_id.clone(),
        "started_at": active.started_at,
    }))
}

/// 任务种类的中文名（错误文案用）。
fn kind_label(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Scan => "扫描",
        JobKind::Scrape => "刮削",
    }
}

/// 扫描回报 → 任务计数。
///
/// 口径：total = 遍历到的文件数；done = 真正落库的（新增 + 更新 + 复原）；
/// skipped = 正常跳过（未变 + 去重）；failed = 三类跳过原因之和。
fn scan_counters(report: &ScanReport) -> Counters {
    Counters {
        total: report.files_seen,
        done: report.added + report.updated + report.restored,
        failed: report.tag_failed + report.io_failed + report.db_failed,
        skipped: report.skipped + report.deduped,
    }
}

/// 刮削批次回报 → 任务计数（字段一一对应）。
fn batch_counters(report: &BatchReport) -> Counters {
    Counters {
        total: report.total,
        done: report.done,
        failed: report.failed,
        skipped: report.skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{header, Request};
    use rusqlite::params;
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::db::migrations;
    use crate::db::models::{Role, User};
    use crate::db::pool::{DbPool, TempDb};
    use crate::server::auth::sign_token_with_ttl;
    use crate::server::routes::build_router;
    use crate::watcher::test_support::TempDir;

    /// 测试用 JWT 密钥。
    const SECRET: &str = "s17-secret";

    /// 单个样本（快用例用）。
    const FIXTURE_ONE: &str = "华夏传说 - 凤凰传奇.mp3";

    /// 并发用例故意用多个大样本：扫描要真的跑上几百毫秒，8 个并发请求才会
    /// 落在同一个「运行中」窗口里，断言才稳定。
    const FIXTURE_SLOW: [&str; 5] = [
        "华夏传说 - 凤凰传奇.mp3",
        "盛夏-毛不易.mp3",
        "最美情侣-白小白.mp3",
        "老男孩-筷子兄弟.mp3",
        "Havana-Camila Cabello&YoungThug-大耳兽莫慢待.mp3",
    ];

    /// 建一个跑完迁移的临时文件库状态（画布指定：DbPool::open_temp）。
    /// 必须持有返回的 TempDb，它一析构就会删库文件。
    fn test_state(tag: &str, roots: Vec<String>) -> (AppState, TempDb) {
        let (pool, temp) = DbPool::open_temp(tag).expect("建临时文件库池");
        {
            let mut guard = pool.acquire().expect("借连接");
            migrations::apply(&mut guard).expect("应用迁移");
        }
        let mut cfg = Config::defaults();
        cfg.server.jwt_secret = SECRET.to_string();
        cfg.storage.library_roots = roots;
        // 测试里不接插件，也不需要节流拖慢用例。
        cfg.scrape.request_delay_ms = 0;
        cfg.scrape.concurrency = 2;
        cfg.scrape.batch_size = 50;
        (AppState::new(Arc::new(pool), Arc::new(cfg)), temp)
    }

    /// 仓库里的真实样本路径。
    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    /// 在临时目录下造一个库根，把给定的样本各拷一份进去，返回 library_roots。
    fn make_root(dir: &TempDir, names: &[&str]) -> Vec<String> {
        let root = dir.path().join("lib");
        std::fs::create_dir_all(&root).expect("建库根");
        for (index, name) in names.iter().enumerate() {
            let target = root.join(format!("track-{index}.mp3"));
            std::fs::copy(fixture(name), &target).expect("拷贝样本");
        }
        vec![root.to_string_lossy().into_owned()]
    }

    /// 预置一个用户并签一个可用令牌（不走注册接口，省掉 argon2 开销）。
    fn issue_token(state: &AppState, username: &str, role: Role) -> String {
        let id = {
            let conn = state.db.acquire().expect("借连接");
            conn.execute(
                "INSERT INTO users (username, password_hash, role, created_at)
                 VALUES (?1, 'hash', ?2, ?3)",
                params![username, role.as_str(), crate::db::now_unix_ms()],
            )
            .expect("预置用户");
            conn.last_insert_rowid()
        };
        let user = User {
            id,
            username: username.to_string(),
            password_hash: "hash".to_string(),
            role,
            created_at: 0,
            last_login: None,
        };
        sign_token_with_ttl(SECRET, &user, 3600).expect("签发令牌")
    }

    fn get(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::empty()).expect("构造请求")
    }

    fn post(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri(uri);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::empty()).expect("构造请求")
    }

    /// tower oneshot 直调 Router（不绑端口），返回状态码 + JSON body。
    async fn call(state: &AppState, request: Request<Body>) -> (StatusCode, Value) {
        let response = build_router(state.clone())
            .oneshot(request)
            .await
            .expect("oneshot");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("读响应体");
        let body = serde_json::from_slice(&bytes).expect("响应体必须是合法 JSON");
        (status, body)
    }

    async fn authed(state: &AppState, token: &str, uri: &str) -> (StatusCode, Value) {
        call(state, get(uri, Some(token))).await
    }

    async fn trigger(state: &AppState, token: &str, uri: &str) -> (StatusCode, Value) {
        call(state, post(uri, Some(token))).await
    }

    /// 带 JSON body 的 POST。Content-Type 必须给，否则 Json 提取器直接 415。
    fn post_json(uri: &str, token: Option<&str>, body: &Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder
            .body(Body::from(body.to_string()))
            .expect("构造请求")
    }

    /// 触发带 body 的刮削。
    async fn trigger_scrape(
        state: &AppState,
        token: &str,
        body: &Value,
    ) -> (StatusCode, Value) {
        call(state, post_json("/api/scrape", Some(token), body)).await
    }

    /// 轮询到任务离开 running 为止：每 20ms 一次，最多等 budget。
    /// 绝不允许无限等待（本机发生过测试挂死）。
    async fn wait_finished(
        state: &AppState,
        token: &str,
        uri: &str,
        budget: Duration,
    ) -> Value {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let (status, body) = authed(state, token, uri).await;
            assert_eq!(status, StatusCode::OK, "轮询必须返回 200：{body}");
            if body["status"] != "running" {
                return body;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "任务在 {budget:?} 内没有结束：{body}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // ── 1. 触发返回 batch_id ───────────────────────────────────────────────

    #[tokio::test]
    async fn trigger_scan_returns_202_with_batch_id() {
        let dir = TempDir::new("s17-scan-trigger");
        let (state, _temp) = test_state("s17-scan-trigger", make_root(&dir, &[FIXTURE_ONE]));
        let token = issue_token(&state, "admin-trigger", Role::Admin);

        let (status, body) = trigger(&state, &token, "/api/scan").await;
        assert_eq!(status, StatusCode::ACCEPTED, "触发必须立刻 202：{body}");
        let batch_id = body["batch_id"].as_str().expect("batch_id 必须是字符串");
        assert!(!batch_id.is_empty(), "batch_id 不能为空");
        assert_eq!(body["kind"], "scan");
        assert_eq!(body["status"], "running");

        let finished = wait_finished(
            &state,
            &token,
            &format!("/api/scan/{batch_id}"),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(finished["status"], "done", "扫描应当正常结束：{finished}");
    }

    // ── 2. 状态字段完整 ───────────────────────────────────────────────────

    #[tokio::test]
    async fn scan_status_carries_every_field() {
        let dir = TempDir::new("s17-scan-fields");
        let (state, _temp) = test_state("s17-scan-fields", make_root(&dir, &[FIXTURE_ONE]));
        let token = issue_token(&state, "admin-fields", Role::Admin);

        let (status, body) = trigger(&state, &token, "/api/scan").await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let batch_id = body["batch_id"].as_str().expect("batch_id").to_string();
        let path = format!("/api/scan/{batch_id}");

        // 逐字段断言：键必须在，且类型正确（finished_at / message 可为 null）。
        let (status, first) = authed(&state, &token, &path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first["batch_id"].as_str(), Some(batch_id.as_str()));
        assert_eq!(first["kind"], "scan");
        assert!(first["status"].is_string(), "status 必须是字符串：{first}");
        assert!(
            first["started_at"].as_i64().unwrap_or(0) > 0,
            "started_at 必须是正的整数时间戳：{first}"
        );
        assert!(
            first.get("finished_at").is_some(),
            "finished_at 字段必须存在（可为 null）"
        );
        assert!(first["total"].is_u64(), "total 必须是非负整数：{first}");
        assert!(first["done"].is_u64(), "done 必须是非负整数：{first}");
        assert!(first["failed"].is_u64(), "failed 必须是非负整数：{first}");
        assert!(first["skipped"].is_u64(), "skipped 必须是非负整数：{first}");
        assert!(
            first.get("message").is_some(),
            "message 字段必须存在（可为 null）"
        );

        let finished = wait_finished(&state, &token, &path, Duration::from_secs(5)).await;
        assert_eq!(finished["status"], "done");
        assert!(
            finished["finished_at"].as_i64().is_some(),
            "跑完后 finished_at 必须非空：{finished}"
        );
        assert!(
            finished["total"].as_u64().unwrap_or(0) >= 1,
            "至少遍历到一个文件：{finished}"
        );
        assert!(
            finished["done"].as_u64().unwrap_or(0) >= 1,
            "新样本应当入库：{finished}"
        );
        assert!(
            finished["message"].is_string(),
            "完成时要带中文统计摘要：{finished}"
        );
    }

    // ── 3. 单例锁：并发触发恰好一个成功 ──────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_scan_triggers_allow_exactly_one() {
        let dir = TempDir::new("s17-scan-concurrent");
        let (state, _temp) = test_state("s17-scan-concurrent", make_root(&dir, &FIXTURE_SLOW));
        let token = issue_token(&state, "admin-concurrent", Role::Admin);

        const N: usize = 8;
        let mut tasks = Vec::with_capacity(N);
        for _ in 0..N {
            let state = state.clone();
            let token = token.clone();
            tasks.push(tokio::spawn(async move {
                let request = post("/api/scan", Some(&token));
                call(&state, request).await
            }));
        }

        let mut accepted: Vec<Value> = Vec::new();
        let mut conflicts = 0usize;
        for task in tasks {
            let (status, body) = task.await.expect("并发请求任务不应 panic");
            match status {
                StatusCode::ACCEPTED => accepted.push(body),
                StatusCode::CONFLICT => {
                    assert_eq!(
                        body["error"]["code"], "CONFLICT",
                        "重复触发必须是 409：{body}"
                    );
                    conflicts += 1;
                }
                other => panic!("并发触发出现意外状态码 {other}：{body}"),
            }
        }
        assert_eq!(accepted.len(), 1, "并发 N 个触发必须恰好一个 202");
        assert_eq!(conflicts, N - 1, "其余必须全是 409");

        // 等这一轮扫描跑完，避免 TempDb 先于后台线程析构。
        let batch_id = accepted[0]["batch_id"].as_str().expect("batch_id").to_string();
        let finished = wait_finished(
            &state,
            &token,
            &format!("/api/scan/{batch_id}"),
            Duration::from_secs(15),
        )
        .await;
        assert_eq!(finished["status"], "done");
    }

    /// 不依赖扫描快慢的确定性版本：直接占住单例锁，两次触发必然一占一拒。
    #[tokio::test]
    async fn duplicate_trigger_while_running_is_409() {
        let dir = TempDir::new("s17-scan-dup");
        let (state, _temp) = test_state("s17-scan-dup", make_root(&dir, &[FIXTURE_ONE]));
        let token = issue_token(&state, "admin-dup", Role::Admin);

        let held = state.jobs.try_start(JobKind::Scan, None).expect("占住扫描锁");

        let (status, body) = trigger(&state, &token, "/api/scan").await;
        assert_eq!(status, StatusCode::CONFLICT, "运行中重复触发必须 409：{body}");
        assert_eq!(body["error"]["code"], "CONFLICT");
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|text| text.contains("扫描")),
            "409 文案要说清是哪类任务：{body}"
        );

        // 占用者退出（Drop 兜底释放）后应当能正常触发。
        drop(held);
        let (accepted, body) = trigger(&state, &token, "/api/scan").await;
        assert_eq!(accepted, StatusCode::ACCEPTED, "锁释放后必须能触发：{body}");
        let batch_id = body["batch_id"].as_str().expect("batch_id").to_string();
        wait_finished(
            &state,
            &token,
            &format!("/api/scan/{batch_id}"),
            Duration::from_secs(5),
        )
        .await;
    }

    // ── 4. 任务失败后能再触发（锁被正确释放）────────────────────────────

    #[tokio::test]
    async fn failed_scan_releases_the_lock() {
        // 没有任何库根 → LibraryService::scan 必然返回 Err(NoRoots)，任务必失败。
        let (state, _temp) = test_state("s17-scan-fail", Vec::new());
        let token = issue_token(&state, "admin-fail", Role::Admin);

        let (status, body) = trigger(&state, &token, "/api/scan").await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let batch_id = body["batch_id"].as_str().expect("batch_id").to_string();
        let failed = wait_finished(
            &state,
            &token,
            &format!("/api/scan/{batch_id}"),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(failed["status"], "failed", "无库根必须整轮失败：{failed}");
        let message = failed["message"].as_str().expect("失败必须带 message");
        assert!(!message.is_empty(), "失败原因不能为空");

        // 关键断言：失败也要把单例锁放掉，否则这类任务只能重启服务。
        let (again, body) = trigger(&state, &token, "/api/scan").await;
        assert_eq!(again, StatusCode::ACCEPTED, "失败后必须能再次触发：{body}");
        let second = body["batch_id"].as_str().expect("batch_id").to_string();
        let second_state = wait_finished(
            &state,
            &token,
            &format!("/api/scan/{second}"),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(second_state["status"], "failed");
    }

    // ── 5. 刮削触发 + 轮询 ────────────────────────────────────────────────

    #[tokio::test]
    async fn scrape_trigger_runs_pending_queue() {
        let dir = TempDir::new("s17-scrape");
        let (state, _temp) = test_state("s17-scrape", make_root(&dir, &[FIXTURE_ONE]));
        let token = issue_token(&state, "admin-scrape", Role::Admin);

        // 先扫描，让库里出现 pending 的歌（刮削队列 = songs.scrape_status='pending'）。
        let (status, scan_body) = trigger(&state, &token, "/api/scan").await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let scan_id = scan_body["batch_id"].as_str().expect("batch_id").to_string();
        let scan_done = wait_finished(
            &state,
            &token,
            &format!("/api/scan/{scan_id}"),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(scan_done["status"], "done");

        let (status, body) = trigger(&state, &token, "/api/scrape").await;
        assert_eq!(status, StatusCode::ACCEPTED, "刮削触发必须 202：{body}");
        let scrape_id = body["batch_id"].as_str().expect("batch_id").to_string();
        assert!(!scrape_id.is_empty());
        assert_eq!(body["kind"], "scrape");
        assert_eq!(body["status"], "running");

        let done = wait_finished(
            &state,
            &token,
            &format!("/api/scrape/{scrape_id}"),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(done["status"], "done", "刮削批次应当正常结束：{done}");
        assert!(
            done["total"].as_u64().unwrap_or(0) >= 1,
            "队列里应有扫描出来的 pending 歌：{done}"
        );
        assert!(done["message"].is_string(), "完成时要带统计摘要：{done}");
    }

    // ── 5b. 重刮：指定曲目 / 失败项（可选请求体）─────────────────────────

    /// 纯解析：字段白名单、类型、边界。老客户端**不发 body** 必须还是 pending 队列。
    #[test]
    fn parse_scrape_body_covers_every_branch() {
        assert_eq!(parse_scrape_body(None).expect("无 body"), ScrapeQueue::Pending);
        assert_eq!(
            parse_scrape_body(Some(&json!({}))).expect("空对象"),
            ScrapeQueue::Pending
        );
        assert_eq!(
            parse_scrape_body(Some(&json!({"mode": "pending"}))).expect("显式 pending"),
            ScrapeQueue::Pending
        );
        assert_eq!(
            parse_scrape_body(Some(&json!({"mode": "failed"}))).expect("失败项"),
            ScrapeQueue::Failed
        );
        assert_eq!(
            parse_scrape_body(Some(&json!({"song_ids": [3, 1]}))).expect("指定曲目"),
            ScrapeQueue::Ids(vec![3, 1])
        );

        // 单曲重刮：这是「已经 done 的歌怎么再刮一遍」的唯一入口
        assert_eq!(
            parse_scrape_body(Some(&json!({"song_ids": [7]}))).expect("单曲"),
            ScrapeQueue::Ids(vec![7])
        );
        // song_ids 更具体，同时给 mode 时以它为准
        assert_eq!(
            parse_scrape_body(Some(&json!({"mode": "failed", "song_ids": [7]}))).expect("id 优先"),
            ScrapeQueue::Ids(vec![7])
        );
        // null 视同没给
        assert_eq!(
            parse_scrape_body(Some(&json!({"mode": null, "song_ids": null}))).expect("null"),
            ScrapeQueue::Pending
        );

        // 以下每条都必须**报错**。刮削会覆盖原文件，拼错字段名静默退化成
        // 「刮全库 pending」的代价太大 —— 宁可 400。
        for bad in [
            json!({"mode": "everything"}),          // 不认识的 mode
            json!({"mode": 1}),                     // 类型不对
            json!({"song_ids": []}),                // 空数组（会变成「什么都没刮」）
            json!({"song_ids": "7"}),               // 不是数组
            json!({"song_ids": [1.5]}),             // 不是整数
            json!({"song_ids": [0]}),               // 非正数
            json!({"song_ids": [-3]}),              // 负数
            json!({"song_id": 7}),                  // 少个 s，最容易犯的拼写错
            json!({"modes": "failed"}),             // 同上
            json!([1, 2, 3]),                       // 顶层不是对象
            json!("failed"),                        // 顶层不是对象
        ] {
            assert!(
                parse_scrape_body(Some(&bad)).is_err(),
                "这个 body 必须 400，不能静默降级：{bad}"
            );
        }
    }

    /// 已 done 的歌**必须**能被显式重刮 —— 在这之前没有任何接口做得到。
    #[tokio::test]
    async fn single_song_can_be_rescraped_after_it_is_done() {
        let dir = TempDir::new("s17-rescrape-one");
        let (state, _temp) = test_state("s17-rescrape-one", make_root(&dir, &[FIXTURE_ONE]));
        let token = issue_token(&state, "admin-rescrape", Role::Admin);

        // 先扫一轮 + 刮一轮 pending，让这首歌走完一次（测试里没接插件 → 落成 failed）。
        let (_, scan_body) = trigger(&state, &token, "/api/scan").await;
        let scan_id = scan_body["batch_id"].as_str().expect("batch_id").to_string();
        wait_finished(
            &state,
            &token,
            &format!("/api/scan/{scan_id}"),
            Duration::from_secs(5),
        )
        .await;
        let (_, first_body) = trigger(&state, &token, "/api/scrape").await;
        let first_id = first_body["batch_id"].as_str().expect("batch_id").to_string();
        let first = wait_finished(
            &state,
            &token,
            &format!("/api/scrape/{first_id}"),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(first["status"], "done");
        assert!(first["total"].as_u64().unwrap_or(0) >= 1, "应刮到歌：{first}");

        // pending 队列现在是空的 —— 这正是「再没有任何接口碰得到这首歌」的那一刻。
        let (_, empty_body) = trigger(&state, &token, "/api/scrape").await;
        let empty_id = empty_body["batch_id"].as_str().expect("batch_id").to_string();
        let empty = wait_finished(
            &state,
            &token,
            &format!("/api/scrape/{empty_id}"),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(empty["total"].as_u64(), Some(0), "pending 队列该空了：{empty}");

        // 拿到歌 id，显式重刮它。
        let (_, lib) = authed(&state, &token, "/api/library").await;
        let song_id = lib["items"][0]["id"].as_i64().expect("歌曲 id");
        let (status, body) = trigger_scrape(&state, &token, &json!({ "song_ids": [song_id] })).await;
        assert_eq!(status, StatusCode::ACCEPTED, "指定曲目重刮必须 202：{body}");
        let again_id = body["batch_id"].as_str().expect("batch_id").to_string();
        let again = wait_finished(
            &state,
            &token,
            &format!("/api/scrape/{again_id}"),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(again["status"], "done", "{again}");
        assert_eq!(
            again["total"].as_u64(),
            Some(1),
            "只该处理点名的这 1 首，不能顺带刮别的：{again}"
        );
    }

    /// `{"mode":"failed"}` 要真的取到失败项（模块文档承诺过，此前没有接口能触发）。
    #[tokio::test]
    async fn failed_queue_is_reachable_through_the_api() {
        let dir = TempDir::new("s17-rescrape-failed");
        let (state, _temp) = test_state("s17-rescrape-failed", make_root(&dir, &[FIXTURE_ONE]));
        let token = issue_token(&state, "admin-failed", Role::Admin);

        let (_, scan_body) = trigger(&state, &token, "/api/scan").await;
        let scan_id = scan_body["batch_id"].as_str().expect("batch_id").to_string();
        wait_finished(
            &state,
            &token,
            &format!("/api/scan/{scan_id}"),
            Duration::from_secs(5),
        )
        .await;
        let (_, first_body) = trigger(&state, &token, "/api/scrape").await;
        let first_id = first_body["batch_id"].as_str().expect("batch_id").to_string();
        wait_finished(
            &state,
            &token,
            &format!("/api/scrape/{first_id}"),
            Duration::from_secs(10),
        )
        .await;

        // 测试环境没有插件，所以歌都是 failed；failed 队列应当能把它们捞回来。
        let (status, body) = trigger_scrape(&state, &token, &json!({ "mode": "failed" })).await;
        assert_eq!(status, StatusCode::ACCEPTED, "failed 队列必须能触发：{body}");
        let retry_id = body["batch_id"].as_str().expect("batch_id").to_string();
        let retry = wait_finished(
            &state,
            &token,
            &format!("/api/scrape/{retry_id}"),
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(retry["status"], "done");
        assert!(
            retry["total"].as_u64().unwrap_or(0) >= 1,
            "失败项应当被重新取到：{retry}"
        );
    }

    /// 非法 body → 400（而不是静默刮全库）。
    #[tokio::test]
    async fn bad_scrape_body_is_400_not_a_full_library_scrape() {
        let dir = TempDir::new("s17-scrape-badbody");
        let (state, _temp) = test_state("s17-scrape-badbody", make_root(&dir, &[FIXTURE_ONE]));
        let token = issue_token(&state, "admin-badbody", Role::Admin);

        for bad in [json!({"mode": "everything"}), json!({"song_ids": []})] {
            let (status, body) = trigger_scrape(&state, &token, &bad).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} 应当 400：{body}");
            assert_eq!(body["error"]["code"], "BAD_REQUEST");
        }
        // 400 之后单例锁必须没被占住，否则刮削就永久锁死了。
        let (status, _) = trigger(&state, &token, "/api/scrape").await;
        assert_eq!(status, StatusCode::ACCEPTED, "报错不该占住刮削锁");
    }

    // ── 6. 未知 / 种类不符的 batch_id → 404 ──────────────────────────────
    #[tokio::test]
    async fn unknown_batch_id_is_404() {
        let (state, _temp) = test_state("s17-unknown", vec!["/tmp/s17-unknown-music".to_string()]);
        let token = issue_token(&state, "admin-unknown", Role::Admin);

        for uri in ["/api/scan/no-such-batch", "/api/scrape/no-such-batch"] {
            let (status, body) = authed(&state, &token, uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri} 应当 404：{body}");
            assert_eq!(body["error"]["code"], "NOT_FOUND");
        }
    }

    #[tokio::test]
    async fn batch_id_of_other_kind_is_404() {
        let dir = TempDir::new("s17-kind-mismatch");
        let (state, _temp) = test_state("s17-kind-mismatch", make_root(&dir, &[FIXTURE_ONE]));
        let token = issue_token(&state, "admin-kind", Role::Admin);

        let (status, body) = trigger(&state, &token, "/api/scan").await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let scan_id = body["batch_id"].as_str().expect("batch_id").to_string();

        // 拿扫描的 batch_id 去刮削的状态接口查 → 404（不泄漏「这个 id 存在」）
        let (mismatch, body) = authed(&state, &token, &format!("/api/scrape/{scan_id}")).await;
        assert_eq!(mismatch, StatusCode::NOT_FOUND, "别的 kind 的 id 必须 404");
        assert_eq!(body["error"]["code"], "NOT_FOUND");

        // 用对的 kind 查就能拿到
        let (ok, _) = authed(&state, &token, &format!("/api/scan/{scan_id}")).await;
        assert_eq!(ok, StatusCode::OK);

        wait_finished(
            &state,
            &token,
            &format!("/api/scan/{scan_id}"),
            Duration::from_secs(5),
        )
        .await;
    }

    // ── 7. GET /api/jobs ──────────────────────────────────────────────────

    #[tokio::test]
    async fn jobs_list_contains_started_batch() {
        let dir = TempDir::new("s17-jobs-list");
        let (state, _temp) = test_state("s17-jobs-list", make_root(&dir, &[FIXTURE_ONE]));
        let token = issue_token(&state, "admin-list", Role::Admin);

        let (status, body) = trigger(&state, &token, "/api/scan").await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let batch_id = body["batch_id"].as_str().expect("batch_id").to_string();

        let (status, list) = authed(&state, &token, "/api/jobs").await;
        assert_eq!(status, StatusCode::OK);
        let items = list["items"].as_array().expect("items 是数组");
        assert!(
            items.iter().any(|item| item["batch_id"] == batch_id.as_str()),
            "任务列表里应包含刚触发的 batch：{list}"
        );
        assert_eq!(list["count"].as_u64(), Some(items.len() as u64));

        wait_finished(
            &state,
            &token,
            &format!("/api/scan/{batch_id}"),
            Duration::from_secs(5),
        )
        .await;
    }

    // ── 8. 鉴权：未登录 401 / 普通用户 403（admin 专属）──────────────────

    #[tokio::test]
    async fn anonymous_requests_are_401() {
        let (state, _temp) = test_state("s17-401", vec!["/tmp/s17-401-music".to_string()]);

        let cases = [
            post("/api/scan", None),
            post("/api/scrape", None),
            get("/api/jobs", None),
            get("/api/scan/some-id", None),
            get("/api/scrape/some-id", None),
        ];
        for request in cases {
            let uri = request.uri().to_string();
            let (status, body) = call(&state, request).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} 未登录必须 401");
            assert_eq!(body["error"]["code"], "UNAUTHORIZED");
        }
    }

    #[tokio::test]
    async fn regular_user_is_403_on_job_routes() {
        let (state, _temp) = test_state("s17-403", vec!["/tmp/s17-403-music".to_string()]);
        let token = issue_token(&state, "bob", Role::User);

        let cases = [
            post("/api/scan", Some(&token)),
            post("/api/scrape", Some(&token)),
            get("/api/jobs", Some(&token)),
            get("/api/scan/some-id", Some(&token)),
            get("/api/scrape/some-id", Some(&token)),
        ];
        for request in cases {
            let uri = request.uri().to_string();
            let (status, body) = call(&state, request).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{uri} 普通用户必须 403：{body}"
            );
            assert_eq!(body["error"]["code"], "FORBIDDEN");
        }
    }
}
