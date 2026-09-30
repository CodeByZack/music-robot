//! S12 · 插件沙箱 —— 插件是**不受信任的子进程**，启动时必须戴上镣铐。
//!
//! 约束挂在 `fork` 之后、`exec` 之前（`CommandExt::pre_exec`），顺序固定不可换：
//!
//! 1. `setsid()` —— 新会话 + 新进程组。插件自己 `fork` 出来的孙进程都留在同一进程组里，
//!    于是 [`kill_process_group`] 能一次收干净，不留孤儿；
//! 2. `setrlimit` ×6 —— DATA(堆内存) / CPU / NPROC(进程数) / FSIZE(单文件) / NOFILE(句柄) / CORE；
//! 3. `prctl(PR_SET_NO_NEW_PRIVS, 1)` —— 禁止靠 setuid 程序提权；
//! 4. `prctl(PR_SET_PDEATHSIG, SIGKILL)` —— 父进程死，插件立刻跟着死；
//! 5. 仅当配置了 uid/gid 才降权：`setgroups(0, null)` → `setgid` → `setuid`（顺序不能反）。
//!
//! 两处实现选择（与规格的差异，理由都写在代码里）：
//!
//! * **配置项写 `0` = 不设置该限制**，而不是「零字节」。`RLIMIT_DATA = 0` 会让子进程连
//!   `exec` 都跑不起来（内核不给它映射任何内存），按字面理解没有任何意义。
//!   CORE 是个例外，它恒为 0（永远不落 core 文件）。
//! * **降权是「尽力而为」**：非 root 进程调 `setuid(12345)` 必然 EPERM。此时我们**不中止
//!   `exec`**（否则没权限的部署环境里插件一个都起不来），子进程保持原 uid 继续运行。
//!   想要「没权限就别启动」的调用方请先问 [`can_drop_privileges`]。
//!
//! `pre_exec` 回调的契约只能返回 `std::io::Error`，所以回调内任何一步失败都会以
//! `io::Error` 的形式从 `Command::spawn()` 冒出来（errno 见 `raw_os_error()`）；
//! [`apply`] 自己的 [`SandboxError`] 只覆盖「还没 fork 就发现配置不合法」这类问题。

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

/// RLIMIT_DATA 缺省值（MB，堆内存上限）
pub const DEFAULT_MEMORY_MB: u64 = 512;
/// RLIMIT_CPU 缺省值（秒，软硬同值）
pub const DEFAULT_CPU_SEC: u64 = 60;
/// RLIMIT_NPROC 缺省值（**该 uid** 的进程/线程总数上限）。
///
/// ⚠️ 这个值从 64 提到 4096，是实测踩出来的，别改小：
///
/// * `RLIMIT_NPROC` 是**按 uid 全局计数**的，**不是**「这个插件能开多少进程」。
///   所以它必须容纳：服务端自身 + 并发跑的每个插件进程 + 该 uid 下其它东西。
/// * node 每个进程要开 ~11 个线程。服务端默认 `scrape.concurrency = 3`，
///   正常也就 ~40 个线程；但 64 的余量小到连测试并行都撑不住 ——
///   实测 12 个并发 node：**NPROC=64 时 9 个直接 abort**，NPROC=1024 时 12 个全过。
/// * 真正用来「收掉 fork 炸弹」的是 [`kill_process_group`]（按进程组杀），
///   NPROC 只是个粗粒度的兜底 —— 别指望它精确限制单个插件。
pub const DEFAULT_PROCS: u64 = 4096;
/// RLIMIT_FSIZE 缺省值（MB，单个文件）
pub const DEFAULT_FILE_MB: u64 = 100;
/// RLIMIT_NOFILE 缺省值（打开的文件描述符数）
pub const DEFAULT_NOFILE: u64 = 256;

/// 沙箱配置。任何字段写 `0` 表示**不设置**对应的 rlimit。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxConfig {
    /// RLIMIT_DATA，单位 MB（默认 512）。**不是 RLIMIT_AS** —— 见 `install()` 里的长注释。
    pub memory_mb: u64,
    /// RLIMIT_CPU，单位秒（默认 60）
    pub cpu_sec: u64,
    /// RLIMIT_NPROC（默认 64）
    pub procs: u64,
    /// RLIMIT_FSIZE，单位 MB（默认 100）
    pub file_mb: u64,
    /// RLIMIT_NOFILE（默认 256）
    pub nofile: u64,
    /// 降权目标 uid；`None` = 不降权
    pub plugin_uid: Option<u32>,
    /// 降权目标 gid；`None` = 不降权
    pub plugin_gid: Option<u32>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            memory_mb: DEFAULT_MEMORY_MB,
            cpu_sec: DEFAULT_CPU_SEC,
            procs: DEFAULT_PROCS,
            file_mb: DEFAULT_FILE_MB,
            nofile: DEFAULT_NOFILE,
            plugin_uid: None,
            plugin_gid: None,
        }
    }
}

