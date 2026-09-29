//! S19 · 转码缓存：把曲库里的无损音频按需转成 MP3，并按 `audio_hash` 缓存复用。
//!
//! 本模块是**唯一**调用 ffmpeg 的地方，同时负责缓存键、临时文件与原子落盘。HTTP 层
//! （状态码映射、响应头组装、鉴权）留在 `crate::server::routes::stream`，与
//! [crate::audio::stream] / [crate::audio::cover] 的分工一致。
//!
//! # 缓存键 = `<cache_dir>/<song_id>-<audio_hash>.mp3`（画布原文）
//!
//! **为什么靠换键失效、不写清理逻辑**：文件名里带 `audio_hash`（裸音频的 sha256，
//! 与标签无关，见 `crate::service::library`）。文件内容一变，扫描就会写入新的 hash，
//! 缓存键随之变成另一个文件名 —— 旧文件自然不会被命中。也就是说 **失效是「键变了」，
//! 不是「删了文件」**：本模块从不主动删缓存，`cache_expiry_days` 那种按天清理
//! 是另一件事（缓存目录的体积治理），不在本步范围。
//!
//! # `audio_hash` 为 NULL / 不可用时：转码但不落缓存
//!
//! 没有 hash 就没有稳定键，同一首歌每次都会得到不同文件名 —— 那样的「缓存」永远
//! 不会命中，只是白占磁盘。所以此时**转码到系统临时目录、把字节直接返回、用完即删**。
//! 另外 `audio_hash` 是数据库里的字符串（库是外部可写的），含 `/`、`..`
//! 之类时必须当作没有键：直接拼进路径就是一次任意路径读写。见 [is_safe_cache_key]。
//!
//! # ffmpeg 调用
//!
//! `ffmpeg_path` 来自 `config.audio.ffmpeg_path`（**可注入，不硬编码 "ffmpeg"**），
//! 参数见 [ffmpeg_args] 的逐条说明。
//!
//! 子进程**没有内置超时**，这里用「spawn + 轮询 try_wait + 到点 kill」实现
//! [TRANSCODE_TIMEOUT]（300s）。stderr 必须**在独立线程里读**：管道缓冲区（约 64KB）
//! 写满而父进程只在等 try_wait，子进程会卡在 write 上永不退出 —— 那才是真正的挂死。
//! stderr 只用于日志与错误分类，**绝不原样回给客户端**（里面可能有服务器路径）。
//!
//! # 并发：唯一临时文件 + 原子 rename
//!
//! 未命中时先写到 `cache_dir/.transcode-<pid>-<nanos>-<seq>.part` 再 `rename`
//! 到缓存路径。同一目录内的 rename 是原子的，所以：
//!
//! * **读者永远看不到半截文件** —— 要么没有缓存（自己也去转），要么是完整的；
//! * 两个请求同时转同一首歌不会互相踩：各自写各自的临时文件，最后 rename 到同一
//!   目标，后到的覆盖先到的。代价是可能白转一次，但两次产出的字节本应相同，且
//!   这与「加锁」相比少了一份跨请求状态。要严格只转一次得引入按 key 的锁表，
//!   本步不做（收益只是省一次 CPU，复杂度却要维护锁生命周期）。
//! * `cache_dir` 不存在会自动 `create_dir_all`。

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// MP3 目标码率。
///
/// 192k CBR 是有损转码里「听感够用、体积可控」的常见折中：比它低（128k）在好一点的
/// 回放设备上能听出区别，比它高（320k）对「给浏览器播放」这个用途收益很小。
const MP3_BITRATE: &str = "192k";

/// 单个 ffmpeg 子进程的最长运行时间。
///
/// 一首歌的 FLAC→MP3 在 NAS 上通常是几秒到几十秒；300s 是「再慢也不该超过」的上界，
/// 用来兜住畸形文件 / ffmpeg 卡死这类无限挂起。配置里目前没有对应字段（不改
/// `src/config.rs`），所以这里是模块常量；将来要按部署调，得先加配置项。
pub const TRANSCODE_TIMEOUT: Duration = Duration::from_secs(300);

