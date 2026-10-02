//! S10 · 插件协议类型 —— 主服务 ↔ 插件进程的 stdin/stdout JSON 契约。
//!
//! 两条铁律：
//!   1. daemon 模式下一行一个 JSON：encode_request / encode_response 的输出保证不含换行
//!      （JSON 字符串里的换行会被转义成 \n），也**不带**行尾换行符；
//!   2. **字段缺省 = 不修改，显式 null = 清空**：用 FieldUpdate / TagValue 三态保真。
//!
//! 设计取舍：
//!   * 请求结构体不带 protocol 字段 —— 解码时校验必须等于 1，编码时恒写 1，冗余字段无意义；
//!   * 响应**必须回显 action**，类型判别只依据它
//!     （scrape / download / download_mv；download_mv 成功复用 DownloadOk，靠 action 区分）；
//!   * 数值字段接受 JSON 数字，也接受数字字符串（shell 拼 JSON 时常见），其余类型报错；
//!   * tags 整体缺省或为 null = 这一条响应不修改任何标签；要清空请把**单个字段**写成 null。

use std::path::{Component, Path};

use serde_json::{Map, Value};

use super::error::ProtocolError;

/// 本服务实现的协议版本
pub const PROTOCOL_VERSION: u32 = 1;
/// confidence 缺省值
pub const DEFAULT_CONFIDENCE: f64 = 0.50;

/// 字段更新三态：缺省不修改 / 显式 null 清空 / 有值写入。
#[derive(Debug, Clone, PartialEq)]
pub enum FieldUpdate<T> {
    /// 字段缺省 —— 保持原值不动
    Absent,
    /// 显式 null —— 清空该字段
    Clear,
    /// 有值 —— 写入
    Set(T),
}

// 手写 Default：derive 会给 T 加上多余的 Default 约束
impl<T> Default for FieldUpdate<T> {
    fn default() -> Self { FieldUpdate::Absent }
}

/// 单个标签字段的更新指令（字符串或整数）
#[derive(Debug, Clone, Default, PartialEq)]
pub enum TagValue {
    /// 字段缺省 —— 不修改
    #[default]
    Absent,
    /// 显式 null —— 清空
    Clear,
    /// 字符串值（title / artist / album / year / genre…）
    Text(String),
    /// 整数值（track 等）
    Number(i64),
}

/// 响应里 tags 支持更新的字段集合。缺省即「不修改」。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tags {
    pub title: TagValue,
    pub artist: TagValue,
    pub album: TagValue,
    pub year: TagValue,
    pub genre: TagValue,
    pub track: TagValue,
}

/// 歌曲描述（请求侧）。字段全可选：抓取请求可能只有 file_path，下载请求只有 title/artist。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SongRef {
    pub file_path: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
    pub audio_hash: Option<String>,
    pub fingerprint: Option<String>,
}

/// 请求侧通用选项
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RequestOptions {
    pub timeout_ms: Option<u64>,
}

/// scrape 请求
#[derive(Debug, Clone, PartialEq)]
pub struct ScrapeRequest {
    pub id: String,
    pub song: SongRef,
    /// 插件可写目录（cover.path 相对于它解析）
    pub work_dir: String,
    /// 期望插件回填的字段名列表
    pub want: Vec<String>,
    pub options: RequestOptions,
}

/// download 请求的偏好
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DownloadPrefer {
    pub format: Option<String>,
    pub bitrate: Option<u32>,
}

/// download_mv 请求的偏好
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MvPrefer {
    pub format: Option<String>,
    pub max_height: Option<u32>,
}

/// download 请求
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadRequest {
    pub id: String,
    pub song: SongRef,
    pub target_dir: String,
    pub prefer: DownloadPrefer,
    pub options: RequestOptions,
}

/// download_mv 请求
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadMvRequest {
    pub id: String,
    pub song: SongRef,
    pub target_dir: String,
    pub prefer: MvPrefer,
    pub options: RequestOptions,
}

/// 三种请求
#[derive(Debug, Clone, PartialEq)]
pub enum PluginRequest {
    Scrape(ScrapeRequest),
    Download(DownloadRequest),
    DownloadMv(DownloadMvRequest),
}

impl PluginRequest {
    /// 请求 id（daemon 用它把响应配回任务）
    pub fn id(&self) -> &str {
        match self {
            PluginRequest::Scrape(r) => &r.id,
            PluginRequest::Download(r) => &r.id,
            PluginRequest::DownloadMv(r) => &r.id,
        }
    }

    /// action 名（与 JSON 里的 action 字段一致）
    pub fn action(&self) -> &'static str {
        match self {
            PluginRequest::Scrape(_) => "scrape",
            PluginRequest::Download(_) => "download",
            PluginRequest::DownloadMv(_) => "download_mv",
        }
    }
}

/// 响应动作 —— 响应必须回显请求的 action，类型判别**只**依据它。
///
/// 修复：旧实现靠「有没有 file_path」猜响应类型，刮削插件一旦回显
/// song.file_path 就会被误判成下载结果，confidence / tags / cover / lyrics
/// 全部被静默丢弃。现在 action 是必填字段，缺失即协议错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseAction {
    Scrape,
    Download,
    DownloadMv,
}

impl ResponseAction {
    /// JSON 里的字面量
    pub fn as_str(&self) -> &'static str {
        match self {
            ResponseAction::Scrape => "scrape",
            ResponseAction::Download => "download",
            ResponseAction::DownloadMv => "download_mv",
        }
    }

    /// 严格字面量解析（大小写敏感，不做 trim）
    pub fn parse(s: &str) -> Option<ResponseAction> {
        match s {
            "scrape" => Some(ResponseAction::Scrape),
            "download" => Some(ResponseAction::Download),
            "download_mv" => Some(ResponseAction::DownloadMv),
            _ => None,
        }
    }
}

/// 匹配到的曲目信息（信息性字段，无「清空」语义）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackMatch {
    pub id: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub url: Option<String>,
}

/// 封面引用。path 必须是相对 work_dir 的安全相对路径。
#[derive(Debug, Clone, PartialEq)]
pub struct CoverRef {
    pub path: String,
    pub mime: Option<String>,
}

/// 插件对一首歌给出的**一条**候选理解。
///
/// 为什么是「候选」而不是「结果」：同一个歌名在数据源里往往对应多条录音
/// （原版 / 现场 / 重混 / 翻唱），它们 title 一模一样，只有发行信息不同。
/// 要说清「哪条才是用户要的那首」得懂数据源的结构（哪些 release 是合辑、
/// 哪个是原版），那是插件的地盘 —— 所以**筛选交回插件**，
/// 服务端只负责「按 confidence 取最高」这类通用的事。
#[derive(Debug, Clone, PartialEq)]
pub struct ScrapeCandidate {
    /// 缺省 [DEFAULT_CONFIDENCE]
    pub confidence: f64,
    pub source: Option<String>,
    pub matched: Option<TrackMatch>,
    pub tags: Tags,
    pub cover: FieldUpdate<CoverRef>,
    pub lyrics: FieldUpdate<String>,
}

/// scrape 成功响应。
///
/// `candidates` **按 confidence 降序**（[ScrapeOk::new] 里排好、对外只读）——
/// 「哪条最可信」是服务端的判断，不依赖插件给的顺序。
#[derive(Debug, Clone, PartialEq)]
pub struct ScrapeOk {
    pub id: String,
    candidates: Vec<ScrapeCandidate>,
}

impl ScrapeOk {
    /// 构造：**排一次序**，把「按 confidence 降序」这个不变式钉在类型里。
    ///
    /// 用**稳定**排序：confidence 相同时保留插件给的先后（插件通常把更可信的放前面）。
    pub fn new(id: String, mut candidates: Vec<ScrapeCandidate>) -> ScrapeOk {
        candidates.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        ScrapeOk { id, candidates }
    }

    /// 全部候选，按 confidence 降序。
    pub fn ranked(&self) -> &[ScrapeCandidate] {
        &self.candidates
    }

    /// 最可信的那条。
    ///
    /// `None` 只可能来自「构造时传了空列表」；解码路径会明确拒掉空 `candidates`，
    /// 所以正常流程里至少有一条。
    pub fn best(&self) -> Option<&ScrapeCandidate> {
        self.candidates.first()
    }
}

/// download / download_mv 成功响应
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadOk {
    pub id: String,
    /// download / download_mv 成功复用本结构，靠它区分二者
    pub action: ResponseAction,
    pub file_path: String,
    pub format: Option<String>,
    pub bitrate: Option<u32>,
    pub size: Option<u64>,
    pub duration_ms: Option<i64>,
    /// 缺省 false
    pub tags_written: bool,
}

/// 协议错误码（固定枚举，未知码直接判协议错误）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    NotFound,
    RateLimited,
    AuthFailed,
    Network,
    Timeout,
    BadRequest,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::AuthFailed => "AUTH_FAILED",
            ErrorCode::Network => "NETWORK",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::BadRequest => "BAD_REQUEST",
            ErrorCode::Internal => "INTERNAL",
        }
    }

    pub fn parse(s: &str) -> Option<ErrorCode> {
        match s {
            "NOT_FOUND" => Some(ErrorCode::NotFound),
            "RATE_LIMITED" => Some(ErrorCode::RateLimited),
            "AUTH_FAILED" => Some(ErrorCode::AuthFailed),
            "NETWORK" => Some(ErrorCode::Network),
            "TIMEOUT" => Some(ErrorCode::Timeout),
            "BAD_REQUEST" => Some(ErrorCode::BadRequest),
            "INTERNAL" => Some(ErrorCode::Internal),
            _ => None,
        }
    }
}

