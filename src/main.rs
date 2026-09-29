//! music-robot CLI 进程入口。
//! 库层（read/write/blank/scan/doctor/wash）全部是返回退出码的纯函数，
//! 这里只做 stdout/stderr 桥接 + `std::process::exit`。
fn main() {
    let code = music_robot::cli::main_console(std::env::args().collect());
    std::process::exit(code as i32);
}