/// stderr 写进错误信息前的最大字符数。
///
/// stderr 可能很长（ffmpeg 的报错有时几百行），只保留开头一段够定位问题即可，
/// 避免把内存 / 日志刷爆。取字符数而不是字节数，保证按 UTF-8 边界截断。
const MAX_STDERR_CHARS: usize = 2000;

/// 缓存键里 `audio_hash` 允许的最大长度（sha256 hex 是 64）。
const MAX_CACHE_KEY_LEN: usize = 128;

/// `?format=` 取值非法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    /// 本接口只认 mp3，其余格式明确拒绝（路由层回 400），绝不静默回退成直传。
    Unsupported,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Unsupported => write!(f, "不支持的转码格式，目前只支持 mp3"),
        }
    }
}

impl std::error::Error for FormatError {}

/// 解析 `?format=` 参数：是否需要转码后的 MP3。
///
/// * 缺省 / 空串 / 全空白 → `Ok(false)`（走 S18 的直传路径，行为完全不变）；
/// * `mp3`（ASCII 大小写不敏感）→ `Ok(true)`；
/// * 其它取值 → [FormatError::Unsupported]（路由层回 400）。
///
/// 纯函数：不碰 IO，便于穷举单测。
pub fn parse_format(raw: Option<&str>) -> Result<bool, FormatError> {
    let Some(text) = raw else {
        return Ok(false);
    };
    let text = text.trim();
    if text.is_empty() {
        return Ok(false);
    }
    if text.eq_ignore_ascii_case("mp3") {
        return Ok(true);
    }
    Err(FormatError::Unsupported)
}

/// `audio_hash` 能不能安全地当缓存键。
///
/// 它是数据库里的字符串，库是外部可写的：含 `/`、`..`、NUL 等字符时直接
/// 拼进文件名就是路径穿越（读 / 写到缓存目录之外）。这里只放行
/// `[0-9A-Za-z_-]` 且长度不超过 [MAX_CACHE_KEY_LEN] 的字符串 —— 真实的 sha256 hex
/// 完全落在这个集合里，不放行的都是脏数据，按「没有键」处理。
pub fn is_safe_cache_key(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= MAX_CACHE_KEY_LEN
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 缓存文件名：`<song_id>-<audio_hash>.mp3`（画布原文）。
///
/// 调用方必须先过 [is_safe_cache_key]，否则拼出来的不是文件名而是路径。
pub fn cache_file_name(song_id: i64, audio_hash: &str) -> String {
    format!("{song_id}-{audio_hash}.mp3")
}

/// 缓存文件的完整路径；`audio_hash` 缺失或不安全 → `None`（不落缓存）。
pub fn cache_path(cache_dir: &Path, song_id: i64, audio_hash: Option<&str>) -> Option<PathBuf> {
    let hash = audio_hash?;
    if !is_safe_cache_key(hash) {
        return None;
    }
    Some(cache_dir.join(cache_file_name(song_id, hash)))
}

/// 一次缓存清理的结果（只用于启动日志与单测断言）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// 删掉的过期缓存文件数。
    pub removed: usize,
    /// 还在有效期内、原样保留的缓存文件数。
    pub kept: usize,
    /// 看着像缓存文件但动不了（权限 / 正被占用）的数量。
    pub failed: usize,
}

/// `name` 是不是本模块产出的缓存文件名（`<song_id>-<safe audio_hash>.mp3`）。
///
/// 单独提出来是因为它**是删除的判据**：放宽一点就是在删用户的文件。所以走
/// [cache_file_name] 的逆向拆解，且每一段都必须严格合法 —— song_id 全十进制且能装进
/// i64，hash 过 [is_safe_cache_key]。
fn is_cache_file_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".mp3") else {
        return false;
    };
    let Some((id, hash)) = stem.split_once('-') else {
        return false;
    };
    !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && id.parse::<i64>().is_ok()
        && is_safe_cache_key(hash)
}

