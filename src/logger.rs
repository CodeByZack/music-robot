//! src/logger.rs — 事件流（可注入 sink）+ NDJSON 文件 sink（移植自 src/logger.ts）
//!
//! ⚠️ 约束（ARCHITECTURE §12.6）：事件 sink 由调用方注入——CLI 终端渲染 / 服务端后台任务
//! 日志 / events.ndjson 文件，均走同一事件形状；不写死 stdout。服务端拿它做进度上报，
//! CLI 拿它做终端渲染。
use crate::cli::io::CommandIO;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// 事件种类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind { RunStart, File, RunEnd }

/// 一次清洗事件（形状与 TS WashEvent 一致，camelCase 序列化）
#[derive(Debug, Clone)]
pub struct WashEvent {
    pub ts: String,
    pub run: String,
    pub kind: EventKind,
    pub cmd: String,
    pub path: Option<String>,
    pub state: Option<String>,
    pub detail: Option<String>,
    pub counts: Option<BTreeMap<String, u64>>,
}

impl WashEvent {
    fn as_value(&self) -> Value {
        let mut v = json!({
            "ts": self.ts, "run": self.run, "kind": self.kind.as_str(), "cmd": self.cmd,
        });
        let o = v.as_object_mut().unwrap();
        if let Some(p) = &self.path { let _ = o.insert("path".into(), json!(p)); }
        if let Some(s) = &self.state { let _ = o.insert("state".into(), json!(s)); }
        if let Some(d) = &self.detail { let _ = o.insert("detail".into(), json!(d)); }
        if let Some(c) = &self.counts { let _ = o.insert("counts".into(), json!(c)); }
        v
    }
}

impl EventKind {
    fn as_str(&self) -> &'static str {
        match self { EventKind::RunStart => "run-start", EventKind::File => "file", EventKind::RunEnd => "run-end" }
    }
}

pub type EventSink = Arc<dyn Fn(&WashEvent)>;

/// 当前时间（ISO8601 UTC）
pub fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs() as i64;
            let days = secs / 86400;
            let rem = secs % 86400;
            let (year, month, day) = civil_from_days(days);
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                year, month, day, rem / 3600, (rem % 3600) / 60, rem % 60, d.subsec_millis()
            )
        }
        Err(_) => "1970-01-01T00:00:00.000Z".into(),
    }
}

/// Unix 天数 → (年, 月, 日)（Howard Hinnant civil_from_days 算法）
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 创建带 run 标识的 emit 闭包（对应 TS createLogger）
pub fn create_logger(cmd: &str, run: &str, sink: EventSink) -> EventSink {
    let cmd2 = cmd.to_string();
    let run2 = run.to_string();
    Arc::new(move |e: &WashEvent| {
        let _ = (&cmd2, &run2);
        sink(e);
    })
}

/// NDJSON 文件 sink：逐行追加；文件只开一次（不再逐事件开关）
pub fn ndjson_sink(file: impl Into<String>) -> EventSink {
    use std::io::Write;
    use std::sync::Mutex;
    let file = file.into();
    let fh: Mutex<Option<std::fs::File>> = match std::fs::OpenOptions::new().create(true).append(true).open(&file) {
        Ok(f) => Mutex::new(Some(f)),
        Err(e) => { eprintln!("事件流创建失败 ({file})：{e}——请确认父目录存在"); Mutex::new(None) }
    };
    Arc::new(move |e: &WashEvent| {
        if let Ok(mut opt) = fh.lock() {
            if let Some(f) = opt.as_mut() {
                if let Err(err) = write!(f, "{}\n", e.as_value().to_string()) {
                    eprintln!("事件流写入失败 ({file})：{err}——请确认父目录存在");
                }
            }
        }
    })
}

/// 终端 sink：人类可读单行（CLI 用）
pub fn console_sink(io: Arc<dyn CommandIO>) -> EventSink {
    Arc::new(move |e: &WashEvent| {
        match e.kind {
            EventKind::File => io.log(&format!("  [{}] {} → {}{}", e.cmd,
                e.path.as_deref().unwrap_or(""), e.state.as_deref().unwrap_or(""),
                e.detail.as_ref().map(|d| format!(" — {d}")).unwrap_or_default())),
            EventKind::RunEnd => io.log(&format!("  [events] {}", serde_json::to_string(e.counts.as_ref().unwrap_or(&BTreeMap::new())).unwrap_or_default())),
            EventKind::RunStart => {}
        }
    })
}