/// ok:false 时的 error 对象
#[derive(Debug, Clone, PartialEq)]
pub struct PluginErrorInfo {
    pub code: ErrorCode,
    pub message: String,
    /// 缺省 false
    pub retryable: bool,
}

/// ok:false 响应
#[derive(Debug, Clone, PartialEq)]
pub struct PluginErrorResponse {
    pub id: String,
    /// 错误响应同样必须回显 action
    pub action: ResponseAction,
    pub error: PluginErrorInfo,
}

/// 三种响应（download_mv 成功复用 DownloadOk）
#[derive(Debug, Clone, PartialEq)]
pub enum PluginResponse {
    ScrapeOk(ScrapeOk),
    DownloadOk(DownloadOk),
    Error(PluginErrorResponse),
}

impl PluginResponse {
    /// 响应对应的 action（DownloadOk / Error 由自身 action 字段决定）
    pub fn action(&self) -> ResponseAction {
        match self {
            PluginResponse::ScrapeOk(_) => ResponseAction::Scrape,
            PluginResponse::DownloadOk(r) => r.action,
            PluginResponse::Error(r) => r.action,
        }
    }

    /// 响应 id（daemon 用它把响应配回任务）
    pub fn id(&self) -> &str {
        match self {
            PluginResponse::ScrapeOk(r) => &r.id,
            PluginResponse::DownloadOk(r) => &r.id,
            PluginResponse::Error(r) => &r.id,
        }
    }
}

// ─────────────────────────── 编码 ───────────────────────────

/// 把请求编码成**单行** JSON（不含换行，也不带行尾换行符）。
pub fn encode_request(req: &PluginRequest) -> String {
    let mut m = Map::new();
    m.insert("id".into(), Value::String(req.id().to_string()));
    m.insert("protocol".into(), Value::from(PROTOCOL_VERSION));
    m.insert("action".into(), Value::from(req.action()));
    match req {
        PluginRequest::Scrape(r) => {
            m.insert("song".into(), song_to_value(&r.song));
            m.insert("work_dir".into(), Value::String(r.work_dir.clone()));
            m.insert("want".into(), str_array(&r.want));
            m.insert("options".into(), options_to_value(&r.options));
        }
        PluginRequest::Download(r) => {
            m.insert("song".into(), song_to_value(&r.song));
            m.insert("target_dir".into(), Value::String(r.target_dir.clone()));
            let mut p = Map::new();
            p.insert("format".into(), opt_value(&r.prefer.format));
            p.insert("bitrate".into(), opt_value(&r.prefer.bitrate));
            m.insert("prefer".into(), Value::Object(p));
            m.insert("options".into(), options_to_value(&r.options));
        }
        PluginRequest::DownloadMv(r) => {
            m.insert("song".into(), song_to_value(&r.song));
            m.insert("target_dir".into(), Value::String(r.target_dir.clone()));
            let mut p = Map::new();
            p.insert("format".into(), opt_value(&r.prefer.format));
            p.insert("max_height".into(), opt_value(&r.prefer.max_height));
            m.insert("prefer".into(), Value::Object(p));
            m.insert("options".into(), options_to_value(&r.options));
        }
    }
    Value::Object(m).to_string()
}

/// 把响应编码成**单行** JSON（不含换行，也不带行尾换行符）。
pub fn encode_response(resp: &PluginResponse) -> String {
    match resp {
        PluginResponse::ScrapeOk(r) => {
            let mut m = Map::new();
            m.insert("id".into(), Value::String(r.id.clone()));
            m.insert("protocol".into(), Value::from(PROTOCOL_VERSION));
            m.insert("ok".into(), Value::Bool(true));
            m.insert("action".into(), Value::from(ResponseAction::Scrape.as_str()));
            m.insert(
                "candidates".into(),
                Value::Array(r.ranked().iter().map(candidate_to_value).collect()),
            );
            Value::Object(m).to_string()
        }
        PluginResponse::DownloadOk(r) => {
            let mut m = Map::new();
            m.insert("id".into(), Value::String(r.id.clone()));
            m.insert("protocol".into(), Value::from(PROTOCOL_VERSION));
            m.insert("ok".into(), Value::Bool(true));
            m.insert("action".into(), Value::from(r.action.as_str()));
            m.insert("file_path".into(), Value::String(r.file_path.clone()));
            m.insert("format".into(), opt_value(&r.format));
            m.insert("bitrate".into(), opt_value(&r.bitrate));
            m.insert("size".into(), opt_value(&r.size));
            m.insert("duration_ms".into(), opt_value(&r.duration_ms));
            m.insert("tags_written".into(), Value::Bool(r.tags_written));
            Value::Object(m).to_string()
        }
        PluginResponse::Error(r) => {
            let mut e = Map::new();
            e.insert("code".into(), Value::from(r.error.code.as_str()));
            e.insert("message".into(), Value::String(r.error.message.clone()));
            e.insert("retryable".into(), Value::Bool(r.error.retryable));
            let mut m = Map::new();
            m.insert("id".into(), Value::String(r.id.clone()));
            m.insert("protocol".into(), Value::from(PROTOCOL_VERSION));
            m.insert("ok".into(), Value::Bool(false));
            m.insert("action".into(), Value::from(r.action.as_str()));
            m.insert("error".into(), Value::Object(e));
            Value::Object(m).to_string()
        }
    }
}

// ─────────────────────────── 解码 ───────────────────────────

/// 解码请求。协议版本必须等于 1。
pub fn decode_request(line: &str) -> Result<PluginRequest, ProtocolError> {
    let root = parse_value(line)?;
    let obj = as_object(&root, "请求根节点")?;
    let id = require_str(obj, "id")?;
    require_protocol(obj)?;
    let action = require_str(obj, "action")?;
    match action.as_str() {
        "scrape" => Ok(PluginRequest::Scrape(ScrapeRequest {
            id,
            song: decode_song(obj.get("song"))?,
            work_dir: require_str(obj, "work_dir")?,
            want: decode_str_array(obj.get("want")),
            options: decode_options(obj.get("options"))?,
        })),
        "download" => Ok(PluginRequest::Download(DownloadRequest {
            id,
            song: decode_song(obj.get("song"))?,
            target_dir: require_str(obj, "target_dir")?,
            prefer: decode_download_prefer(obj.get("prefer"))?,
            options: decode_options(obj.get("options"))?,
        })),
        "download_mv" => Ok(PluginRequest::DownloadMv(DownloadMvRequest {
            id,
            song: decode_song(obj.get("song"))?,
            target_dir: require_str(obj, "target_dir")?,
            prefer: decode_mv_prefer(obj.get("prefer"))?,
            options: decode_options(obj.get("options"))?,
        })),
        other => Err(ProtocolError::BadJson(format!("未知 action：{other}"))),
    }
}

/// 解一条候选。
///
/// 字段可以平铺在响应根上（协议 v1 的旧形状），也可以在 `candidates` 数组的元素里 ——
/// 两种形状走**同一个**解码函数，免得两条路径慢慢漂移。
fn decode_candidate(obj: &Map<String, Value>) -> Result<ScrapeCandidate, ProtocolError> {
    Ok(ScrapeCandidate {
        confidence: match obj.get("confidence") {
            None | Some(Value::Null) => DEFAULT_CONFIDENCE,
            Some(v) => value_to_f64(v)
                .ok_or_else(|| ProtocolError::BadJson("\"confidence\" 必须是数字".to_string()))?,
        },
        source: opt_str(obj, "source"),
        matched: decode_matched(obj.get("matched"))?,
        tags: decode_tags(obj.get("tags"))?,
        cover: decode_cover(obj.get("cover"))?,
        lyrics: decode_field(obj, "lyrics", |v| v.as_str().map(str::to_string))?,
    })
}

/// 读取响应必填的 action 字段，只接受三个字面量（大小写敏感）。
fn decode_response_action(obj: &Map<String, Value>) -> Result<ResponseAction, ProtocolError> {
    let raw = match obj.get("action") {
        Some(Value::String(s)) if !s.is_empty() => s.as_str(),
        // 缺省 / null / 空串都按「缺字段」处理，非字符串则是类型错误
        None | Some(Value::Null) | Some(Value::String(_)) => {
            return Err(ProtocolError::MissingField("action"))
        }
        Some(_) => return Err(ProtocolError::BadJson("\"action\" 必须是字符串".to_string())),
    };
    ResponseAction::parse(raw)
        .ok_or_else(|| ProtocolError::BadJson(format!("未知 action：{raw}")))
}