/// 删掉 `cache_dir` 里超过 `expiry_days` 没被转码过的缓存（`audio.cache_expiry_days`）。
///
/// ## 为什么必须严格匹配文件名，而不是「清空目录」
///
/// `cache_dir` 来自用户配置（`audio.cache_dir` / `MR_CACHE_DIR`），用户完全可能把它
/// 指到一个**已经有别的东西**的目录（甚至 `~/.cache`）。无条件清目录 = 删用户的文件。
/// 所以只删**同时满足**这三条的项：
///
/// 1. 文件名严格匹配 [is_cache_file_name]（就是本模块 [cache_file_name] 的产物）；
/// 2. 是**普通文件** —— 用 `symlink_metadata` 判，软链既不算普通文件也不删
///    （否则删的是用户拿软链挂进来的东西）；
/// 3. 目录项本身能读、mtime 能取到。
///
/// ## 为什么用 mtime、为什么 `expiry_days == 0` 是「不清理」
///
/// 命中缓存时本模块**直接读文件、不写回**（见模块头「失效是键变了」），所以 mtime 就是
/// 「最后一次真正转码」的时刻，可以当「多久没用过」的保守近似。
///
/// `0` 按**关闭清理**处理，而不是「删光」：配置里 0 常被当成「不限制 / 关闭」，
/// 真去删光会是个很吓人的意外。要立刻清空缓存，用户自己删目录。
///
/// 目录不存在 / 读不了 → 返回全 0 而不报错：清理是**尽力而为**的维护动作，
/// 不该因为它让服务起不来。
///
/// `now` 由调用方注入（不用 `SystemTime::now()`）纯粹是为了可测：单测不必真的等 30 天。
pub fn prune_cache(cache_dir: &Path, expiry_days: u64, now: SystemTime) -> PruneReport {
    let mut report = PruneReport::default();
    if expiry_days == 0 {
        return report;
    }
    let max_age = Duration::from_secs(expiry_days.saturating_mul(24 * 60 * 60));

    let Ok(entries) = fs::read_dir(cache_dir) else {
        return report;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !is_cache_file_name(name) {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            report.failed += 1;
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else {
            report.failed += 1;
            continue;
        };
        // mtime 在未来（时钟回拨 / 手工 touch）时 duration_since 会 Err，按「刚用过」处理。
        if now.duration_since(mtime).unwrap_or_default() < max_age {
            report.kept += 1;
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => report.removed += 1,
            Err(_) => report.failed += 1,
        }
    }
    report
}

/// 构造 ffmpeg 参数（不含 argv[0]）。逐条理由：
///
/// | 参数 | 作用 / 为什么需要 |
/// |------|------------------|
/// | `-hide_banner` | 不打印版权横幅，stderr 更干净 |
/// | `-nostdin` | 不让 ffmpeg 读 stdin；缺了它子进程可能因等交互输入而挂住 |
/// | `-y` | 目标已存在（同名临时文件残留）时直接覆盖，不阻塞在提问上 |
/// | `-i <src>` | 输入文件 |
/// | `-vn` | 丢弃视频流。FLAC/MP3 里的内嵌封面在 ffmpeg 眼里就是一条视频流，不丢的话会原样搬进输出，白白放大体积 |
/// | `-map_metadata -1` | 不复制任何元数据。曲目信息前端已经从库接口拿到，转码产物只用于播放；复制元数据会把大封面图也带进去 |
/// | `-codec:a libmp3lame` | 明确指定 MP3 编码器，不靠输出扩展名猜 |
/// | `-b:a 192k` | 目标码率（见 [MP3_BITRATE] 的说明） |
/// | `-f mp3` | 显式指定封装格式；临时文件名是 `.part`，ffmpeg 无法从扩展名判断 |
/// | `<out>` | 输出文件 |
pub fn ffmpeg_args(src: &Path, out: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-hide_banner"),
        OsString::from("-nostdin"),
        OsString::from("-y"),
        OsString::from("-i"),
        src.as_os_str().to_os_string(),
        OsString::from("-vn"),
        OsString::from("-map_metadata"),
        OsString::from("-1"),
        OsString::from("-codec:a"),
        OsString::from("libmp3lame"),
        OsString::from("-b:a"),
        OsString::from(MP3_BITRATE),
        OsString::from("-f"),
        OsString::from("mp3"),
        out.as_os_str().to_os_string(),
    ]
}

