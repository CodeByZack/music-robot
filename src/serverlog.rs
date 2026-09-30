//! 服务端日志：**分级** + **写文件**（按 UTC 日期切分）+ 同时回显到 stderr。
//!
//! ## 为什么不用 tracing / log 这类库
//!
//! 项目一贯的依赖纪律，而且这里要的东西很少：级别过滤 + 一行一条 + 按天落盘。
//! 全自研约 200 行，换掉一个会带来一串传递依赖的框架不划算。
//!
//! ⚠️ **别和 [`crate::logger`] 搞混**：那个是 CLI `wash` 的**事件流**（`WashEvent` +
//! NDJSON sink，给 `--events` 用的），跟服务端运行日志是两回事。
//!
//! ## 三条硬约束
//!
//! 1. **绝不 panic**。日志写不出来是运维问题（磁盘满 / 权限不对），不能让服务跟着挂 ——
//!    所有 IO 失败一律吞掉，只在 stderr 留一句提示。
//! 2. **不阻塞服务**。全局一把 `Mutex` 只护住「格式化 + 写一行 + flush」。服务端是
//!    tokio worker / spawn_blocking / 任务 OS 线程混着来的，日志必须能从任何线程写。
//! 3. **只删自己命名的文件**。清理旧日志时只认 `music-robot-<日期>.log` 这个形状的
//!    **普通文件** —— `log.dir` 是用户配的，完全可能指向一个已经有别的东西的目录，
//!    无条件清目录就是删用户的文件（转码缓存那边同理）。
//!
//! ## 文件布局
//!
//! ```text
//! <log.dir>/music-robot-2026-09-30.log      ← 一天一个，UTC 日期
//! ```
//!
//! 启动时清掉修改时间早于 `keep_days` 的旧日志；`keep_days == 0` 表示**不清理**。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 日志文件名前缀。清理旧文件时只认这个前缀 + `.log` 后缀。
const FILE_PREFIX: &str = "music-robot-";
const FILE_SUFFIX: &str = ".log";

/// 日志级别。**数值越小越严重**；配置里的级别是**下限**（比它严重的都打出来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 出错了，需要人看
    Error = 0,
    /// 有问题但不影响本次请求
    Warn = 1,
    /// 常规运行信息
    Info = 2,
    /// 排查用的细节
    Debug = 3,
    /// 极细（当前只保留级别，没有调用点）
    Trace = 4,
}

impl Level {
    /// 解析配置里的级别字符串；未知取值返回 `None`（由 `Config::validate` 报错）。
    pub fn parse(s: &str) -> Option<Level> {
        match s {
            "error" => Some(Level::Error),
            "warn" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            "trace" => Some(Level::Trace),
            _ => None,
        }
    }

    /// 大写短名，写进日志行。
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }
}

/// `log.level` 的合法取值（供 `Config::validate` 用）。
pub const LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

// ───────────────────────────── 全局 sink ─────────────────────────────

struct Sink {
    dir: PathBuf,
    file: Option<File>,
    /// 当前文件对应的 UTC 日期（`YYYY-MM-DD`）。换天时重开。
    file_date: String,
    level: Level,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// 今天的 UTC 日期（`YYYY-MM-DD`）。
///
/// 直接用 [`crate::logger::now_iso`] 的前 10 个字符 —— 那里已经算好了公历日期，
/// 没必要再写一份。⚠️ 是 **UTC**，UTC+8 的用户会看到文件在北京时间早上 8 点换天。
fn today_utc() -> String {
    crate::logger::now_iso().chars().take(10).collect()
}

fn file_name(date: &str) -> String {
    format!("{FILE_PREFIX}{date}{FILE_SUFFIX}")
}

impl Sink {
    /// 建目录 → 清旧文件 → 打开今天的文件。任何一步失败都不算致命：
    /// 文件打不开就退化成「只回显 stderr」。
    fn open(dir: &Path, level: Level, keep_days: u64) -> std::io::Result<Sink> {
        fs::create_dir_all(dir)?;
        prune_old(dir, keep_days);
        let date = today_utc();
        let path = dir.join(file_name(&date));
        let file = OpenOptions::new().create(true).append(true).open(&path).ok();
        Ok(Sink {
            dir: dir.to_path_buf(),
            file,
            file_date: date,
            level,
        })
    }

    fn allows(&self, level: Level) -> bool {
        level <= self.level
    }