/// 解码响应。协议版本必须等于 1；action 为必填字段，类型判别**只**依据它。
pub fn decode_response(line: &str) -> Result<PluginResponse, ProtocolError> {
    let root = parse_value(line)?;
    let obj = as_object(&root, "响应根节点")?;
    let id = require_str(obj, "id")?;
    require_protocol(obj)?;
    let action = decode_response_action(obj)?;
    let ok = match obj.get("ok") {
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(ProtocolError::BadJson("\"ok\" 必须是布尔值".to_string())),
        None => return Err(ProtocolError::MissingField("ok")),
    };
    if !ok {
        return Ok(PluginResponse::Error(PluginErrorResponse {
            id,
            action,
            error: decode_error_info(obj)?,
        }));
    }
    match action {
        // 即使响应里带了 file_path（回显请求的 song.file_path）也仍按 scrape 解析
        ResponseAction::Scrape => {
            let candidates = match obj.get("candidates") {
                // 新形状：一次给多条候选
                Some(Value::Array(items)) => {
                    if items.is_empty() {
                        return Err(ProtocolError::BadJson(
                            "ok:true 但 candidates 是空数组 —— 没有候选就该回 ok:false + NOT_FOUND"
                                .to_string(),
                        ));
                    }
                    items
                        .iter()
                        .map(|item| {
                            let o = as_object(item, "candidates 元素")?;
                            decode_candidate(o)
                        })
                        .collect::<Result<Vec<_>, _>>()?
                }
                Some(_) => {
                    return Err(ProtocolError::BadJson(
                        "\"candidates\" 必须是数组".to_string(),
                    ))
                }
                // 旧形状（协议 v1 的原始形状）：字段直接平铺在响应根上。
                // 等价于「只有一条候选」，保留它是为了不把已发布的插件一次性打死。
                None => vec![decode_candidate(obj)?],
            };
            Ok(PluginResponse::ScrapeOk(ScrapeOk::new(id, candidates)))
        }
        // download_mv 成功复用 DownloadOk，靠 action 字段区分二者
        ResponseAction::Download | ResponseAction::DownloadMv => {
            Ok(PluginResponse::DownloadOk(DownloadOk {
                id,
                action,
                file_path: require_str(obj, "file_path")?,
                format: opt_str(obj, "format"),
                bitrate: opt_u32(obj, "bitrate")?,
                size: opt_u64(obj, "size")?,
                duration_ms: opt_i64(obj, "duration_ms")?,
                tags_written: obj.get("tags_written").and_then(Value::as_bool).unwrap_or(false),
            }))
        }
    }
}

/// 校验 cover.path：必须是相对 work_dir 的安全相对路径。
///
/// 拒绝：空串、绝对路径、Windows 盘符、反斜杠开头、任何 .. 组件、含 NUL 的串。
/// 允许：cover.jpg、sub/cover.jpg、./cover.jpg。
pub fn validate_cover_path(path: &str) -> Result<(), ProtocolError> {
    let bad = || ProtocolError::BadCoverPath(path.to_string());
    if path.is_empty() || path.contains('\0') {
        return Err(bad());
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(bad());
    }
    // Windows 盘符（C:\... / C:/...）；本工具跑在 NAS 上，这里是防火墙不是必需品
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(bad());
    }
    for comp in Path::new(path).components() {
        match comp {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return Err(bad()),
            _ => {}
        }
    }
    Ok(())
}

// ─────────────────────── 解码：内部小工具 ───────────────────────

fn parse_value(line: &str) -> Result<Value, ProtocolError> {
    serde_json::from_str(line).map_err(|e| ProtocolError::BadJson(e.to_string()))
}

fn as_object<'a>(v: &'a Value, what: &str) -> Result<&'a Map<String, Value>, ProtocolError> {
    v.as_object()
        .ok_or_else(|| ProtocolError::BadJson(format!("{what}必须是 JSON 对象")))
}

fn require_str(obj: &Map<String, Value>, key: &'static str) -> Result<String, ProtocolError> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or(ProtocolError::MissingField(key))
}

fn require_protocol(obj: &Map<String, Value>) -> Result<u32, ProtocolError> {
    let p = obj
        .get("protocol")
        .and_then(value_to_u32)
        .ok_or(ProtocolError::MissingField("protocol"))?;
    if p != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedProtocol(p));
    }
    Ok(p)
}

fn opt_str(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).and_then(Value::as_str).map(str::to_string)
}

fn value_to_u32(v: &Value) -> Option<u32> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|x| u32::try_from(x).ok()),
        Value::String(s) => s.trim().parse::<u32>().ok(),
        _ => None,
    }
}

fn value_to_u64(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

fn value_to_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn opt_u32(obj: &Map<String, Value>, key: &str) -> Result<Option<u32>, ProtocolError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => value_to_u32(v)
            .map(Some)
            .ok_or_else(|| ProtocolError::BadJson(format!("字段 {key} 必须是非负整数"))),
    }
}

fn opt_u64(obj: &Map<String, Value>, key: &str) -> Result<Option<u64>, ProtocolError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => value_to_u64(v)
            .map(Some)
            .ok_or_else(|| ProtocolError::BadJson(format!("字段 {key} 必须是非负整数"))),
    }
}

fn opt_i64(obj: &Map<String, Value>, key: &str) -> Result<Option<i64>, ProtocolError> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => value_to_i64(v)
            .map(Some)
            .ok_or_else(|| ProtocolError::BadJson(format!("字段 {key} 必须是整数"))),
    }
}

fn decode_song(v: Option<&Value>) -> Result<SongRef, ProtocolError> {
    let Some(v) = v else { return Ok(SongRef::default()) };
    if v.is_null() {
        return Ok(SongRef::default());
    }
    let o = as_object(v, "\"song\"")?;
    Ok(SongRef {
        file_path: opt_str(o, "file_path"),
        title: opt_str(o, "title"),
        artist: opt_str(o, "artist"),
        album: opt_str(o, "album"),
        duration_ms: opt_i64(o, "duration_ms")?,
        audio_hash: opt_str(o, "audio_hash"),
        fingerprint: opt_str(o, "fingerprint"),
    })
}

fn decode_str_array(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

fn decode_options(v: Option<&Value>) -> Result<RequestOptions, ProtocolError> {
    let Some(v) = v else { return Ok(RequestOptions::default()) };
    if v.is_null() {
        return Ok(RequestOptions::default());
    }
    let o = as_object(v, "\"options\"")?;
    Ok(RequestOptions { timeout_ms: opt_u64(o, "timeout_ms")? })
}

fn decode_download_prefer(v: Option<&Value>) -> Result<DownloadPrefer, ProtocolError> {
    let Some(v) = v else { return Ok(DownloadPrefer::default()) };
    if v.is_null() {
        return Ok(DownloadPrefer::default());
    }
    let o = as_object(v, "\"prefer\"")?;
    Ok(DownloadPrefer { format: opt_str(o, "format"), bitrate: opt_u32(o, "bitrate")? })
}

fn decode_mv_prefer(v: Option<&Value>) -> Result<MvPrefer, ProtocolError> {
    let Some(v) = v else { return Ok(MvPrefer::default()) };
    if v.is_null() {
        return Ok(MvPrefer::default());
    }
    let o = as_object(v, "\"prefer\"")?;
    Ok(MvPrefer { format: opt_str(o, "format"), max_height: opt_u32(o, "max_height")? })
}

fn decode_error_info(obj: &Map<String, Value>) -> Result<PluginErrorInfo, ProtocolError> {
    let e = obj
        .get("error")
        .and_then(Value::as_object)
        .ok_or(ProtocolError::MissingField("error"))?;
    let raw = e
        .get("code")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("error.code"))?;
    let code = ErrorCode::parse(raw)
        .ok_or_else(|| ProtocolError::BadJson(format!("未知错误码：{raw}")))?;
    let message = e
        .get("message")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("error.message"))?
        .to_string();
    Ok(PluginErrorInfo {
        code,
        message,
        retryable: e.get("retryable").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn decode_matched(v: Option<&Value>) -> Result<Option<TrackMatch>, ProtocolError> {
    let Some(v) = v else { return Ok(None) };
    if v.is_null() {
        return Ok(None);
    }
    let o = as_object(v, "\"matched\"")?;
    Ok(Some(TrackMatch {
        id: opt_str(o, "id"),
        title: opt_str(o, "title"),
        artist: opt_str(o, "artist"),
        url: opt_str(o, "url"),
    }))
}

fn decode_tags(v: Option<&Value>) -> Result<Tags, ProtocolError> {
    let Some(v) = v else { return Ok(Tags::default()) };
    // tags 整体为 null = 这一条响应不修改任何标签（要清空请写单个字段的 null）
    if v.is_null() {
        return Ok(Tags::default());
    }
    let o = as_object(v, "\"tags\"")?;
    Ok(Tags {
        title: decode_tag_field(o, "title")?,
        artist: decode_tag_field(o, "artist")?,
        album: decode_tag_field(o, "album")?,
        year: decode_tag_field(o, "year")?,
        genre: decode_tag_field(o, "genre")?,
        track: decode_tag_field(o, "track")?,
    })
}

/// 单个标签字段的三态解码：缺省 / null / 值
fn decode_tag_field(o: &Map<String, Value>, key: &str) -> Result<TagValue, ProtocolError> {
    match o.get(key) {
        None => Ok(TagValue::Absent),
        Some(Value::Null) => Ok(TagValue::Clear),
        Some(Value::String(s)) => Ok(TagValue::Text(s.clone())),
        Some(Value::Number(n)) => n
            .as_i64()
            .map(TagValue::Number)
            .ok_or_else(|| ProtocolError::BadJson(format!("tags.{key} 的数值超出整数范围"))),
        Some(_) => Err(ProtocolError::BadJson(format!(
            "tags.{key} 必须是字符串、整数或 null"
        ))),
    }
}

fn decode_cover(v: Option<&Value>) -> Result<FieldUpdate<CoverRef>, ProtocolError> {
    let Some(v) = v else { return Ok(FieldUpdate::Absent) };
    if v.is_null() {
        return Ok(FieldUpdate::Clear);
    }
    let o = as_object(v, "\"cover\"")?;
    let path = o
        .get("path")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingField("cover.path"))?;
    validate_cover_path(path)?;
    Ok(FieldUpdate::Set(CoverRef { path: path.to_string(), mime: opt_str(o, "mime") }))
}

/// 通用三态字段解码（lyrics 等）
fn decode_field<T>(
    obj: &Map<String, Value>,
    key: &str,
    f: impl Fn(&Value) -> Option<T>,
) -> Result<FieldUpdate<T>, ProtocolError> {
    match obj.get(key) {
        None => Ok(FieldUpdate::Absent),
        Some(Value::Null) => Ok(FieldUpdate::Clear),
        Some(v) => f(v)
            .map(FieldUpdate::Set)
            .ok_or_else(|| ProtocolError::BadJson(format!("字段 {key} 类型非法"))),
    }
}

// ─────────────────────── 编码：内部小工具 ───────────────────────

fn opt_value<T: Clone + Into<Value>>(v: &Option<T>) -> Value {
    match v {
        Some(x) => x.clone().into(),
        None => Value::Null,
    }
}

fn str_array(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::String(s.clone())).collect())
}