/// 沙箱相关错误。
///
/// 注意：`pre_exec` 里失败的步骤不走这里——`std::process` 的契约决定了它们只能以
/// `io::Error` 从 `Command::spawn()` 返回。
#[derive(Debug)]
pub enum SandboxError {
    /// 配置不合法（目前只有「字节数溢出 u64」）。此时**没有启动任何进程**。
    BadConfig(&'static str),
    /// pid 参数会误伤自己或语义不明，拒绝执行（0 / 1 / 本进程 / 超出 pid_t）
    BadPid(u32),
    /// 发信号失败（进程组不存在算成功，ESRCH 会被吞掉）
    Kill { pid: u32, errno: i32 },
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxError::BadConfig(what) => write!(f, "沙箱配置不合法（{what} 溢出）"),
            SandboxError::BadPid(pid) => write!(f, "拒绝向 pid {pid} 发信号：会误伤自己或语义不明"),
            SandboxError::Kill { pid, errno } => write!(f, "杀掉进程组 {pid} 失败（errno={errno}）"),
        }
    }
}

impl std::error::Error for SandboxError {}

// ─────────────────────────── 应用到 Command ───────────────────────────

/// 把沙箱约束挂到 `Command` 上。真正的系统调用发生在 `spawn()` 的子进程里。
///
/// 返回值只表示「配置能不能用」；子进程侧的失败会在 `spawn()` 里以 `io::Error` 出现。
pub fn apply(cmd: &mut Command, cfg: &SandboxConfig) -> Result<(), SandboxError> {
    let plan = LimitPlan::from_config(cfg)?;
    let uid = cfg.plugin_uid;
    let gid = cfg.plugin_gid;
    // SAFETY: 闭包只做异步信号安全的系统调用（setsid/setrlimit/prctl/setuid…），
    // 不做内存分配、不加锁、不碰共享状态；捕获的两份配置是 Copy 的普通数据。
    unsafe {
        cmd.pre_exec(move || plan.install(uid, gid));
    }
    Ok(())
}

/// 六条 rlimit 的字节/数量，已做溢出检查。`None` = 不设置。
#[derive(Debug, Clone, Copy)]
struct LimitPlan {
    mem_bytes: Option<u64>,
    cpu_sec: Option<u64>,
    procs: Option<u64>,
    file_bytes: Option<u64>,
    nofile: Option<u64>,
}

impl LimitPlan {
    fn from_config(cfg: &SandboxConfig) -> Result<Self, SandboxError> {
        fn mb(value: u64, what: &'static str) -> Result<Option<u64>, SandboxError> {
            if value == 0 {
                return Ok(None);
            }
            value.checked_mul(1024 * 1024).map(Some).ok_or(SandboxError::BadConfig(what))
        }
        fn count(value: u64) -> Option<u64> {
            if value == 0 {
                None
            } else {
                Some(value)
            }
        }
        Ok(LimitPlan {
            mem_bytes: mb(cfg.memory_mb, "memory_mb")?,
            cpu_sec: count(cfg.cpu_sec),
            procs: count(cfg.procs),
            file_bytes: mb(cfg.file_mb, "file_mb")?,
            nofile: count(cfg.nofile),
        })
    }