/// 转码失败的原因。
///
/// 路由层按变体分状态码：转码能力不可用（前四类）→ 503，缓存目录 / IO 故障 → 500。
/// `Display` 是中文且带完整细节（含截断后的 stderr），**只进日志**。
#[derive(Debug)]
pub enum TranscodeError {
    /// 启动不了 ffmpeg（不存在 / 没有执行权限）→ 转码能力不可用
    Spawn {
        /// 配置里的可执行文件路径
        program: String,
        /// 底层 IO 错误（NotFound / PermissionDenied……）
        source: std::io::Error,
    },
    /// ffmpeg 非零退出
    Failed {
        /// 退出码；`None` = 被信号终止
        status: Option<i32>,
        /// 截断后的 stderr，仅用于日志
        stderr: String,
    },
    /// 超过 [TRANSCODE_TIMEOUT] 被强杀
    Timeout {
        /// 超时秒数，用于错误信息
        seconds: u64,
    },
    /// ffmpeg 退出码为 0，但没有产出可用文件（0 字节或不是文件）
    NoOutput {
        /// 期望的输出路径
        path: PathBuf,
    },
    /// 缓存目录 / 读写 / rename 等文件系统错误
    Io {
        /// 正在做哪一步
        stage: &'static str,
        /// 涉及的文件
        path: PathBuf,
        /// 底层 IO 错误
        source: std::io::Error,
    },
}

impl std::fmt::Display for TranscodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranscodeError::Spawn { program, source } => {
                write!(f, "无法启动 ffmpeg（{program}）：{source}")
            }
            TranscodeError::Failed { status, stderr } => match status {
                Some(code) => write!(f, "ffmpeg 转码失败（退出码 {code}）：{stderr}"),
                None => write!(f, "ffmpeg 转码失败（进程被信号终止）：{stderr}"),
            },
            TranscodeError::Timeout { seconds } => {
                write!(f, "ffmpeg 转码超时（超过 {seconds} 秒），已强制结束")
            }
            TranscodeError::NoOutput { path } => {
                write!(f, "ffmpeg 退出码为 0 但没有产出文件：{}", path.display())
            }
            TranscodeError::Io {
                stage,
                path,
                source,
            } => write!(f, "转码缓存 {stage} 失败（{}）：{source}", path.display()),
        }
    }
}

impl std::error::Error for TranscodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TranscodeError::Spawn { source, .. } | TranscodeError::Io { source, .. } => Some(source),
            TranscodeError::Failed { .. }
            | TranscodeError::Timeout { .. }
            | TranscodeError::NoOutput { .. } => None,
        }
    }
}

/// 转码入口：命中缓存直接读盘返回（**绝不调 ffmpeg**）；未命中才转码。
///
/// * `src` 必须是调用方已经过 `FileStorage` 沙箱解析过的**真实路径**
///   （`StorageBackend::stat` 返回的 `FileStat::path`），不是数据库里的原始串；
/// * `song_id` / `audio_hash` 来自 songs 行；hash 为 None 或不可用时
///   转码到系统临时目录、返回字节、不落缓存（理由见模块文档）；
/// * `ffmpeg_path` 来自 `config.audio.ffmpeg_path`；
/// * `cache_dir` 来自 `config.audio.cache_dir`，不存在会自动创建（仅在有键、要落盘时）。
///
/// 同步阻塞（子进程 + 文件 IO），调用方必须包在 `tokio::task::spawn_blocking` 里。
pub fn transcode_to_mp3(
    src: &Path,
    song_id: i64,
    audio_hash: Option<&str>,
    ffmpeg_path: &str,
    cache_dir: &Path,
) -> Result<Vec<u8>, TranscodeError> {
    let cached = cache_path(cache_dir, song_id, audio_hash);

    // ── 命中缓存：直接读，绝不启动 ffmpeg（画布 UT 的硬要求）──────────────
    if let Some(path) = cached.as_deref() {
        match fs::metadata(path) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => {
                return fs::read(path).map_err(|source| TranscodeError::Io {
                    stage: "读取",
                    path: path.to_path_buf(),
                    source,
                });
            }
            // 0 字节 / 是目录：当作未命中重新生成。这里不删它 —— 随后的 rename 会原子覆盖。
            Ok(_) => {}
            // 不存在 / 无权限：当作未命中；真有权限问题会在下面写临时文件时报出来。
            Err(_) => {}
        }
    }

    // 有缓存键 → 临时文件也放缓存目录（保证 rename 在同一文件系统上，原子）；
    // 无缓存键 → 借用系统临时目录，转完即删，连缓存目录都不必碰。
    let (temp_dir, target) = match cached {
        Some(target) => (cache_dir.to_path_buf(), Some(target)),
        None => (std::env::temp_dir(), None),
    };
    fs::create_dir_all(&temp_dir).map_err(|source| TranscodeError::Io {
        stage: "创建目录",
        path: temp_dir.clone(),
        source,
    })?;

    let temp = temp_dir.join(format!(".transcode-{}.part", unique_stamp()));
    let result = run_and_finalize(ffmpeg_path, src, &temp, target.as_deref());
    if result.is_err() {
        // 失败路径必须清理临时文件，否则缓存目录里会堆半截产物。
        // 成功路径下 temp 已被 rename 或读取后删除，这里的 remove 是幂等兜底。
        let _ = fs::remove_file(&temp);
    }
    result
}