fn song_to_value(s: &SongRef) -> Value {
    let mut m = Map::new();
    m.insert("file_path".into(), opt_value(&s.file_path));
    m.insert("title".into(), opt_value(&s.title));
    m.insert("artist".into(), opt_value(&s.artist));
    m.insert("album".into(), opt_value(&s.album));
    m.insert("duration_ms".into(), opt_value(&s.duration_ms));
    m.insert("audio_hash".into(), opt_value(&s.audio_hash));
    m.insert("fingerprint".into(), opt_value(&s.fingerprint));
    Value::Object(m)
}

fn options_to_value(o: &RequestOptions) -> Value {
    let mut m = Map::new();
    m.insert("timeout_ms".into(), opt_value(&o.timeout_ms));
    Value::Object(m)
}

/// 一条候选 → JSON 对象（`candidates` 数组的元素）。
///
/// `cover` / `lyrics` 是三态：**缺省不写键**，显式 `Clear` 才写 `null` ——
/// 「不修改」与「清空」在线上必须分得开。
fn candidate_to_value(c: &ScrapeCandidate) -> Value {
    let mut m = Map::new();
    m.insert("confidence".into(), Value::from(c.confidence));
    m.insert("source".into(), opt_value(&c.source));
    m.insert("matched".into(), matched_to_value(&c.matched));
    m.insert("tags".into(), tags_to_value(&c.tags));
    match &c.cover {
        FieldUpdate::Absent => {}
        FieldUpdate::Clear => { m.insert("cover".into(), Value::Null); }
        FieldUpdate::Set(cover) => { m.insert("cover".into(), cover_to_value(cover)); }
    }
    match &c.lyrics {
        FieldUpdate::Absent => {}
        FieldUpdate::Clear => { m.insert("lyrics".into(), Value::Null); }
        FieldUpdate::Set(s) => { m.insert("lyrics".into(), Value::String(s.clone())); }
    }
    Value::Object(m)
}

fn cover_to_value(c: &CoverRef) -> Value {
    let mut m = Map::new();
    m.insert("path".into(), Value::String(c.path.clone()));
    m.insert("mime".into(), opt_value(&c.mime));
    Value::Object(m)
}

fn matched_to_value(src: &Option<TrackMatch>) -> Value {
    let Some(t) = src else { return Value::Null };
    let mut m = Map::new();
    m.insert("id".into(), opt_value(&t.id));
    m.insert("title".into(), opt_value(&t.title));
    m.insert("artist".into(), opt_value(&t.artist));
    m.insert("url".into(), opt_value(&t.url));
    Value::Object(m)
}

fn put_tag(m: &mut Map<String, Value>, key: &str, v: &TagValue) {
    let encoded = match v {
        TagValue::Absent => return,
        TagValue::Clear => Value::Null,
        TagValue::Text(s) => Value::String(s.clone()),
        TagValue::Number(n) => Value::from(*n),
    };
    m.insert(key.to_string(), encoded);
}