    fn write_line(&mut self, line: &str) {
        // 换天了就重开文件（长时间运行的服务不重启也不能一直写同一个文件）
        let today = today_utc();
        if today != self.file_date {
            self.file_date = today.clone();
            self.file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(file_name(&today)))
                .ok();
        }
        if let Some(f) = self.file.as_mut() {
            // 每行 flush：崩溃时最后几行也留得住。本服务 QPS 很低，代价可忽略。
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    }
}

/// 清掉过期的日志文件。
///
/// **判据极严**（与转码缓存清理同一套谨慎）：必须是 `music-robot-<日期>.log` 命名的
/// **普通文件**，且修改时间早于 `keep_days` 天前。`keep_days == 0` = 关闭清理。
fn prune_old(dir: &Path, keep_days: u64) {
    if keep_days == 0 {
        return;
    }
    let Some(cutoff) = SystemTime::now().checked_sub(Duration::from_secs(keep_days * 86_400))
    else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(FILE_PREFIX) || !name.ends_with(FILE_SUFFIX) {
            continue;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if modified < cutoff {
            let _ = fs::remove_file(&path);
        }
    }
}

/// 初始化全局 sink。返回实际使用的日志文件路径。
///
/// 目录建不出来时返回 `Err`，调用方应当**只提示、不中止启动** —— 服务照跑，
/// 日志退化成只回显 stderr。
pub fn init(dir: &Path, level: Level, keep_days: u64) -> std::io::Result<PathBuf> {
    let sink = Sink::open(dir, level, keep_days)?;
    let path = sink.dir.join(file_name(&sink.file_date));
    let _ = SINK.set(Mutex::new(sink));
    Ok(path)
}

/// 日志文件是否可用（不可用时只有 stderr）。
pub fn file_enabled() -> bool {
    SINK.get()
        .and_then(|m| m.lock().ok().map(|s| s.file.is_some()))
        .unwrap_or(false)
}

/// 当前配置的级别是否放行这一条。
pub fn enabled(level: Level) -> bool {
    match SINK.get() {
        Some(m) => m.lock().map(|s| s.allows(level)).unwrap_or(true),
        None => level <= Level::Info, // init 之前按默认 info 放行
    }
}

/// 写一条日志。`target` 是短的分类标签（`http` / `job` / `auth` / `plugin` / `db`…），
/// 方便 `grep '\[job\]'` 过滤。
pub fn log(level: Level, target: &str, message: impl AsRef<str>) {
    if !enabled(level) {
        return;
    }
    let line = format!(
        "{} {:<5} [{}] {}",
        crate::logger::now_iso(),
        level.as_str(),
        target,
        message.as_ref()
    );
    // 先回显 —— 前台跑 `serve` 时这才是用户第一眼看到的东西。
    eprintln!("{line}");
    if let Some(m) = SINK.get() {
        if let Ok(mut sink) = m.lock() {
            sink.write_line(&line);
        }
    }
}

/// 出错了，需要人看。
pub fn error(target: &str, message: impl AsRef<str>) {
    log(Level::Error, target, message);
}

/// 有问题但不影响本次请求。
pub fn warn(target: &str, message: impl AsRef<str>) {
    log(Level::Warn, target, message);
}

/// 常规运行信息。
pub fn info(target: &str, message: impl AsRef<str>) {
    log(Level::Info, target, message);
}

/// 排查用的细节。
pub fn debug(target: &str, message: impl AsRef<str>) {
    log(Level::Debug, target, message);
}

/// 供日志行前缀用的「今天」，测试与别处复用。
pub fn current_file_name() -> String {
    file_name(&today_utc())
}

/// `SystemTime` → Unix 毫秒；拿不到就 0（只用于日志，不值得报错）。
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "mr-serverlog-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("建临时目录");
        p
    }

    #[test]
    fn level_parse_and_ordering() {
        assert_eq!(Level::parse("error"), Some(Level::Error));
        assert_eq!(Level::parse("warn"), Some(Level::Warn));
        assert_eq!(Level::parse("info"), Some(Level::Info));
        assert_eq!(Level::parse("debug"), Some(Level::Debug));
        assert_eq!(Level::parse("trace"), Some(Level::Trace));
        // 大小写不宽容、未知取值不静默降级
        assert_eq!(Level::parse("INFO"), None);
        assert_eq!(Level::parse("verbose"), None);
        assert_eq!(Level::parse(""), None);

        // 越严重数值越小 —— 过滤靠这个顺序
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);

        // 每个合法取值都在 LEVELS 里（validate 用同一份清单）
        for s in LEVELS {
            assert!(Level::parse(s).is_some(), "{s} 应在 LEVELS 里可解析");
        }
        assert_eq!(LEVELS.len(), 5);
    }

    #[test]
    fn level_filters_by_configured_floor() {
        // 配成 warn：warn/error 放行，info/debug 拦下
        let dir = temp_dir("filter");
        let sink = Sink::open(&dir, Level::Warn, 0).expect("开 sink");
        assert!(sink.allows(Level::Error));
        assert!(sink.allows(Level::Warn));
        assert!(!sink.allows(Level::Info));
        assert!(!sink.allows(Level::Debug));

        // 配成 debug：全放行（除 trace）
        let sink = Sink::open(&dir, Level::Debug, 0).expect("开 sink");
        assert!(sink.allows(Level::Trace) == false);
        assert!(sink.allows(Level::Info));
        assert!(sink.allows(Level::Debug));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_to_todays_file() {
        let dir = temp_dir("write");
        let mut sink = Sink::open(&dir, Level::Info, 0).expect("开 sink");
        sink.write_line("第一行");
        sink.write_line("第二行");
        let path = dir.join(file_name(&today_utc()));
        let text = fs::read_to_string(&path).expect("读回日志");
        assert!(text.contains("第一行"), "实际：{text}");
        assert!(text.contains("第二行"), "实际：{text}");
        assert!(path.file_name().unwrap().to_str().unwrap().starts_with(FILE_PREFIX));
        let _ = fs::remove_dir_all(&dir);
    }

    /// 把文件的 mtime 推到过去（清理判据看的是 mtime，不看文件名叫什么）。
    fn touch_old(path: &Path, when: SystemTime) {
        let f = OpenOptions::new().write(true).open(path).expect("打开待改 mtime 的文件");
        f.set_modified(when).expect("改 mtime");
    }

    #[test]
    fn prune_only_removes_our_own_old_files() {
        let dir = temp_dir("prune");
        let old = SystemTime::now() - Duration::from_secs(40 * 86_400);

        // 我们的旧文件（mtime 推到 40 天前）
        let ours = dir.join("music-robot-2000-01-01.log");
        fs::write(&ours, "old").expect("写旧日志");
        touch_old(&ours, old);

        // 不是我们的：别的名字、别的后缀、子目录 —— 都要原样留着
        let foreign = dir.join("someone-elses.log");
        fs::write(&foreign, "别动我").expect("写外来文件");
        touch_old(&foreign, old);
        let wrong_suffix = dir.join("music-robot-2000-01-01.txt");
        fs::write(&wrong_suffix, "别动我").expect("写外来文件");
        touch_old(&wrong_suffix, old);
        let subdir = dir.join("music-robot-2000-01-02.log");
        fs::create_dir_all(&subdir).expect("建同名子目录");

        // 我们命名、但**还没过期**的：也不能删
        let fresh = dir.join("music-robot-2026-09-30.log");
        fs::write(&fresh, "new").expect("写新日志");

        prune_old(&dir, 7);

        assert!(!ours.exists(), "我们的过期文件应被清掉");
        assert!(foreign.exists(), "不是我们命名的文件绝不能删");
        assert!(wrong_suffix.exists(), "后缀不对的不能删");
        assert!(subdir.exists(), "同名目录不能删");
        assert!(fresh.exists(), "没过期的不能删");

        // keep_days == 0 = 关闭清理
        fs::write(&ours, "old").expect("再写一份");
        touch_old(&ours, old);
        prune_old(&dir, 0);
        assert!(ours.exists(), "keep_days=0 表示不清理");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_format_carries_level_target_and_message() {
        let line = format!(
            "{} {:<5} [{}] {}",
            "2026-09-30T12:00:00.000Z",
            Level::Warn.as_str(),
            "http",
            "GET /api/library -> 401 (1ms)"
        );
        assert!(line.contains("WARN "), "级别要对齐：{line}");
        assert!(line.contains("[http]"), "要有分类标签：{line}");
        assert!(line.contains("GET /api/library"), "要有正文：{line}");
    }

    #[test]
    fn open_is_ok_even_for_uncreatable_dir() {
        // 用一个不可能建成的路径：父级是文件
        let dir = temp_dir("badparent");
        let blocker = dir.join("blocker");
        fs::write(&blocker, "x").expect("写占位文件");
        let bad = blocker.join("logs");
        // 不 panic 就行（返回 Err 由调用方提示）
        assert!(Sink::open(&bad, Level::Info, 0).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