    /// 在 fork 出来的子进程里执行。**不能分配内存、不能加锁。**
    fn install(&self, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
        // 1) 新会话 + 新进程组：pid == pgid，killpg 才能一网打尽
        if unsafe { libc::setsid() } < 0 {
            return Err(io::Error::last_os_error());
        }

        // 2) rlimit ×6
        //
        // ⚠️ 内存这一项用 RLIMIT_DATA（堆）而不是 RLIMIT_AS（虚拟地址空间）——
        // 这是实测踩出来的，别改回去：
        //
        //   · RLIMIT_AS 限的是**虚拟地址空间**。V8（Node）启动时就预留约 1 GiB 虚拟内存
        //     （实测 VmPeak ≈ 1047 MiB），而**常驻内存只有 38 MiB**。
        //   · 原先默认 512 MiB 的 RLIMIT_AS 会让 node 直接
        //     `Fatal process out of memory: SegmentedTable::InitializeTable` 崩掉（signal 5 / SIGTRAP）；
        //     实测要放大到 1024 MiB 才勉强启动，而那时真实刮削请求仍会崩。
        //   · RLIMIT_DATA 限的是 brk + 匿名 mmap，也就是**真正会吃掉的堆**。
        //     512 MiB 的 DATA 下 node 插件跑完整请求毫无问题（实测 ok）。
        //
        // 换句话说：对 JS 运行时，AS 既拦不住该拦的（RSS 很小），又卡死无辜的（虚拟预留）。
        // ⚠️ 每处都要 `as libc::c_uint`：`RLIMIT_*` 常量在 Linux 上是 `c_uint`，
        //    在 macOS/BSD 上是 `c_int`（见 libc 的 bsd/apple 定义），不转就编不过。
        //    Linux 侧这个转换是恒等的，行为不变。
        if let Some(v) = self.mem_bytes {
            set_limit(libc::RLIMIT_DATA as libc::c_uint, v)?;
        }
        if let Some(v) = self.cpu_sec {
            set_limit(libc::RLIMIT_CPU as libc::c_uint, v)?;
        }
        if let Some(v) = self.procs {
            set_limit(libc::RLIMIT_NPROC as libc::c_uint, v)?;
        }
        if let Some(v) = self.file_bytes {
            set_limit(libc::RLIMIT_FSIZE as libc::c_uint, v)?;
        }
        if let Some(v) = self.nofile {
            set_limit(libc::RLIMIT_NOFILE as libc::c_uint, v)?;
        }
        set_limit(libc::RLIMIT_CORE as libc::c_uint, 0)?;

        // 3) 禁止提权：execve 遇到 setuid 程序也不再给新权限
        // 4) 父死子死：主服务被 kill -9 时插件不会变成孤儿
        //
        // ⚠️ 这两条是 **Linux 专有**（`prctl`）：macOS 与 Windows 的内核**没有等价能力**
        //    （macOS 的 Seatbelt / entitlements、Windows 的受限令牌都是另一套东西，且多在
        //    构建签名期设定），所以非 Linux 上**只能跳过** —— 插件会少这两道防护，不假装有。
        //    Linux 侧行为完全不变。
        #[cfg(target_os = "linux")]
        {
            if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, 0, 0, 0) } != 0 {
                return Err(io::Error::last_os_error());
            }

            if unsafe {
                libc::prctl(
                    libc::PR_SET_PDEATHSIG,
                    libc::SIGKILL as libc::c_ulong,
                    0,
                    0,
                    0,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }

        // 5) 降权：setgroups → setgid → setuid（顺序不能反，反了就再也改不动 gid）
        //    尽力而为：非 root 时这些调用必然 EPERM，忽略之，子进程保持原 uid 跑起来。
        if gid.is_some() {
            unsafe {
                libc::setgroups(0, std::ptr::null());
            }
        }
        if let Some(g) = gid {
            unsafe {
                libc::setgid(g as libc::gid_t);
            }
        }
        if let Some(u) = uid {
            unsafe {
                libc::setuid(u as libc::uid_t);
            }
        }
        Ok(())
    }
}

