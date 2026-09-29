pub mod scan;
pub mod doctor;
pub mod wash;
pub mod args;
pub mod io;
pub mod read_cmd;
pub mod write_cmd;
pub mod blank_cmd;
pub mod serve;

use crate::cli::args::{UsageError, UsageErrorKind};
use crate::cli::io::{CommandIO, ConsoleIO};

pub const VERSION: &str = "0.1.0";

pub const TOP_USAGE: &str = r#"music-robot 0.1.0 — 音乐服务器（曲库索引 / 音频流 / 插件化刮削）

用法: music-robot <command> [options] [file|dir]

命令:
  read   <file>            查看单文件标签（表格/JSON + 警告）
  write  <file> [options]  修改单文件指定字段（局部编辑：只动点名，其余原样保留）
  blank  <file> [options]  重建成标准空标签（所有信息清空，音频不动）
  doctor [dir]               环境自检 + 全库体检（只读）
  scan   <dir>             批量扫描分级（ok/warn/rejected/broken，只读）
  wash   <dir> [options]   批量清洗（默认 preview；--apply 才落盘）
  serve  [options]         启动 HTTP 服务（前台运行，Ctrl-C 退出）

全局:
  -h, --help     本帮助
  -V, --version  版本号"#;

/// CLI 顶层分发。返回进程退出码（2 = 用法错误，1 = 运行期错误，0 = 成功）。
///
/// ⚠️ 返回码而非进程退出：本函数是纯库函数，Web/测试直接调用，不起子进程。
pub fn run(argv: &[String], io: &dyn CommandIO) -> i32 {
    if argv.is_empty() { io.error(TOP_USAGE); return 2 }
    let rest: Vec<&str> = argv[1..].iter().map(|s| s.as_str()).collect();
    match argv[0].as_str() {
        "read" => match read_cmd::run_read(&rest, io) { Ok(c) => c, Err(e) => { io.error(&e.message); 2 } },
        "write" => match write_cmd::run_write(&rest, io) { Ok(c) => c, Err(e) => { io.error(&e.message); 2 } },
        "blank" => match blank_cmd::run_blank(&rest, io) { Ok(c) => c, Err(e) => { io.error(&e.message); 2 } },
        "scan" => match scan::run_scan(&rest, io) { Ok(c) => c, Err(e) => { io.error(&e.message); 2 } },
        "doctor" => match doctor::run_doctor(&rest, io) { Ok(c) => c, Err(e) => { io.error(&e.message); 2 } },
        "wash" => match wash::run_wash(&rest, io) { Ok(c) => c, Err(e) => { io.error(&e.message); 2 } },
        "serve" => match serve::run_serve(&rest, io) { Ok(c) => c, Err(e) => { io.error(&e.message); 2 } },
        "-h" | "--help" => { io.log(TOP_USAGE); 0 }
        "-V" | "--version" => { io.log(&format!("music-robot {VERSION}")); 0 }
        other => {
            io.error(&format!("未知命令: {other}\n"));
            io.error(TOP_USAGE);
            2
        }
    }
}

/// 进程入口包装（对应 TS cli.ts 的 main + 自启动块）
pub fn main_with(args: Vec<String>, io: &dyn CommandIO) -> i32 {
    let argv: Vec<String> = if args.is_empty() { Vec::new() } else { args[1..].to_vec() };
    run(&argv, io)
}

/// 便捷入口：直接用 stdout/stderr
pub fn main_console(args: Vec<String>) -> i32 { main_with(args, &ConsoleIO) }

/// UsageError 的库级判断（供集成测试断言）
pub fn is_usage_error(e: &UsageError) -> bool {
    matches!(e.kind, UsageErrorKind::UnknownFlag | UsageErrorKind::DuplicateFlag | UsageErrorKind::NotANumber | UsageErrorKind::Usage)
}
