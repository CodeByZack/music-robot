//! copy→write→verify→rename 原子写入 —— 移植自 src/tag/write/atomic.ts
//!
//! 设计要点（与 TS 一致）：
//!   · tmp 文件与原文件**同目录** → rename 必在同一 fs 内，EXDEV 结构性不可能发生
//!   · tmp 名带 pid+ns+随机后缀 → 并发写同一文件互不覆盖（review §3.7）
//!   · copy/写/校验/rename 任一步失败都清 tmp 残留（round5 P3-5 + round6 P3-1）
use std::fs;
use std::path::Path;

/// 写入前校验：orig 与将要写入的 buf；返回 false 则中止（不 rename）。
pub type Verify<'a> = &'a dyn Fn(&[u8], &[u8]) -> bool;

#[derive(Debug)]
pub enum AtomicError {
    Io { stage: &'static str, source: std::io::Error },
    /// 完整性校验未通过 → 拒绝落盘（原文件保持不动）
    IntegrityMismatch(String),
}
impl std::fmt::Display for AtomicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AtomicError::Io { stage, source } => write!(f, "{stage} 失败：{source}"),
            AtomicError::IntegrityMismatch(p) => write!(f, "音频完整性校验失败: {p}"),
        }
    }
}
impl std::error::Error for AtomicError {}

fn tmp_name(path: &Path) -> std::path::PathBuf {
    let pid = std::process::id();
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    // 随机后缀：std 无 RNG，用纳秒 + 地址熵混合即可满足「并发不互踩」的目的
    let seed = (ts as u64) ^ ((path.as_os_str().len() as u64) << 32) ^ (pid as u64).wrapping_mul(0x9E3779B97F4A7C15);
    let r = (seed >> 17) % 0xFFFFFF;
    let stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("tagwash");
    path.with_file_name(format!("{stem}.tagwash-tmp-{pid}-{ts:x}-{r:06x}"))
}

/// 原子替换：copy 原文件到 tmp → 写新内容 → 校验 → rename 覆盖。
/// rename 或校验失败时**原文件不被破坏**。
pub fn atomic_replace(path: &Path, buf: Vec<u8>, verify: Option<Verify>) -> Result<(), AtomicError> {
    let io_err = |stage: &'static str| move |e: std::io::Error| AtomicError::Io { stage, source: e };
    let tmp = tmp_name(path);
    let orig = fs::read(path).map_err(io_err("读取原文件"))?;
    // round6 P3-1：copy 也放进 try —— copy 中途失败同样要清残留
    let body = || -> Result<(), AtomicError> {
        fs::copy(path, &tmp).map_err(io_err("复制原文件"))?;
        fs::write(&tmp, &buf).map_err(io_err("写入临时文件"))?;
        if let Some(v) = verify {
            if !v(&orig, &buf) { return Err(AtomicError::IntegrityMismatch(path.display().to_string())) }
        }
        fs::rename(&tmp, path).map_err(io_err("rename 覆盖"))?;
        Ok(())
    }();
    if body.is_err() {
        // 已被 rename 走或不可删时忽略——与原实现 catch{} 等价
        let _ = fs::remove_file(&tmp);
    }
    body
}