/// 设置一条 rlimit，软硬同值（硬限制也压死，否则插件可以自己把软限制调回去）。
fn set_limit(resource: libc::c_uint, value: u64) -> io::Result<()> {
    let lim = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    if unsafe { libc::setrlimit(resource as _, &lim) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// ─────────────────────────── 进程组 ───────────────────────────

/// 杀**整个进程组**（不是只杀直接子进程），防止插件 fork 出的孙进程变孤儿。
///
/// 因为 [`apply`] 里做过 `setsid`，正常情况下 `pgid == pid`。若插件自己又 `setsid`
/// 跑掉了，这里会补一发 `kill(pid, SIGKILL)` 兜底。
///
/// 进程组已经不存在（ESRCH）视为成功——杀一个已经死掉的东西不该报错。
/// 调用方仍需 `Child::wait()` 回收直接子进程，否则它会变成僵尸。
pub fn kill_process_group(pid: u32) -> Result<(), SandboxError> {
    let me = std::process::id();
    // pid 1 是 init；pid 0 在 kill 语义里是「我自己的进程组」——都不能碰
    if pid <= 1 || pid == me || pid > i32::MAX as u32 {
        return Err(SandboxError::BadPid(pid));
    }
    let target = pid as libc::pid_t;

    // 先整组杀
    if unsafe { libc::kill(-target, libc::SIGKILL) } != 0 {
        let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if errno != libc::ESRCH {
            return Err(SandboxError::Kill { pid, errno });
        }
        // ESRCH：进程组没了，但直接子进程可能自己换了组，下面兜底再来一发
    }

    // 再兜底杀直接子进程
    if unsafe { libc::kill(target, libc::SIGKILL) } != 0 {
        let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if errno != libc::ESRCH {
            return Err(SandboxError::Kill { pid, errno });
        }
    }
    Ok(())
}

/// 当前进程是否有能力降权（即 effective capability 里有 CAP_SETUID）。
///
/// 读 `/proc/self/status` 的 `CapEff`（bit 7 = CAP_SETUID）；读不到就退化成
/// 「euid 是不是 0」。任何情况下都不 panic。
pub fn can_drop_privileges() -> bool {
    /// CAP_SETUID 在 capability 位图里的编号
    const CAP_SETUID: u32 = 7;
    if let Ok(text) = std::fs::read_to_string("/proc/self/status") {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("CapEff:") {
                return u64::from_str_radix(rest.trim(), 16)
                    .map(|bits| bits & (1u64 << CAP_SETUID) != 0)
                    .unwrap_or(false);
            }
        }
    }
    unsafe { libc::geteuid() == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Read};
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    /// 写一个临时 shell 脚本，返回脚本路径（保留给更复杂的用例）
    fn tmp_script(tag: &str, body: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("music-robot-sbx-{}-{}.sh", std::process::id(), tag));
        std::fs::write(&path, body).expect("写临时脚本失败");
        path
    }

    fn tmp_path(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("music-robot-sbx-{}-{}", std::process::id(), tag));
        path
    }

    /// 一个只放开 AS 的沙箱：NPROC/CPU/FSIZE/NOFILE 都不设，
    /// 免得受测试机上同 uid 的任务数影响而假失败
    fn generous() -> SandboxConfig {
        SandboxConfig {
            memory_mb: 4096,
            cpu_sec: 0,
            procs: 0,
            file_mb: 0,
            nofile: 0,
            plugin_uid: None,
            plugin_gid: None,
        }
    }

    /// 用 /bin/sh -c 跑一段脚本，返回 (退出码, 信号, stdout)
    fn run_sh(script: &str, cfg: &SandboxConfig) -> (Option<i32>, Option<i32>, String) {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(script);
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        apply(&mut cmd, cfg).expect("apply 不应失败");
        let mut child = cmd.spawn().expect("spawn 失败");
        let mut out = String::new();
        if let Some(mut so) = child.stdout.take() {
            let _ = so.read_to_string(&mut out);
        }
        let status = child.wait().expect("wait 失败");
        (status.code(), status.signal(), out)
    }

    fn alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    // ── 纯逻辑：默认值与配置校验 ──

    #[test]
    fn defaults_match_spec() {
        let d = SandboxConfig::default();
        assert_eq!(d.memory_mb, 512);
        assert_eq!(d.cpu_sec, 60);
        assert_eq!(d.procs, 4096);
        assert_eq!(d.file_mb, 100);
        assert_eq!(d.nofile, 256);
        assert_eq!(d.plugin_uid, None);
        assert_eq!(d.plugin_gid, None);
        assert_eq!(SandboxConfig::default(), d, "Default 应当可重复");
    }

    #[test]
    fn zero_means_unset_and_values_convert_to_bytes() {
        let plan = LimitPlan::from_config(&SandboxConfig {
            memory_mb: 0,
            cpu_sec: 0,
            procs: 0,
            file_mb: 0,
            nofile: 0,
            plugin_uid: None,
            plugin_gid: None,
        })
        .expect("0 值是合法的");
        assert_eq!(plan.mem_bytes, None);
        assert_eq!(plan.cpu_sec, None);
        assert_eq!(plan.procs, None);
        assert_eq!(plan.file_bytes, None);
        assert_eq!(plan.nofile, None);

        let p = LimitPlan::from_config(&SandboxConfig {
            memory_mb: 1,
            cpu_sec: 2,
            procs: 3,
            file_mb: 4,
            nofile: 5,
            plugin_uid: None,
            plugin_gid: None,
        })
        .expect("配置合法");
        assert_eq!(p.mem_bytes, Some(1024 * 1024));
        assert_eq!(p.cpu_sec, Some(2));
        assert_eq!(p.procs, Some(3));
        assert_eq!(p.file_bytes, Some(4 * 1024 * 1024));
        assert_eq!(p.nofile, Some(5));
    }

    #[test]
    fn overflowing_limits_are_rejected_before_fork() {
        let cfg = SandboxConfig { memory_mb: u64::MAX, ..generous() };
        match apply(&mut Command::new("/bin/true"), &cfg) {
            Err(SandboxError::BadConfig("memory_mb")) => {}
            other => panic!("期望 BadConfig(memory_mb)，实际 {other:?}"),
        }

        let cfg = SandboxConfig { file_mb: u64::MAX, ..generous() };
        match apply(&mut Command::new("/bin/true"), &cfg) {
            Err(SandboxError::BadConfig("file_mb")) => {}
            other => panic!("期望 BadConfig(file_mb)，实际 {other:?}"),
        }
    }

    #[test]
    fn kill_process_group_rejects_dangerous_pids() {
        // 这几个 pid 一旦真的 kill 下去，会把我们自己或 init 干掉
        match kill_process_group(0) {
            Err(SandboxError::BadPid(0)) => {}
            other => panic!("期望 BadPid(0)，实际 {other:?}"),
        }
        match kill_process_group(1) {
            Err(SandboxError::BadPid(1)) => {}
            other => panic!("期望 BadPid(1)，实际 {other:?}"),
        }
        let me = std::process::id();
        match kill_process_group(me) {
            Err(SandboxError::BadPid(p)) => assert_eq!(p, me),
            other => panic!("期望 BadPid(本进程)，实际 {other:?}"),
        }
        match kill_process_group(u32::MAX) {
            Err(SandboxError::BadPid(_)) => {}
            other => panic!("期望 BadPid(u32::MAX)，实际 {other:?}"),
        }
        // 上面这几发都不该把我们自己弄死
        assert!(alive(me));
    }

    #[test]
    fn sandbox_error_display_is_chinese() {
        let cases = [
            SandboxError::BadConfig("memory_mb"),
            SandboxError::BadPid(0),
            SandboxError::Kill { pid: 42, errno: 1 },
        ];
        for e in &cases {
            let text = e.to_string();
            assert!(!text.is_empty());
            assert!(
                text.contains("沙箱") || text.contains("pid") || text.contains("进程组"),
                "错误消息应为中文：{text}"
            );
        }
        let _boxed: Box<dyn std::error::Error> = Box::new(SandboxError::BadPid(1));
    }

    #[test]
    fn can_drop_privileges_never_panics() {
        let can = can_drop_privileges();
        // 非 root 又没有 CAP_SETUID 时必须是 false
        if unsafe { libc::geteuid() } != 0 {
            assert!(!can, "非 root 进程不该自称能降权");
        }
        assert_eq!(can, can_drop_privileges(), "同一进程内应当稳定");
    }

    #[test]
    fn apply_only_registers_the_hook() {
        // apply 只是登记 pre_exec，不启动任何进程，也不会 panic
        let mut cmd = Command::new("/bin/true");
        let cfg = SandboxConfig::default();
        assert!(apply(&mut cmd, &cfg).is_ok());
        assert!(apply(&mut cmd, &cfg).is_ok());
    }

    // ── 真实执行：setsid / no_new_privs ──

    // 靠 /proc/self/stat 读回 pid/pgid —— /proc 是 Linux 专有。
    #[cfg(target_os = "linux")]
    #[test]
    fn setsid_makes_child_its_own_group_leader() {
        // `exec cat /proc/self/stat` 让 shell 被 cat 覆盖：
        // pid 不变，第 5 个字段是 pgid，二者相等才说明 setsid 生效
        let (code, signal, out) = run_sh("exec cat /proc/self/stat", &generous());
        assert_eq!((code, signal), (Some(0), None), "读 stat 失败：{out}");
        let fields: Vec<&str> = out.split_whitespace().collect();
        assert!(fields.len() > 5, "解析 /proc/self/stat 失败：{out:?}");
        let pid: u32 = fields[0].parse().expect("pid 字段");
        let pgid: u32 = fields[4].parse().expect("pgrp 字段");
        assert_eq!(pid, pgid, "setsid 后子进程应当自己是组长：{out:?}");
    }

    // prctl(PR_SET_NO_NEW_PRIVS) 是 Linux 专有；非 Linux 上 apply 根本不设它。
    #[cfg(target_os = "linux")]
    #[test]
    fn no_new_privs_is_set_in_child() {
        let (_, _, out) = run_sh("exec cat /proc/self/status", &generous());
        let line = out
            .lines()
            .find(|l| l.starts_with("NoNewPrivs:"))
            .expect("status 里应有 NoNewPrivs");
        assert_eq!(
            line.split(':').nth(1).map(str::trim),
            Some("1"),
            "prctl(PR_SET_NO_NEW_PRIVS) 应当生效：{line}"
        );
    }

    // ── 真实执行：rlimit ──

    #[test]
    fn rlimit_fsize_kills_oversized_write() {
        let out_file = tmp_path("fsize.out");
        let _ = std::fs::remove_file(&out_file);
        let cfg = SandboxConfig { file_mb: 1, ..generous() };
        // exec：让 dd 直接顶替 shell，这样拿到的是 dd 自己的退出状态
        let script = format!("exec dd if=/dev/zero of={} bs=1M count=4", out_file.display());
        let (code, signal, _) = run_sh(&script, &cfg);
        let size = std::fs::metadata(&out_file).map(|m| m.len()).unwrap_or(0);
        let _ = std::fs::remove_file(&out_file);

        assert_eq!(
            signal,
            Some(libc::SIGXFSZ),
            "写超过 RLIMIT_FSIZE 应当被 SIGXFSZ 杀掉（code={code:?}）"
        );
        assert!(size <= 1024 * 1024, "落盘字节数不该超过 1 MiB，实际 {size}");
    }

    #[test]
    fn rlimit_cpu_kills_runaway_process() {
        let cfg = SandboxConfig { cpu_sec: 1, ..generous() };
        let start = Instant::now();
        let (code, signal, _) = run_sh("while :; do :; done", &cfg);
        let elapsed = start.elapsed();

        assert!(
            signal == Some(libc::SIGXCPU) || signal == Some(libc::SIGKILL),
            "CPU 超限应当被 SIGXCPU/SIGKILL 杀掉（code={code:?} signal={signal:?}）"
        );
        assert!(elapsed < Duration::from_secs(10), "回收太慢：{elapsed:?}");
    }

    // 断言 RLIMIT_DATA(堆) 真的卡住 300MB 分配。这条依赖 Linux 的 DATA 语义；
    // macOS 上 DATA 只覆盖 brk，64 位 malloc 走 mmap，限制未必咬得住（未经实测）。
    #[cfg(target_os = "linux")]
    #[test]
    fn rlimit_as_blocks_large_allocation() {
        let alloc = "import sys; print('START', flush=True); b = bytearray(300*1024*1024); print('ALLOC_OK', flush=True)";
        let python = "/usr/bin/python3";

        // 对照组：堆上限够大时 300 MB 分配必须成功（证明实验组的失败是 rlimit 造成的）
        let (code, signal, out) = run_sh(
            &format!("{python} -c \"{alloc}\""),
            &SandboxConfig { memory_mb: 2048, ..generous() },
        );
        assert_eq!((code, signal), (Some(0), None), "对照组应当成功：{out}");
        assert!(out.contains("ALLOC_OK"), "对照组应当分配成功：{out}");

        // 实验组：128 MB 堆上限下，要么解释器直接起不来，要么 malloc 失败
        let (_, _, out) = run_sh(
            &format!("{python} -c \"{alloc}\""),
            &SandboxConfig { memory_mb: 128, ..generous() },
        );
        assert!(!out.contains("ALLOC_OK"), "RLIMIT_DATA=128MB 下不该能分配 300MB：{out}");
    }

    // ── 回归：默认配置必须真的能跑起受支持的运行时 ──

    /// 机器上有没有这个可执行文件（走 PATH）。
    fn on_path(bin: &str) -> bool {
        std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|d| d.join(bin).is_file()))
            .unwrap_or(false)
    }

    /// ⚠️ 回归：**默认**沙箱配置下，node / python3 必须能正常跑完一段脚本。
    ///
    /// 这条是补出来的。原先内存限制落在 `RLIMIT_AS`（虚拟地址空间），默认 512 MiB ——
    /// 而 V8 启动就要预留约 1 GiB 虚拟内存，于是 node 直接
    /// `Fatal process out of memory: SegmentedTable::InitializeTable`（signal 5 / SIGTRAP）崩掉，
    /// **所有 Node 插件在生产默认配置下都用不了**。
    ///
    /// 当时为什么没发现：pool / sandbox 的测试全都用 `memory_mb: 4096` 之类的**宽松值**，
    /// 恰好绕开了出问题的默认值。所以这里**刻意不传自定义配置**，
    /// 就用 `SandboxConfig::default()` —— 测的就是用户拿到的那份默认值。
    #[test]
    fn default_limits_let_supported_runtimes_start() {
        for (bin, script, marker) in [
            ("node", r#"node -e 'console.log("NODE_OK")'"#, "NODE_OK"),
            ("python3", r#"python3 -c 'print("PY_OK")'"#, "PY_OK"),
        ] {
            if !on_path(bin) {
                eprintln!("跳过 {bin}：机器上没有");
                continue;
            }
            let (code, signal, out) = run_sh(script, &SandboxConfig::default());
            assert_eq!(
                (code, signal),
                (Some(0), None),
                "{bin} 在**默认**沙箱配置下没能正常跑完（这对插件是致命的）：{out}"
            );
            assert!(out.contains(marker), "{bin} 没输出预期标记：{out}");
        }
    }

    /// 内存上限确实生效：堆上限卡死后，node 必须起不来（而不是把机器吃爆）。
    ///
    /// 与上面那条配对：默认值要「够用」，但也不能「形同虚设」。
    // 同上：依赖 RLIMIT_DATA 在 Linux 上的语义。
    #[cfg(target_os = "linux")]
    #[test]
    fn a_tiny_memory_limit_still_bites() {
        if !on_path("node") {
            eprintln!("跳过 node：机器上没有");
            return;
        }
        let tiny = SandboxConfig { memory_mb: 64, ..SandboxConfig::default() };
        let (code, signal, out) = run_sh(r#"node -e 'console.log("SHOULD_NOT_REACH")'"#, &tiny);
        assert!(
            !out.contains("SHOULD_NOT_REACH") || (code, signal) != (Some(0), None),
            "压到 64MiB 堆上限后 node 竟然毫发无伤，说明限制没生效：{out}"
        );
    }

    // ── 真实执行：进程组清理 ──

    #[test]
    fn kill_process_group_kills_whole_live_group() {
        let script = tmp_script("group", "sleep 30 &\necho $!\nwait\n");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(&script);
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        apply(&mut cmd, &generous()).expect("apply 不应失败");
        let mut child = cmd.spawn().expect("spawn 失败");
        let pid = child.id();

        let mut out = String::new();
        if let Some(so) = child.stdout.take() {
            let _ = std::io::BufReader::new(so).read_line(&mut out);
        }
        let grandchild: u32 = out.trim().parse().expect("应当打印孙进程 pid");
        assert!(alive(grandchild), "孙进程应当活着");
        assert!(alive(pid), "插件本体应当活着");

        kill_process_group(pid).expect("killpg 应当成功");
        let _ = child.wait();

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && (alive(pid) || alive(grandchild)) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_file(&script);
        assert!(!alive(pid), "插件本体应当已被回收");
        assert!(!alive(grandchild), "孙进程也应当被 killpg 一起清掉");
    }

    // ── 真实执行：降权 ──

    #[test]
    fn uid_without_permission_keeps_original_uid() {
        // 非 root 时 setuid/setgid 必然失败——必须「尽力而为」，不能把 spawn 搞挂
        let cfg = SandboxConfig {
            plugin_uid: Some(12345),
            plugin_gid: Some(12345),
            ..generous()
        };
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("exec id -u");
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        assert!(apply(&mut cmd, &cfg).is_ok(), "apply 不该 panic/报错");

        let mut child = cmd.spawn().expect("没权限时也必须能 spawn");
        let mut out = String::new();
        if let Some(mut so) = child.stdout.take() {
            let _ = so.read_to_string(&mut out);
        }
        let status = child.wait().expect("wait 失败");
        assert!(status.success(), "子进程本身应当正常退出");

        let got: u32 = out.trim().parse().expect("应当打印 uid");
        assert_eq!(
            got,
            unsafe { libc::geteuid() },
            "无权限降权时子进程保持原 uid，而不是静默变成别的"
        );
    }

    /// 从 /proc/self/limits 的某一行取出 (soft, hard)。Linux 专有，只服务于下面那条用例。
    #[cfg(target_os = "linux")]
    fn limit_pair(limits: &str, name: &str) -> Option<(String, String)> {
        let line = limits.lines().find(|l| l.starts_with(name))?;
        let mut it = line[name.len()..].split_whitespace();
        Some((it.next()?.to_string(), it.next()?.to_string()))
    }

    // 靠 /proc/self/limits 对账六项 rlimit —— /proc 是 Linux 专有。
    #[cfg(target_os = "linux")]
    #[test]
    fn all_six_rlimits_are_actually_installed() {
        // 用一组好认的值，直接从子进程的 /proc/self/limits 读回来对账
        let cfg = SandboxConfig {
            memory_mb: 512,
            cpu_sec: 60,
            procs: 1234,
            file_mb: 100,
            nofile: 256,
            plugin_uid: None,
            plugin_gid: None,
        };
        let (_, _, out) = run_sh("exec cat /proc/self/limits", &cfg);
        assert!(out.contains("Max data size"), "拿不到 limits：{out}");

        let pair = |name: &str| limit_pair(&out, name);
        assert_eq!(
            pair("Max data size"),
            Some(("536870912".to_string(), "536870912".to_string())),
            "RLIMIT_DATA 应为 512MiB 且软硬同值"
        );
        assert_eq!(
            pair("Max cpu time"),
            Some(("60".to_string(), "60".to_string())),
            "RLIMIT_CPU 应为 60 秒"
        );
        assert_eq!(
            pair("Max processes"),
            Some(("1234".to_string(), "1234".to_string())),
            "RLIMIT_NPROC 应被设置"
        );
        assert_eq!(
            pair("Max file size"),
            Some(("104857600".to_string(), "104857600".to_string())),
            "RLIMIT_FSIZE 应为 100MiB"
        );
        assert_eq!(
            pair("Max open files"),
            Some(("256".to_string(), "256".to_string())),
            "RLIMIT_NOFILE 应为 256"
        );
        assert_eq!(
            pair("Max core file size"),
            Some(("0".to_string(), "0".to_string())),
            "RLIMIT_CORE 必须恒为 0"
        );
    }

    // ── 真实执行：父死子死（PDEATHSIG）──

    /// 辅助用例：平时是 no-op，只有被下面那个用例用环境变量拉起时才干活。
    ///
    /// 它自己是「父进程」：带沙箱 spawn 一个长命插件，把插件 pid 写进文件，然后赖着不走，
    /// 等外面把它 kill -9 —— 那一刻插件应当被 PDEATHSIG 带走。
    #[test]
    #[ignore = "由 pdeathsig_kills_plugin_when_parent_dies 以子进程方式拉起"]
    fn pdeathsig_helper_process() {
        let Ok(pidfile) = std::env::var("MUSIC_ROBOT_PDEATHSIG_PIDFILE") else {
            return;
        };
        let mut cmd = Command::new("/bin/sh");
        // 必须是 shell 自己转圈（不能被 exec 优化掉），否则带上 PDEATHSIG 的进程就没了
        cmd.arg("-c").arg("while :; do sleep 0.2; done");
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        apply(&mut cmd, &generous()).expect("apply 失败");
        let child = cmd.spawn().expect("spawn 失败");
        std::fs::write(&pidfile, child.id().to_string()).expect("写 pid 失败");
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    // 断言父死子死。PDEATHSIG 是 Linux 专有；非 Linux 上 apply 不设它，插件会活下来。
    #[cfg(target_os = "linux")]
    #[test]
    fn pdeathsig_kills_plugin_when_parent_dies() {
        let pidfile = tmp_path("pdeathsig.pid");
        let _ = std::fs::remove_file(&pidfile);
        let exe = std::env::current_exe().expect("拿不到测试二进制路径");

        let mut helper = Command::new(exe)
            .arg("--exact")
            .arg("plugin::sandbox::tests::pdeathsig_helper_process")
            .arg("--ignored")
            .arg("--test-threads=1")
            .env("MUSIC_ROBOT_PDEATHSIG_PIDFILE", &pidfile)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("拉起辅助进程失败");

        // 等辅助进程把插件 pid 写出来
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut plugin_pid = 0u32;
        while Instant::now() < deadline {
            if let Ok(text) = std::fs::read_to_string(&pidfile) {
                if let Ok(p) = text.trim().parse::<u32>() {
                    plugin_pid = p;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if plugin_pid <= 1 {
            let _ = helper.kill();
            let _ = helper.wait();
            panic!("辅助进程没能写出插件 pid（--exact 过滤器可能没匹配上）");
        }
        assert!(alive(plugin_pid), "插件应当活着");

        // 干掉父进程（辅助进程），插件应当被内核按 PDEATHSIG=SIGKILL 一并带走
        let _ = helper.kill();
        let _ = helper.wait();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && alive(plugin_pid) {
            std::thread::sleep(Duration::from_millis(20));
        }
        let survived = alive(plugin_pid);
        // 无论成败都别把长命进程留在机器上
        let _ = kill_process_group(plugin_pid);
        let _ = std::fs::remove_file(&pidfile);
        assert!(!survived, "父进程死后插件应当被 PDEATHSIG 杀掉");
    }
}
