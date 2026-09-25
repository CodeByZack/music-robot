//! 命令输出通道 —— 可注入（移植自 src/cli/io.ts）。
//! CLI 用 ConsoleIO；测试/服务端注入 CollectingIO 捕获结构化输出，不需要起子进程。
use std::sync::Mutex;

pub trait CommandIO {
    fn log(&self, m: &str);
    fn error(&self, m: &str);
}

pub struct ConsoleIO;
impl CommandIO for ConsoleIO {
    fn log(&self, m: &str) { println!("{m}") }
    fn error(&self, m: &str) { eprintln!("{m}") }
}

/// 测试用：把输出收集起来断言。线程安全（cargo test 默认并行）。
#[derive(Default)]
pub struct CollectingIO {
    out: Mutex<Vec<String>>,
    err: Mutex<Vec<String>>,
}
impl CollectingIO {
    pub fn new() -> Self { Self::default() }
    pub fn out(&self) -> String { self.out.lock().unwrap().join("\n") }
    pub fn err(&self) -> String { self.err.lock().unwrap().join("\n") }
}
impl CommandIO for CollectingIO {
    fn log(&self, m: &str) { self.out.lock().unwrap().push(m.to_string()) }
    fn error(&self, m: &str) { self.err.lock().unwrap().push(m.to_string()) }
}