/// 调 ffmpeg 产出临时文件，校验后落盘 / 读回。
fn run_and_finalize(
    ffmpeg_path: &str,
    src: &Path,
    temp: &Path,
    target: Option<&Path>,
) -> Result<Vec<u8>, TranscodeError> {
    run_ffmpeg(ffmpeg_path, src, temp)?;

    // 退出码为 0 不代表一定产出了文件（假 ffmpeg / 被截断的调用都可能这样）。
    let meta = fs::metadata(temp).map_err(|source| TranscodeError::Io {
        stage: "检查产物",
        path: temp.to_path_buf(),
        source,
    })?;
    if !meta.is_file() || meta.len() == 0 {
        return Err(TranscodeError::NoOutput {
            path: temp.to_path_buf(),
        });
    }

    match target {
        // 原子落盘：同目录 rename 要么是旧文件、要么是完整的新文件，读者看不到中间态。
        Some(target) => {
            fs::rename(temp, target).map_err(|source| TranscodeError::Io {
                stage: "落盘",
                path: target.to_path_buf(),
                source,
            })?;
            fs::read(target).map_err(|source| TranscodeError::Io {
                stage: "读取",
                path: target.to_path_buf(),
                source,
            })
        }
        // 无缓存键：读回字节后立刻删掉临时文件。
        None => {
            let bytes = fs::read(temp).map_err(|source| TranscodeError::Io {
                stage: "读取临时产物",
                path: temp.to_path_buf(),
                source,
            })?;
            let _ = fs::remove_file(temp);
            Ok(bytes)
        }
    }
}

/// 启动 ffmpeg 并等它结束，带超时与 stderr 捕获。
fn run_ffmpeg(program: &str, src: &Path, out: &Path) -> Result<(), TranscodeError> {
    let mut child = Command::new(program)
        .args(ffmpeg_args(src, out))
        .stdin(Stdio::null())
        // 输出走文件，不需要 stdout；取 piped 只会多一根要读的管子。
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| TranscodeError::Spawn {
            program: program.to_string(),
            source,
        })?;

    // stderr 必须在独立线程里持续读走：管道缓冲区写满后子进程会阻塞在 write 上，
    // 父进程若只在 try_wait，就会「互相等」到超时才被杀 —— 那是真挂死，不是超时。
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut text = String::new();
            // 读失败不算致命：stderr 只用于报错信息，拿多少算多少。
            let _ = pipe.read_to_string(&mut text);
            text
        })
    });

    let deadline = Instant::now() + TRANSCODE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    join_stderr(stderr_reader);
                    return Err(TranscodeError::Timeout {
                        seconds: TRANSCODE_TIMEOUT.as_secs(),
                    });
                }
                // 20ms 粒度：一首歌的转码是秒级，这点轮询开销可以忽略，
                // 换来的是不必为了等子进程再引入一个 async 运行时依赖。
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(source) => {
                let _ = child.kill();
                let _ = child.wait();
                join_stderr(stderr_reader);
                return Err(TranscodeError::Io {
                    stage: "等待子进程",
                    path: out.to_path_buf(),
                    source,
                });
            }
        }
    };

    let stderr = join_stderr(stderr_reader);
    if !status.success() {
        return Err(TranscodeError::Failed {
            status: status.code(),
            stderr: truncate_for_log(&stderr),
        });
    }
    Ok(())
}