fn tags_to_value(t: &Tags) -> Value {
    let mut m = Map::new();
    put_tag(&mut m, "title", &t.title);
    put_tag(&mut m, "artist", &t.artist);
    put_tag(&mut m, "album", &t.album);
    put_tag(&mut m, "year", &t.year);
    put_tag(&mut m, "genre", &t.genre);
    put_tag(&mut m, "track", &t.track);
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRAPE_OK_EXAMPLE: &str = r#"{"id":"q-8842","protocol":1,"action":"scrape","ok":true,"confidence":0.92,"source":"netease","matched":{"id":"123456","title":"t","artist":"a","url":"u"},"tags":{"album":"x","year":"2004","genre":"流行","track":1},"cover":{"path":"cover.jpg","mime":"image/jpeg"},"lyrics":"[00:00.00]..."}"#;
    const DOWNLOAD_OK_EXAMPLE: &str = r#"{"id":"q-8843","protocol":1,"action":"download","ok":true,"file_path":"/music/incoming/x.mp3","format":"mp3","bitrate":320,"size":12345678,"duration_ms":299000,"tags_written":true}"#;
    const ERROR_EXAMPLE: &str = r#"{"id":"q-8844","protocol":1,"action":"scrape","ok":false,"error":{"code":"NOT_FOUND","message":"没找到","retryable":false}}"#;

    fn sample_song() -> SongRef {
        SongRef {
            file_path: Some("/music/a.mp3".to_string()),
            title: Some("夜曲".to_string()),
            artist: Some("周杰伦".to_string()),
            album: None,
            duration_ms: Some(299_000),
            audio_hash: Some("sha256:abc".to_string()),
            fingerprint: None,
        }
    }

    fn scrape_request() -> PluginRequest {
        PluginRequest::Scrape(ScrapeRequest {
            id: "q-8842".to_string(),
            song: sample_song(),
            work_dir: "/var/tmp/mtsrv-job-8842/".to_string(),
            want: vec!["title".to_string(), "artist".to_string(), "cover".to_string()],
            options: RequestOptions { timeout_ms: Some(30_000) },
        })
    }

    fn download_request() -> PluginRequest {
        PluginRequest::Download(DownloadRequest {
            id: "q-8843".to_string(),
            song: sample_song(),
            target_dir: "/music/incoming".to_string(),
            prefer: DownloadPrefer { format: Some("mp3".to_string()), bitrate: Some(320) },
            options: RequestOptions { timeout_ms: Some(120_000) },
        })
    }

    fn download_mv_request() -> PluginRequest {
        PluginRequest::DownloadMv(DownloadMvRequest {
            id: "q-8844".to_string(),
            song: SongRef {
                title: Some("x".to_string()),
                artist: Some("y".to_string()),
                ..Default::default()
            },
            target_dir: "/music/mv".to_string(),
            prefer: MvPrefer { format: Some("mp4".to_string()), max_height: Some(1080) },
            options: RequestOptions::default(),
        })
    }

    fn roundtrip(req: &PluginRequest) -> PluginRequest {
        let line = encode_request(req);
        assert!(!line.contains('\n'), "编码结果不能含换行：{line}");
        assert!(!line.contains('\r'), "编码结果不能含回车：{line}");
        decode_request(&line).expect("解码失败")
    }

    fn scrape_of(line: &str) -> ScrapeOk {
        match decode_response(line).expect("解码失败") {
            PluginResponse::ScrapeOk(r) => r,
            other => panic!("期望 ScrapeOk，实际 {other:?}"),
        }
    }

    /// 首选候选。多数用例只关心最可信那条（插件只回一条时它就是那条）。
    fn cand_of(line: &str) -> ScrapeCandidate {
        scrape_of(line).best().cloned().expect("至少要有一条候选")
    }

    fn download_of(line: &str) -> DownloadOk {
        match decode_response(line).expect("解码失败") {
            PluginResponse::DownloadOk(r) => r,
            other => panic!("期望 DownloadOk，实际 {other:?}"),
        }
    }

    fn error_of(line: &str) -> PluginErrorResponse {
        match decode_response(line).expect("解码失败") {
            PluginResponse::Error(r) => r,
            other => panic!("期望 Error，实际 {other:?}"),
        }
    }

    fn scrape_with_cover(path: &str) -> Result<PluginResponse, ProtocolError> {
        let line = serde_json::json!({
            "id": "q",
            "protocol": 1,
            "action": "scrape",
            "ok": true,
            "cover": { "path": path, "mime": "image/jpeg" }
        })
        .to_string();
        decode_response(&line)
    }

    // ── 请求：往返 ──

    #[test]
    fn scrape_request_roundtrip() {
        let req = scrape_request();
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn download_request_roundtrip() {
        let req = download_request();
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn download_mv_request_roundtrip() {
        let req = download_mv_request();
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn request_id_and_action_accessors() {
        assert_eq!(scrape_request().id(), "q-8842");
        assert_eq!(scrape_request().action(), "scrape");
        assert_eq!(download_request().id(), "q-8843");
        assert_eq!(download_request().action(), "download");
        assert_eq!(download_mv_request().id(), "q-8844");
        assert_eq!(download_mv_request().action(), "download_mv");
    }

    #[test]
    fn encoded_request_is_single_line() {
        let line = encode_request(&scrape_request());
        assert!(line.starts_with('{') && line.ends_with('}'));
        assert!(line.contains("\"protocol\":1"));
        assert!(line.contains("\"action\":\"scrape\""));
        assert!(!line.ends_with('\n'), "编码结果不能带行尾换行");

        // 值里的换行必须被转义成 \n，而不是裸换行
        let req = PluginRequest::Scrape(ScrapeRequest {
            id: "q-1".to_string(),
            song: SongRef { title: Some("a\nb".to_string()), ..Default::default() },
            work_dir: "/tmp/w".to_string(),
            want: Vec::new(),
            options: RequestOptions::default(),
        });
        let line2 = encode_request(&req);
        assert!(!line2.contains('\n'), "内嵌换行必须转义：{line2}");
        assert!(line2.contains("a\\nb"), "换行应被转义成反斜杠 n：{line2}");
        match decode_request(&line2).expect("解码失败") {
            PluginRequest::Scrape(s) => assert_eq!(s.song.title.as_deref(), Some("a\nb")),
            other => panic!("期望 Scrape，实际 {other:?}"),
        }
    }

    #[test]
    fn song_null_fields_are_emitted_explicitly() {
        let line = encode_request(&scrape_request());
        assert!(line.contains("\"album\":null"), "缺值字段按协议示例显式写 null：{line}");
        assert!(line.contains("\"fingerprint\":null"));
    }

    #[test]
    fn download_mv_request_decodes_spec_example() {
        let line = r#"{"id":"q-8844","protocol":1,"action":"download_mv","song":{"title":"x","artist":"y"},"target_dir":"/music/mv","prefer":{"format":"mp4","max_height":1080}}"#;
        match decode_request(line).expect("解码失败") {
            PluginRequest::DownloadMv(r) => {
                assert_eq!(r.id, "q-8844");
                assert_eq!(r.target_dir, "/music/mv");
                assert_eq!(r.prefer.format.as_deref(), Some("mp4"));
                assert_eq!(r.prefer.max_height, Some(1080));
                assert_eq!(r.options.timeout_ms, None, "options 缺省应为 None");
                assert_eq!(r.song.title.as_deref(), Some("x"));
                assert_eq!(r.song.file_path, None);
            }
            other => panic!("期望 DownloadMv，实际 {other:?}"),
        }
    }

    #[test]
    fn scrape_request_decodes_spec_example() {
        let line = r#"{"id":"q-8842","protocol":1,"action":"scrape","song":{"file_path":"/m/a.mp3","title":"t","artist":"a","album":null,"duration_ms":299000,"audio_hash":"sha256:x","fingerprint":null},"work_dir":"/var/tmp/mtsrv-job-8842/","want":["title","cover"],"options":{"timeout_ms":30000}}"#;
        match decode_request(line).expect("解码失败") {
            PluginRequest::Scrape(r) => {
                assert_eq!(r.work_dir, "/var/tmp/mtsrv-job-8842/");
                assert_eq!(r.want, vec!["title", "cover"]);
                assert_eq!(r.options.timeout_ms, Some(30_000));
                assert_eq!(r.song.duration_ms, Some(299_000));
                assert_eq!(r.song.album, None);
            }
            other => panic!("期望 Scrape，实际 {other:?}"),
        }
    }

    #[test]
    fn options_default_when_absent() {
        let line = r#"{"id":"q","protocol":1,"action":"scrape","work_dir":"/tmp"}"#;
        match decode_request(line).expect("解码失败") {
            PluginRequest::Scrape(r) => {
                assert_eq!(r.options.timeout_ms, None);
                assert!(r.want.is_empty());
                assert_eq!(r.song, SongRef::default());
            }
            other => panic!("期望 Scrape，实际 {other:?}"),
        }
    }

    // ── 请求：错误路径 ──

    #[test]
    fn request_rejects_bad_json() {
        assert!(matches!(decode_request("{oops"), Err(ProtocolError::BadJson(_))));
        assert!(matches!(decode_request("[1,2]"), Err(ProtocolError::BadJson(_))));
        assert!(matches!(decode_request(""), Err(ProtocolError::BadJson(_))));
    }

    #[test]
    fn request_rejects_missing_id() {
        let line = r#"{"protocol":1,"action":"scrape","work_dir":"/tmp"}"#;
        assert_eq!(decode_request(line), Err(ProtocolError::MissingField("id")));
    }

    #[test]
    fn request_rejects_missing_protocol() {
        let line = r#"{"id":"q","action":"scrape","work_dir":"/tmp"}"#;
        assert_eq!(decode_request(line), Err(ProtocolError::MissingField("protocol")));
    }

    #[test]
    fn request_rejects_unsupported_protocol() {
        let line = r#"{"id":"q","protocol":2,"action":"scrape","work_dir":"/tmp"}"#;
        assert_eq!(decode_request(line), Err(ProtocolError::UnsupportedProtocol(2)));
    }

    #[test]
    fn request_rejects_unknown_action() {
        let line = r#"{"id":"q","protocol":1,"action":"frobnicate"}"#;
        assert!(matches!(decode_request(line), Err(ProtocolError::BadJson(_))));
    }

    #[test]
    fn request_rejects_missing_action() {
        let line = r#"{"id":"q","protocol":1}"#;
        assert_eq!(decode_request(line), Err(ProtocolError::MissingField("action")));
    }

    #[test]
    fn scrape_requires_work_dir() {
        let line = r#"{"id":"q","protocol":1,"action":"scrape"}"#;
        assert_eq!(decode_request(line), Err(ProtocolError::MissingField("work_dir")));
    }

    #[test]
    fn download_requires_target_dir() {
        let one = r#"{"id":"q","protocol":1,"action":"download"}"#;
        assert_eq!(decode_request(one), Err(ProtocolError::MissingField("target_dir")));
        let two = r#"{"id":"q","protocol":1,"action":"download_mv"}"#;
        assert_eq!(decode_request(two), Err(ProtocolError::MissingField("target_dir")));
    }

    #[test]
    fn request_rejects_bad_song_type() {
        let line = r#"{"id":"q","protocol":1,"action":"scrape","work_dir":"/tmp","song":[]}"#;
        assert!(matches!(decode_request(line), Err(ProtocolError::BadJson(_))));
    }

    // ── 响应：三种形状 ──

    #[test]
    fn scrape_ok_decodes_spec_example() {
        let ok = scrape_of(SCRAPE_OK_EXAMPLE);
        assert_eq!(ok.id, "q-8842");
        let r = ok.best().expect("至少一条候选");
        assert!((r.confidence - 0.92).abs() < 1e-9);
        assert_eq!(r.source.as_deref(), Some("netease"));
        let m = r.matched.clone().expect("matched 缺失");
        assert_eq!(m.id.as_deref(), Some("123456"));
        assert_eq!(m.url.as_deref(), Some("u"));
        assert_eq!(r.tags.album, TagValue::Text("x".to_string()));
        assert_eq!(r.tags.year, TagValue::Text("2004".to_string()));
        assert_eq!(r.tags.genre, TagValue::Text("流行".to_string()));
        assert_eq!(r.tags.track, TagValue::Number(1));
        assert_eq!(r.tags.title, TagValue::Absent, "未出现的标签字段必须保持缺省");
        match &r.cover {
            FieldUpdate::Set(c) => {
                assert_eq!(c.path, "cover.jpg");
                assert_eq!(c.mime.as_deref(), Some("image/jpeg"));
            }
            other => panic!("期望 Set，实际 {other:?}"),
        }
        assert_eq!(r.lyrics, FieldUpdate::Set("[00:00.00]...".to_string()));
    }

    #[test]
    fn download_ok_decodes_spec_example() {
        let r = download_of(DOWNLOAD_OK_EXAMPLE);
        assert_eq!(r.id, "q-8843");
        assert_eq!(r.file_path, "/music/incoming/x.mp3");
        assert_eq!(r.format.as_deref(), Some("mp3"));
        assert_eq!(r.bitrate, Some(320));
        assert_eq!(r.size, Some(12_345_678));
        assert_eq!(r.duration_ms, Some(299_000));
        assert!(r.tags_written);
    }

    #[test]
    fn error_response_decodes_spec_example() {
        let r = error_of(ERROR_EXAMPLE);
        assert_eq!(r.id, "q-8844");
        assert_eq!(r.error.code, ErrorCode::NotFound);
        assert_eq!(r.error.message, "没找到");
        assert!(!r.error.retryable);
    }

    #[test]
    fn response_kind_is_discriminated_by_action() {
        assert!(matches!(decode_response(SCRAPE_OK_EXAMPLE), Ok(PluginResponse::ScrapeOk(_))));
        assert!(matches!(decode_response(DOWNLOAD_OK_EXAMPLE), Ok(PluginResponse::DownloadOk(_))));
        // download_mv 成功复用 DownloadOk，靠 action 字段区分二者
        let mv = r#"{"id":"q-8844","protocol":1,"action":"download_mv","ok":true,"file_path":"/music/mv/x.mp4","format":"mp4"}"#;
        assert!(matches!(decode_response(mv), Ok(PluginResponse::DownloadOk(_))));
        // 只有 ok 的裸响应按 scrape 处理（tags 全缺省 = 不修改）
        let bare = cand_of(r#"{"id":"q","protocol":1,"action":"scrape","ok":true}"#);
        assert_eq!(bare.confidence, 0.50);
        assert_eq!(bare.tags, Tags::default());
    }

    // ── 多候选（本次改动核心）──
    //
    // 同一歌名在数据源里往往对应多条录音（原版 / 现场 / 重混 / 翻唱），
    // 服务端只负责「按 confidence 取最高」，所以这两条不变式必须钉住：
    //   ① 解码后**一定**是降序，不依赖插件给的顺序；
    //   ② 旧的平铺形状仍然能解，且等价于「只有一条候选」。

    #[test]
    fn candidates_are_sorted_by_confidence_regardless_of_plugin_order() {
        // 故意把最差的那条放在最前面 —— 服务端不能被插件的顺序带跑
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"candidates":[
            {"confidence":0.55,"tags":{"album":"合辑"}},
            {"confidence":0.97,"tags":{"album":"原版"}},
            {"confidence":0.80,"tags":{"album":"现场版"}}
        ]}"#;
        let ok = scrape_of(line);
        let albums: Vec<_> = ok
            .ranked()
            .iter()
            .map(|c| match &c.tags.album {
                TagValue::Text(t) => t.clone(),
                other => panic!("album 应是文本，实际 {other:?}"),
            })
            .collect();
        assert_eq!(albums, vec!["原版", "现场版", "合辑"], "必须按 confidence 降序");
        assert!((ok.best().expect("候选").confidence - 0.97).abs() < 1e-9);
    }

    #[test]
    fn ties_keep_the_plugin_order() {
        // confidence 相同时保留插件给的先后（插件往往把更可信的放前面）。
        // 用稳定排序才做得到；换成 sort_unstable 这条就会随机翻车。
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"candidates":[
            {"confidence":0.8,"tags":{"title":"先"}},
            {"confidence":0.8,"tags":{"title":"后"}}
        ]}"#;
        let ok = scrape_of(line);
        let titles: Vec<_> = ok
            .ranked()
            .iter()
            .map(|c| match &c.tags.title {
                TagValue::Text(t) => t.clone(),
                other => panic!("title 应是文本，实际 {other:?}"),
            })
            .collect();
        assert_eq!(titles, vec!["先", "后"]);
    }

    #[test]
    fn flat_shape_is_a_single_candidate() {
        // 协议 v1 的原始形状（字段平铺）必须继续能解 —— 已发布的插件不能被这次改动打死。
        let ok = scrape_of(SCRAPE_OK_EXAMPLE);
        assert_eq!(ok.ranked().len(), 1, "平铺形状等价于只有一条候选");
        assert_eq!(ok.best().map(|c| c.source.as_deref()), Some(Some("netease")));
    }

    #[test]
    fn empty_candidate_array_is_rejected() {
        // `ok:true` 却一条候选都没有是自相矛盾的 —— 那种情况该回 ok:false + NOT_FOUND。
        // 静默接受会让上层拿到一个「成功但没有内容」的结果，比报错更难查。
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"candidates":[]}"#;
        assert!(matches!(decode_response(line), Err(ProtocolError::BadJson(_))));
    }

    #[test]
    fn candidates_must_be_an_array() {
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"candidates":{}}"#;
        assert!(matches!(decode_response(line), Err(ProtocolError::BadJson(_))));
    }

    #[test]
    fn candidates_roundtrip_and_stay_sorted() {
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"candidates":[
            {"confidence":0.6,"source":"a","tags":{"album":"合辑"}},
            {"confidence":0.9,"source":"b","tags":{"album":"原版"}}
        ]}"#;
        let ok = scrape_of(line);
        let encoded = encode_response(&PluginResponse::ScrapeOk(ok.clone()));
        assert!(encoded.contains("\"candidates\""), "编码必须用新形状：{encoded}");
        assert_eq!(scrape_of(&encoded), ok, "往返后应完全一致（含顺序）");
    }

    #[test]
    fn each_candidate_carries_its_own_fields() {
        // 关键：candidates 里的 tags / cover / lyrics 是**每条各自**的，
        // 不能从外层继承 —— 每条候选对应不同的 release，专辑名与曲目号本来就不同。
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"candidates":[
            {"confidence":0.9,"tags":{"album":"A","track":1},"lyrics":"[00:01]a"},
            {"confidence":0.8,"tags":{"album":"B","track":9}}
        ]}"#;
        let ok = scrape_of(line);
        let first = &ok.ranked()[0];
        let second = &ok.ranked()[1];
        assert_eq!(first.tags.album, TagValue::Text("A".to_string()));
        assert_eq!(first.tags.track, TagValue::Number(1));
        assert_eq!(second.tags.album, TagValue::Text("B".to_string()));
        assert_eq!(second.tags.track, TagValue::Number(9));
        assert_eq!(first.lyrics, FieldUpdate::Set("[00:01]a".to_string()));
        assert_eq!(second.lyrics, FieldUpdate::Absent, "第二条没给歌词就是缺省，不能继承第一条");
    }

    #[test]
    fn response_roundtrips_through_encoder() {
        let scrape = scrape_of(SCRAPE_OK_EXAMPLE);
        let line = encode_response(&PluginResponse::ScrapeOk(scrape.clone()));
        assert!(!line.contains('\n'));
        assert_eq!(scrape_of(&line), scrape);

        let dl = download_of(DOWNLOAD_OK_EXAMPLE);
        let line = encode_response(&PluginResponse::DownloadOk(dl.clone()));
        assert_eq!(download_of(&line), dl);

        let err = error_of(ERROR_EXAMPLE);
        let line = encode_response(&PluginResponse::Error(err.clone()));
        assert_eq!(error_of(&line), err);
    }

    // ── 响应：错误码 ──

    #[test]
    fn all_error_codes_roundtrip() {
        let all = [
            (ErrorCode::NotFound, "NOT_FOUND"),
            (ErrorCode::RateLimited, "RATE_LIMITED"),
            (ErrorCode::AuthFailed, "AUTH_FAILED"),
            (ErrorCode::Network, "NETWORK"),
            (ErrorCode::Timeout, "TIMEOUT"),
            (ErrorCode::BadRequest, "BAD_REQUEST"),
            (ErrorCode::Internal, "INTERNAL"),
        ];
        for (code, text) in all {
            assert_eq!(code.as_str(), text);
            assert_eq!(ErrorCode::parse(text), Some(code));
            let line = format!(
                "{{\"id\":\"q\",\"protocol\":1,\"action\":\"scrape\",\"ok\":false,\"error\":{{\"code\":\"{text}\",\"message\":\"m\",\"retryable\":false}}}}"
            );
            let resp = error_of(&line);
            assert_eq!(resp.error.code, code);
            assert_eq!(resp.error.message, "m");
            assert_eq!(error_of(&encode_response(&PluginResponse::Error(resp.clone()))), resp);
        }
        assert_eq!(ErrorCode::parse("NOPE"), None);
        assert_eq!(ErrorCode::parse("not_found"), None);
    }

    #[test]
    fn response_rejects_unknown_error_code() {
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":false,"error":{"code":"OOPS","message":"m"}}"#;
        assert!(matches!(decode_response(line), Err(ProtocolError::BadJson(_))));
    }

    #[test]
    fn error_retryable_defaults_to_false() {
        let a = error_of(
            r#"{"id":"q","protocol":1,"action":"scrape","ok":false,"error":{"code":"NETWORK","message":"m"}}"#,
        );
        assert!(!a.error.retryable);
        let b = error_of(
            r#"{"id":"q","protocol":1,"action":"scrape","ok":false,"error":{"code":"NETWORK","message":"m","retryable":true}}"#,
        );
        assert!(b.error.retryable);
    }

    // ── 敏感语义：缺省 vs 显式 null ──

    #[test]
    fn tags_absent_vs_null_are_distinguished() {
        let absent = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{"album":"A"}}"#;
        let null = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{"album":null}}"#;

        let a_ok = scrape_of(absent);
        let a = a_ok.best().expect("候选");
        assert_eq!(a.tags.album, TagValue::Text("A".to_string()));
        assert_eq!(a.tags.year, TagValue::Absent, "字段缺省 = 不修改");
        let a_line = encode_response(&PluginResponse::ScrapeOk(a_ok.clone()));
        assert!(!a_line.contains("year"), "缺省字段不应出现在编码结果里：{a_line}");
        assert_eq!(scrape_of(&a_line), a_ok);

        let n_ok = scrape_of(null);
        let n = n_ok.best().expect("候选");
        assert_eq!(n.tags.album, TagValue::Clear, "显式 null = 清空");
        let n_line = encode_response(&PluginResponse::ScrapeOk(n_ok.clone()));
        assert!(n_line.contains("\"album\":null"), "显式 null 必须编码成 null：{n_line}");
        assert_eq!(scrape_of(&n_line), n_ok);

        // 区分度：两者在类型上就不相等，调用方不会被误导
        assert_ne!(a.tags.album, n.tags.album);
    }

    #[test]
    fn null_vs_absent_does_not_survive_as_same_value() {
        // 反向验证：把显式 null 当成缺省就会踩坑，这里断言它们不同
        let absent = cand_of(r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{}}"#);
        let cleared = cand_of(
            r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{"track":null}}"#,
        );
        assert_eq!(absent.tags.track, TagValue::Absent);
        assert_eq!(cleared.tags.track, TagValue::Clear);
        assert_ne!(absent.tags.track, cleared.tags.track);
    }

    #[test]
    fn cover_absent_null_and_set_are_distinguished() {
        let base = |cover: &str| {
            format!("{{\"id\":\"q\",\"protocol\":1,\"action\":\"scrape\",\"ok\":true{cover}}}")
        };

        let absent_ok = scrape_of(&base(""));
        let absent = absent_ok.best().expect("候选");
        assert_eq!(absent.cover, FieldUpdate::Absent);
        let line = encode_response(&PluginResponse::ScrapeOk(absent_ok.clone()));
        assert!(!line.contains("cover"), "缺省 cover 不应出现在编码结果里：{line}");

        let cleared_ok = scrape_of(&base(",\"cover\":null"));
        let cleared = cleared_ok.best().expect("候选");
        assert_eq!(cleared.cover, FieldUpdate::Clear);
        let line = encode_response(&PluginResponse::ScrapeOk(cleared_ok.clone()));
        assert!(line.contains("\"cover\":null"), "显式 null 必须编码成 null：{line}");
        assert_eq!(cand_of(&line).cover, FieldUpdate::Clear);

        let set_ok = scrape_of(&base(",\"cover\":{\"path\":\"cover.jpg\"}"));
        let set = set_ok.best().expect("候选");
        match &set.cover {
            FieldUpdate::Set(c) => {
                assert_eq!(c.path, "cover.jpg");
                assert_eq!(c.mime, None);
            }
            other => panic!("期望 Set，实际 {other:?}"),
        }
        assert_eq!(scrape_of(&encode_response(&PluginResponse::ScrapeOk(set_ok.clone()))), set_ok);
    }

    #[test]
    fn lyrics_absent_null_and_set_are_distinguished() {
        let base = |lyrics: &str| {
            format!("{{\"id\":\"q\",\"protocol\":1,\"action\":\"scrape\",\"ok\":true{lyrics}}}")
        };
        assert_eq!(cand_of(&base("")).lyrics, FieldUpdate::Absent);
        assert_eq!(cand_of(&base(",\"lyrics\":null")).lyrics, FieldUpdate::Clear);
        assert_eq!(
            cand_of(&base(",\"lyrics\":\"[00:01]\"")).lyrics,
            FieldUpdate::Set("[00:01]".to_string())
        );

        let cleared = encode_response(&PluginResponse::ScrapeOk(scrape_of(&base(",\"lyrics\":null"))));
        assert!(cleared.contains("\"lyrics\":null"));
        let absent = encode_response(&PluginResponse::ScrapeOk(scrape_of(&base(""))));
        assert!(!absent.contains("lyrics"), "缺省歌词不应写键：{absent}");
    }

    #[test]
    fn confidence_defaults_to_half() {
        let d = |c: &str| {
            cand_of(&format!(
                "{{\"id\":\"q\",\"protocol\":1,\"action\":\"scrape\",\"ok\":true,\"tags\":{{}}{c}}}"
            ))
            .confidence
        };
        assert!((d("") - 0.50).abs() < 1e-9, "confidence 缺省应为 0.50");
        assert!((d(",\"confidence\":null") - 0.50).abs() < 1e-9);
        assert!((d(",\"confidence\":0.92") - 0.92).abs() < 1e-9);
        assert!((d(",\"confidence\":\"0.75\"") - 0.75).abs() < 1e-9, "数字字符串也接受");
        assert!(matches!(
            decode_response(r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"confidence":"abc"}"#),
            Err(ProtocolError::BadJson(_))
        ));
    }

    // ── cover.path 校验 ──

    #[test]
    fn cover_path_absolute_or_escaping_is_rejected() {
        let bad = ["/etc/passwd", "\\etc\\passwd", "C:\\x.jpg", "../x.jpg", "a/../../b.jpg", "", "a\u{0}b"];
        for p in bad {
            match scrape_with_cover(p) {
                Err(ProtocolError::BadCoverPath(got)) => assert_eq!(got, p),
                other => panic!("路径 {p:?} 应判协议错误，实际 {other:?}"),
            }
        }
    }

    #[test]
    fn cover_path_relative_is_accepted() {
        for p in ["cover.jpg", "sub/cover.jpg", "./cover.jpg", "a/b/c.png"] {
            match scrape_with_cover(p) {
                Ok(PluginResponse::ScrapeOk(r)) => match &r.best().expect("候选").cover {
                    FieldUpdate::Set(c) => assert_eq!(c.path, p),
                    other => panic!("期望 Set，实际 {other:?}"),
                },
                other => panic!("路径 {p:?} 应当通过，实际 {other:?}"),
            }
        }
    }

    #[test]
    fn validate_cover_path_direct() {
        assert!(validate_cover_path("cover.jpg").is_ok());
        assert!(validate_cover_path("a/b.jpg").is_ok());
        assert!(validate_cover_path("./a.jpg").is_ok());
        assert_eq!(
            validate_cover_path("/etc/passwd"),
            Err(ProtocolError::BadCoverPath("/etc/passwd".to_string()))
        );
        assert_eq!(
            validate_cover_path("../x.jpg"),
            Err(ProtocolError::BadCoverPath("../x.jpg".to_string()))
        );
        assert_eq!(validate_cover_path(""), Err(ProtocolError::BadCoverPath(String::new())));
        assert!(validate_cover_path("..").is_err());
        assert!(validate_cover_path("a/..").is_err());
        assert!(validate_cover_path("C:/x.jpg").is_err());
    }

    #[test]
    fn cover_without_path_is_missing_field() {
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"cover":{"mime":"image/jpeg"}}"#;
        assert_eq!(decode_response(line), Err(ProtocolError::MissingField("cover.path")));
    }

    #[test]
    fn cover_of_wrong_type_is_bad_json() {
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"cover":[]}"#;
        assert!(matches!(decode_response(line), Err(ProtocolError::BadJson(_))));
    }

    // ── 响应：错误路径 ──

    #[test]
    fn response_rejects_bad_json() {
        assert!(matches!(decode_response("nope"), Err(ProtocolError::BadJson(_))));
        assert!(matches!(decode_response("[1,2]"), Err(ProtocolError::BadJson(_))));
    }

    #[test]
    fn response_rejects_missing_id() {
        assert_eq!(
            decode_response(r#"{"protocol":1,"action":"scrape","ok":true}"#),
            Err(ProtocolError::MissingField("id"))
        );
    }

    #[test]
    fn response_rejects_missing_protocol() {
        assert_eq!(
            decode_response(r#"{"id":"q","action":"scrape","ok":true}"#),
            Err(ProtocolError::MissingField("protocol"))
        );
    }

    #[test]
    fn response_rejects_unsupported_protocol() {
        assert_eq!(
            decode_response(r#"{"id":"q","protocol":2,"action":"scrape","ok":true}"#),
            Err(ProtocolError::UnsupportedProtocol(2))
        );
    }

    #[test]
    fn response_rejects_missing_ok() {
        assert_eq!(
            decode_response(r#"{"id":"q","protocol":1,"action":"scrape"}"#),
            Err(ProtocolError::MissingField("ok"))
        );
        assert!(matches!(
            decode_response(r#"{"id":"q","protocol":1,"action":"scrape","ok":"yes"}"#),
            Err(ProtocolError::BadJson(_))
        ));
    }

    #[test]
    fn failure_response_requires_error_object_and_code() {
        assert_eq!(
            decode_response(r#"{"id":"q","protocol":1,"action":"scrape","ok":false}"#),
            Err(ProtocolError::MissingField("error"))
        );
        assert_eq!(
            decode_response(
                r#"{"id":"q","protocol":1,"action":"scrape","ok":false,"error":{"message":"m"}}"#
            ),
            Err(ProtocolError::MissingField("error.code"))
        );
        assert_eq!(
            decode_response(
                r#"{"id":"q","protocol":1,"action":"scrape","ok":false,"error":{"code":"NETWORK"}}"#
            ),
            Err(ProtocolError::MissingField("error.message"))
        );
    }

    // ── 其它小语义 ──

    #[test]
    fn matched_absent_and_null_are_both_none() {
        assert!(cand_of(r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{}}"#)
            .matched
            .is_none());
        assert!(cand_of(
            r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{},"matched":null}"#
        )
        .matched
        .is_none());
        let m = cand_of(
            r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{},"matched":{"id":"1"}}"#,
        )
            .matched
            .expect("matched 缺失");
        assert_eq!(m.id.as_deref(), Some("1"));
        assert_eq!(m.title, None);
    }

    #[test]
    fn download_ok_optional_fields_default() {
        let r = download_of(
            r#"{"id":"q","protocol":1,"action":"download","ok":true,"file_path":"/x.mp3"}"#,
        );
        assert_eq!(r.format, None);
        assert_eq!(r.bitrate, None);
        assert_eq!(r.size, None);
        assert_eq!(r.duration_ms, None);
        assert!(!r.tags_written);
    }

    #[test]
    fn numeric_strings_in_response_are_accepted() {
        let line = r#"{"id":"q","protocol":"1","action":"download","ok":true,"file_path":"/x.mp3","bitrate":"320","size":"9","duration_ms":"1"}"#;
        let r = download_of(line);
        assert_eq!(r.bitrate, Some(320));
        assert_eq!(r.size, Some(9));
        assert_eq!(r.duration_ms, Some(1));
        assert!(!r.tags_written);
    }

    #[test]
    fn tag_field_types_and_clear_roundtrip() {
        let line = r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{"title":"T","artist":"A","album":null,"year":2004,"genre":"Rock","track":7}}"#;
        let ok = scrape_of(line);
        let r = ok.best().expect("候选");
        assert_eq!(r.tags.title, TagValue::Text("T".to_string()));
        assert_eq!(r.tags.artist, TagValue::Text("A".to_string()));
        assert_eq!(r.tags.album, TagValue::Clear);
        assert_eq!(r.tags.year, TagValue::Number(2004));
        assert_eq!(r.tags.track, TagValue::Number(7));
        assert_eq!(scrape_of(&encode_response(&PluginResponse::ScrapeOk(ok.clone()))), ok);

        // 非法类型：布尔 / 浮点
        assert!(matches!(
            decode_response(r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{"track":true}}"#),
            Err(ProtocolError::BadJson(_))
        ));
        assert!(matches!(
            decode_response(r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":{"track":1.5}}"#),
            Err(ProtocolError::BadJson(_))
        ));
    }

    #[test]
    fn tags_null_means_no_tag_update() {
        // tags 整体为 null：按「不做任何修改」处理，而不是清空全部字段
        let r = cand_of(r#"{"id":"q","protocol":1,"action":"scrape","ok":true,"tags":null}"#);
        assert_eq!(r.tags, Tags::default());
        assert_eq!(r.tags.album, TagValue::Absent);
    }

    #[test]
    fn field_update_default_is_absent() {
        assert_eq!(FieldUpdate::<String>::default(), FieldUpdate::Absent);
        assert_eq!(TagValue::default(), TagValue::Absent);
        assert_eq!(Tags::default(), Tags::default());
    }

    // ── action 判别（本次缺陷修复的核心证据） ──

    #[test]
    fn scrape_with_stray_file_path_is_still_scrape() {
        // 回归：刮削插件回显了请求里的 song.file_path。旧实现靠「有没有 file_path」
        // 判类型，会把这条响应当成 DownloadOk，confidence / tags / cover / lyrics
        // 全部被静默丢弃。现在判别只看 action，这些字段一个都不能丢。
        let line = r#"{"id":"q-9001","protocol":1,"action":"scrape","ok":true,"file_path":"/music/a.mp3","confidence":0.92,"source":"netease","matched":{"id":"123456","title":"t","artist":"a","url":"u"},"tags":{"album":"x","year":"2004","genre":"流行","track":1},"cover":{"path":"cover.jpg","mime":"image/jpeg"},"lyrics":"[00:00.00]..."}"#;
        let ok = scrape_of(line);
        assert_eq!(ok.id, "q-9001");
        let r = ok.best().expect("候选");
        assert!((r.confidence - 0.92).abs() < 1e-9, "confidence 不能被静默丢弃");
        assert_eq!(r.source.as_deref(), Some("netease"), "source 不能被静默丢弃");
        let m = r.matched.clone().expect("matched 不能被静默丢弃");
        assert_eq!(m.id.as_deref(), Some("123456"));
        assert_eq!(m.title.as_deref(), Some("t"));
        assert_eq!(r.tags.album, TagValue::Text("x".to_string()), "tags 不能被静默丢弃");
        assert_eq!(r.tags.year, TagValue::Text("2004".to_string()));
        assert_eq!(r.tags.genre, TagValue::Text("流行".to_string()));
        assert_eq!(r.tags.track, TagValue::Number(1));
        assert_eq!(r.tags.title, TagValue::Absent, "未出现的标签字段保持缺省");
        match &r.cover {
            FieldUpdate::Set(c) => {
                assert_eq!(c.path, "cover.jpg");
                assert_eq!(c.mime.as_deref(), Some("image/jpeg"));
            }
            other => panic!("cover 不能被静默丢弃，期望 Set，实际 {other:?}"),
        }
        assert_eq!(r.lyrics, FieldUpdate::Set("[00:00.00]...".to_string()), "lyrics 不能被静默丢弃");
    }

    #[test]
    fn response_requires_action() {
        assert_eq!(
            decode_response(r#"{"id":"q","protocol":1,"ok":true,"confidence":0.9}"#),
            Err(ProtocolError::MissingField("action"))
        );
        // action 为 null / 空串一律按缺字段处理
        assert_eq!(
            decode_response(r#"{"id":"q","protocol":1,"action":null,"ok":true}"#),
            Err(ProtocolError::MissingField("action"))
        );
        assert_eq!(
            decode_response(r#"{"id":"q","protocol":1,"action":"","ok":true}"#),
            Err(ProtocolError::MissingField("action"))
        );
        // 非字符串 action 是类型错误
        assert!(matches!(
            decode_response(r#"{"id":"q","protocol":1,"action":7,"ok":true}"#),
            Err(ProtocolError::BadJson(_))
        ));
    }

    #[test]
    fn response_rejects_unknown_action() {
        for bad in ["scrape_mv", "DOWNLOAD", "Download_Mv", "download-mv", "frobnicate"] {
            let line = format!(r#"{{"id":"q","protocol":1,"action":"{bad}","ok":true}}"#);
            assert!(
                matches!(decode_response(&line), Err(ProtocolError::BadJson(_))),
                "未知 action {bad:?} 必须报协议错误：{line}"
            );
        }
    }

    #[test]
    fn download_and_download_mv_actions_are_preserved() {
        let dl = download_of(
            r#"{"id":"q","protocol":1,"action":"download","ok":true,"file_path":"/x.mp3"}"#,
        );
        assert_eq!(dl.action, ResponseAction::Download);
        let mv = download_of(
            r#"{"id":"q","protocol":1,"action":"download_mv","ok":true,"file_path":"/x.mp4"}"#,
        );
        assert_eq!(mv.action, ResponseAction::DownloadMv);
        assert_ne!(dl.action, mv.action, "download 与 download_mv 必须可区分");
    }

    #[test]
    fn error_response_carries_action() {
        let e = error_of(
            r#"{"id":"q","protocol":1,"action":"download_mv","ok":false,"error":{"code":"NOT_FOUND","message":"m"}}"#,
        );
        assert_eq!(e.action, ResponseAction::DownloadMv);
        let s = error_of(
            r#"{"id":"q","protocol":1,"action":"scrape","ok":false,"error":{"code":"TIMEOUT","message":"m"}}"#,
        );
        assert_eq!(s.action, ResponseAction::Scrape);
        // 错误响应同样必须回显 action
        assert_eq!(
            decode_response(
                r#"{"id":"q","protocol":1,"ok":false,"error":{"code":"TIMEOUT","message":"m"}}"#
            ),
            Err(ProtocolError::MissingField("action"))
        );
    }

    #[test]
    fn encoded_response_always_contains_action() {
        let scrape = scrape_of(SCRAPE_OK_EXAMPLE);
        let line = encode_response(&PluginResponse::ScrapeOk(scrape));
        assert!(line.contains(r#""action":"scrape""#), "scrape 编码必须带 action：{line}");

        let dl = download_of(DOWNLOAD_OK_EXAMPLE);
        let line = encode_response(&PluginResponse::DownloadOk(dl));
        assert!(line.contains(r#""action":"download""#), "download 编码必须带 action：{line}");

        let mv = DownloadOk {
            id: "q".to_string(),
            action: ResponseAction::DownloadMv,
            file_path: "/x.mp4".to_string(),
            format: None,
            bitrate: None,
            size: None,
            duration_ms: None,
            tags_written: false,
        };
        let line = encode_response(&PluginResponse::DownloadOk(mv));
        assert!(line.contains(r#""action":"download_mv""#), "download_mv 编码必须带 action：{line}");

        let err = error_of(ERROR_EXAMPLE);
        let line = encode_response(&PluginResponse::Error(err));
        assert!(line.contains(r#""action":"scrape""#), "错误响应编码必须带 action：{line}");
    }

    #[test]
    fn response_action_roundtrips_through_encoder() {
        // scrape
        let scrape = scrape_of(SCRAPE_OK_EXAMPLE);
        let line = encode_response(&PluginResponse::ScrapeOk(scrape.clone()));
        assert!(line.contains(r#""action":"scrape""#));
        assert_eq!(scrape_of(&line), scrape);
        assert_eq!(PluginResponse::ScrapeOk(scrape).action(), ResponseAction::Scrape);

        // download / download_mv 共用 DownloadOk，action 必须原样往返
        for action in [ResponseAction::Download, ResponseAction::DownloadMv] {
            let dl = DownloadOk {
                id: "q-9002".to_string(),
                action,
                file_path: "/x.mp3".to_string(),
                format: Some("mp3".to_string()),
                bitrate: Some(320),
                size: Some(1),
                duration_ms: Some(2),
                tags_written: true,
            };
            let line = encode_response(&PluginResponse::DownloadOk(dl.clone()));
            let back = download_of(&line);
            assert_eq!(back, dl);
            assert_eq!(back.action, action);
            assert_eq!(PluginResponse::DownloadOk(back).action(), action);
        }

        // error 的 action 也要往返
        let err = error_of(ERROR_EXAMPLE);
        let line = encode_response(&PluginResponse::Error(err.clone()));
        let back = error_of(&line);
        assert_eq!(back, err);
        assert_eq!(back.action, ResponseAction::Scrape);
    }

    #[test]
    fn response_action_parse_and_as_str_are_strict() {
        for (action, text) in [
            (ResponseAction::Scrape, "scrape"),
            (ResponseAction::Download, "download"),
            (ResponseAction::DownloadMv, "download_mv"),
        ] {
            assert_eq!(action.as_str(), text);
            assert_eq!(ResponseAction::parse(text), Some(action));
        }
        // 大小写敏感、不 trim、不接受近义字面量
        for bad in ["SCRAPE", "Scrape", "downloadMV", "download-mv", "scrape_mv", " download", ""] {
            assert_eq!(ResponseAction::parse(bad), None, "{bad:?} 不应被解析");
        }
    }

    #[test]
    fn response_accessors_reflect_action_and_id() {
        let s = PluginResponse::ScrapeOk(scrape_of(SCRAPE_OK_EXAMPLE));
        assert_eq!(s.id(), "q-8842");
        assert_eq!(s.action(), ResponseAction::Scrape);

        let d = PluginResponse::DownloadOk(download_of(DOWNLOAD_OK_EXAMPLE));
        assert_eq!(d.id(), "q-8843");
        assert_eq!(d.action(), ResponseAction::Download);

        let e = PluginResponse::Error(error_of(ERROR_EXAMPLE));
        assert_eq!(e.id(), "q-8844");
        assert_eq!(e.action(), ResponseAction::Scrape);
    }
}