/// 收 stderr 读取线程的结果；线程 panic 或没取到管道都折叠成空串（不影响错误分类）。
fn join_stderr(handle: Option<std::thread::JoinHandle<String>>) -> String {
    match handle {
        Some(handle) => match handle.join() {
            Ok(text) => text,
            Err(_) => String::new(),
        },
        None => String::new(),
    }
}

/// 截断 stderr：只在 UTF-8 字符边界上切，避免切片 panic。
fn truncate_for_log(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "（ffmpeg 没有输出 stderr）".to_string();
    }
    if trimmed.chars().count() <= MAX_STDERR_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(MAX_STDERR_CHARS).collect();
    format!("{head}……（stderr 已截断）")
}

/// 进程内唯一的临时文件后缀：pid + 纳秒 + 单调序号。
///
/// 三者缺一不可：pid 区分进程，纳秒区分同一时刻的多次调用，序号兜住「系统时钟精度
/// 不够、两次调用拿到同一纳秒」的极端情况。并发请求绝不会撞同一个临时文件。
fn unique_stamp() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_nanos(),
        // 系统时钟早于 UNIX 纪元（极罕见）：退化成 0，仍有 pid + seq 保证唯一。
        Err(_) => 0,
    };
    format!("{}-{nanos}-{seq}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::watcher::test_support::TempDir;

    // ─────────────────────── ?format= 解析 ───────────────────────

    /// 缺省 / 空串 / 空白 = 不转码；mp3 大小写不敏感；其余明确报错。
    #[test]
    fn parse_format_accepts_only_mp3() {
        assert_eq!(parse_format(None), Ok(false));
        assert_eq!(parse_format(Some("")), Ok(false));
        assert_eq!(parse_format(Some("   ")), Ok(false));
        assert_eq!(parse_format(Some("mp3")), Ok(true));
        assert_eq!(parse_format(Some("MP3")), Ok(true));
        assert_eq!(parse_format(Some("Mp3")), Ok(true));
        assert_eq!(parse_format(Some(" mp3 ")), Ok(true));

        for bad in ["flac", "wav", "m4a", "aac", "mp3,flac", "mpeg"] {
            assert_eq!(
                parse_format(Some(bad)),
                Err(FormatError::Unsupported),
                "{bad:?} 必须明确拒绝，不能静默直传"
            );
        }
    }

    /// 错误文案必须是中文。
    #[test]
    fn format_error_message_is_chinese() {
        let text = FormatError::Unsupported.to_string();
        assert!(
            text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "错误文案必须是中文：{text}"
        );
    }

    // ─────────────────────── 缓存键 ───────────────────────

    /// 正常 sha256 hex / 带下划线连字符的键放行；空、超长、含路径字符的拒绝。
    #[test]
    fn cache_key_accepts_only_plain_file_name_characters() {
        assert!(is_safe_cache_key("hash-a"));
        assert!(is_safe_cache_key("deadBEEF_0123"));
        assert!(is_safe_cache_key(&"a".repeat(MAX_CACHE_KEY_LEN)));

        assert!(!is_safe_cache_key(""));
        assert!(!is_safe_cache_key(&"a".repeat(MAX_CACHE_KEY_LEN + 1)));
        // 路径穿越 / 分隔符 / 空白 / 点，一律当作「没有键」
        for bad in ["../etc/passwd", "a/b", "a\\b", "a b", "a.b", "..", "a\0b", "a\nb"] {
            assert!(!is_safe_cache_key(bad), "{bad:?} 不该被当成缓存键");
        }
    }

    /// 缓存文件名与路径就是画布原文 `<song_id>-<audio_hash>.mp3`。
    #[test]
    fn cache_path_follows_the_canvas_layout() {
        assert_eq!(cache_file_name(7, "abc"), "7-abc.mp3");
        let dir = Path::new("/var/cache/transcode");
        assert_eq!(
            cache_path(dir, 7, Some("abc")),
            Some(PathBuf::from("/var/cache/transcode/7-abc.mp3"))
        );
        // hash 为 NULL → 没有稳定键 → 不落缓存
        assert_eq!(cache_path(dir, 7, None), None);
        // hash 不安全 → 同样不落缓存（绝不能穿越出缓存目录）
        assert_eq!(cache_path(dir, 7, Some("../x")), None);
    }

    // ─────────────────────── 缓存清理（audio.cache_expiry_days）───────────────────────

    /// 只有「严格长成本模块缓存文件名」的才被当成可删对象。放宽一点就是在删用户的文件。
    #[test]
    fn prune_only_recognizes_its_own_cache_file_names() {
        assert!(is_cache_file_name("7-abc.mp3"));
        assert!(is_cache_file_name("7-deadBEEF_0123.mp3"));
        assert!(is_cache_file_name(&format!("1-{}.mp3", "a".repeat(MAX_CACHE_KEY_LEN))));

        for bad in [
            "",
            "7-abc.flac",
            "7-abc",
            "7.mp3",
            "-abc.mp3",
            "x-abc.mp3",
            "7-../x.mp3",
            "7-a.b.mp3",
            "7-.mp3",
            "7-abc.mp3.bak",
            "99999999999999999999999-abc.mp3",
            &format!("1-{}.mp3", "a".repeat(MAX_CACHE_KEY_LEN + 1)),
        ] {
            assert!(!is_cache_file_name(bad), "{bad:?} 不该被当成可删的缓存文件");
        }
    }

    /// 给文件设一个 mtime（用 std 的 File::set_times，不引依赖、不用 unsafe）。
    fn set_mtime(path: &Path, when: SystemTime) {
        let file = fs::File::options().write(true).open(path).expect("打开文件设 mtime");
        file.set_times(fs::FileTimes::new().set_modified(when))
            .expect("设 mtime");
    }

    /// 清理只删「名字像本模块缓存 **且** 已过期」的普通文件：
    /// 没过期的缓存、用户自己的文件、同名目录、软链，一律不许动。
    #[test]
    fn prune_removes_only_expired_cache_files() {
        let dir = TempDir::new("prune");
        let cache = dir.path();
        let now = SystemTime::now();
        let old = now - Duration::from_secs(60 * 24 * 60 * 60); // 60 天前

        let stale = cache.join("1-aaa.mp3"); // 过期的缓存 → 删
        let fresh = cache.join("2-bbb.mp3"); // 没过期的缓存 → 留
        let user_mp3 = cache.join("my-own-album.mp3"); // 用户自己的文件 → 留
        let user_txt = cache.join("notes.txt"); // 无关文件 → 留
        let tricky_dir = cache.join("3-ccc.mp3"); // 名字像缓存但是个目录 → 留
        let link_target = cache.join("link-target.bin"); // 软链指向它 → 必须活着
        let link = cache.join("4-ddd.mp3"); // 名字像缓存的软链 → 留

        fs::write(&stale, b"stale").expect("写");
        fs::write(&fresh, b"fresh").expect("写");
        fs::write(&user_mp3, b"user").expect("写");
        fs::write(&user_txt, b"user").expect("写");
        fs::create_dir(&tricky_dir).expect("建目录");
        fs::write(&link_target, b"target").expect("写");
        std::os::unix::fs::symlink(&link_target, &link).expect("建软链");

        set_mtime(&stale, old);
        set_mtime(&fresh, now);
        set_mtime(&user_mp3, old);
        set_mtime(&user_txt, old);
        set_mtime(&link_target, old);

        let report = prune_cache(cache, 30, now);
        assert_eq!(report.removed, 1, "只该删掉过期的 1-aaa.mp3：{report:?}");
        assert_eq!(report.kept, 1, "只该保留没过期的 2-bbb.mp3：{report:?}");
        assert_eq!(report.failed, 0, "{report:?}");

        assert!(!stale.exists(), "过期的缓存该被删");
        assert!(fresh.exists(), "没过期的缓存不能删");
        assert!(user_mp3.exists(), "**用户的文件绝不能删**");
        assert!(user_txt.exists(), "**用户的文件绝不能删**");
        assert!(tricky_dir.is_dir(), "同名目录不能被当成文件删掉");
        assert!(link_target.exists(), "软链指向的文件绝不能被删");
        assert!(
            fs::symlink_metadata(&link).is_ok(),
            "软链本身也不删（它可能是用户自己挂进缓存目录的）"
        );
    }

    /// expiry_days = 0 是「关闭清理」，不是「删光」—— 后者会是个很吓人的意外。
    #[test]
    fn zero_expiry_days_disables_pruning_instead_of_wiping() {
        let dir = TempDir::new("prune-zero");
        let cache = dir.path();
        let file = cache.join("1-aaa.mp3");
        fs::write(&file, b"x").expect("写");
        set_mtime(&file, SystemTime::now() - Duration::from_secs(365 * 24 * 60 * 60));

        let report = prune_cache(cache, 0, SystemTime::now());
        assert_eq!(report, PruneReport::default(), "0 天 = 不清理：{report:?}");
        assert!(file.exists(), "0 天绝不能删文件");
    }

    /// 目录不存在 / 根本不是目录 → 返回全 0，不报错（清理是尽力而为，不该拖垮启动）。
    #[test]
    fn prune_is_silent_when_cache_dir_is_missing() {
        let dir = TempDir::new("prune-missing");
        let missing = dir.path().join("nope");
        assert_eq!(
            prune_cache(&missing, 30, SystemTime::now()),
            PruneReport::default()
        );
        // 传一个普通文件当目录也一样：read_dir 报错 → 全 0。
        let file = dir.path().join("a-file");
        fs::write(&file, b"x").expect("写");
        assert_eq!(
            prune_cache(&file, 30, SystemTime::now()),
            PruneReport::default()
        );
    }

    // ─────────────────────── ffmpeg 参数 ───────────────────────

    /// 参数齐全且顺序正确：输入跟着 -i，输出是最后一个，编码器 / 码率 / 格式都在。
    #[test]
    fn ffmpeg_args_are_complete_and_ordered() {
        let args = ffmpeg_args(Path::new("/music/a.flac"), Path::new("/cache/1-h.mp3"));
        let text: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert_eq!(text.last().map(String::as_str), Some("/cache/1-h.mp3"));
        let i = text.iter().position(|a| a == "-i").expect("必须有 -i");
        assert_eq!(text.get(i + 1).map(String::as_str), Some("/music/a.flac"));

        for expected in ["-hide_banner", "-nostdin", "-y", "-vn", "-map_metadata", "-1"] {
            assert!(text.iter().any(|a| a == expected), "缺参数 {expected}：{text:?}");
        }
        assert!(text.windows(2).any(|w| w == ["-codec:a", "libmp3lame"]));
        assert!(text.windows(2).any(|w| w == ["-b:a", MP3_BITRATE]));
        assert!(text.windows(2).any(|w| w == ["-f", "mp3"]));
    }

    // ─────────────────────── 错误文案 ───────────────────────

    /// 每一类转码错误都有中文说明（且构造不 panic）。
    #[test]
    fn every_transcode_error_has_a_chinese_message() {
        let errors = [
            TranscodeError::Spawn {
                program: "/no/such/ffmpeg".to_string(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "not found"),
            },
            TranscodeError::Failed {
                status: Some(1),
                stderr: "boom".to_string(),
            },
            TranscodeError::Failed {
                status: None,
                stderr: "killed".to_string(),
            },
            TranscodeError::Timeout { seconds: 300 },
            TranscodeError::NoOutput {
                path: PathBuf::from("/cache/x.part"),
            },
            TranscodeError::Io {
                stage: "创建目录",
                path: PathBuf::from("/cache"),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
            },
        ];
        for error in &errors {
            let text = error.to_string();
            assert!(
                text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "错误文案必须是中文：{text}"
            );
        }
    }

    /// stderr 为空时给一句人话；超长时按字符截断且不 panic。
    #[test]
    fn stderr_is_truncated_for_logging() {
        assert_eq!(truncate_for_log("   "), "（ffmpeg 没有输出 stderr）");
        assert_eq!(truncate_for_log("  boom\n"), "boom");

        let long = "错".repeat(MAX_STDERR_CHARS + 100);
        let text = truncate_for_log(&long);
        assert!(text.starts_with(&"错".repeat(MAX_STDERR_CHARS)));
        assert!(text.ends_with("（stderr 已截断）"));
    }

    /// 并发场景下临时文件后缀不重复。
    #[test]
    fn unique_stamp_does_not_repeat() {
        let a = unique_stamp();
        let b = unique_stamp();
        assert_ne!(a, b);
    }
}
