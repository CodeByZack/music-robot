//! 路径沙箱 —— 服务端拿用户输入的路径读写文件时的唯一入口。
//!
//! 这一层为什么不是照搬 TS 的 FsLike：
//!   TS `FsLike` 的形状是 `Pick<typeof fs, 'readFileSync'|...>` —— 那是「注入一个假 fs 来做测试」的
//!   形状，接口跟着 mock 走。而这里真正的诉求是「限制一个真 fs 能碰哪里」。两件事混在同一个 trait
//!   里，mock 会反过来约束生产接口（TS 就吃了这个亏）。
//!   所以本模块拆成：能力导向的少量方法 + **路径策略在 resolve 里一次性做**。
//!
//! ## 安全属性（每条都有 tests/sandbox.rs 对应用例）
//!   1. 任何路径必须先过 `resolve()`，越界即 `FsError::Escape`，绝不退化成"无限制访问"
//!   2. 逃逸判定基于 **canonicalize（解析 symlink）** 而非字符串前缀比较 —— 只做 `starts_with("root/")`
//!      的实现在 `root/link -> /etc` 面前会直接放行
//!   3. 写侧与读侧走同一道闸：`write_file` / `rename` / `atomic_replace_fs` 全部先 resolve
//!   4. 目标不存在时按「已存在的父前缀 + 未存在的后缀」分段解析，而不是整条 canonicalize
//!      （否则新建文件永远失败）
//!
//! ## 已知的残余缺口（不藏，服务端上线前必须知道）
//!   · **TOCTOU**：resolve 与真正的 read/write 之间不是原子的，期间 symlink 被换掉仍可逃逸。
//!     彻底堵住需要 openat2(AT_SYMLINK_NOFOLLOW) 或 fd 级传递，属于内核接口选择，本模块不做。
//!     缓解：服务端应把曲库目录设为非用户可写，并只允许服务账号解析 symlink。
//!   · **不是权限模型**：本模块只回答"这个路径在不在这个库根下"，不回答"这个用户有没有权限"。
//!     多租户隔离要靠上层 token/tenant 映射到不同 root。

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum FsError {
    /// 路径解析到沙箱 root 之外（含 symlink 逃逸、`..` 穿越、root 外的绝对路径）
    Escape { requested: PathBuf, root: PathBuf },
    Io { stage: &'static str, source: io::Error },
    NotADirectory { path: PathBuf },
}

impl FsError {
    pub fn is_escape(&self) -> bool { matches!(self, FsError::Escape { .. }) }
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FsError::Escape { requested, root } =>
                write!(f, "路径越界：{} 不在允许的库根 {} 之内", requested.display(), root.display()),
            FsError::Io { stage, source } => write!(f, "文件系统操作 {stage} 失败：{source}"),
            FsError::NotADirectory { path } => write!(f, "不是目录：{}", path.display()),
        }
    }
}
impl std::error::Error for FsError {}

/// 限定在 `root` 之下的文件系统访问。`root` 必须是已存在的目录（否则 fail-closed）。
#[derive(Debug, Clone)]
pub struct PathSandbox {
    root: PathBuf,      // 原始配置值，仅用于报错可读性
    canonical: PathBuf, // 已解析的库根，逃逸判定的基准
}

impl PathSandbox {
    /// 构造沙箱。root 不存在 / 不是目录 → 报错而非退化，**绝不能静默变成无限制访问**。
    pub fn new(root: &Path) -> Result<PathSandbox, FsError> {
        let canonical = std::fs::canonicalize(root)
            .map_err(|e| FsError::Io { stage: "初始化库根", source: e })?;
        if !canonical.is_dir() {
            return Err(FsError::NotADirectory { path: canonical });
        }
        Ok(PathSandbox { root: root.to_path_buf(), canonical })
    }

    pub fn root(&self) -> &Path { &self.root }
    pub fn canonical_root(&self) -> &Path { &self.canonical }

    /// 把（可能来自用户的）路径解析为库根内的规范路径。
    ///
    /// 两步走，两步都不能省：
    /// 1. **先按逻辑语义折叠 `.`/`..`**，得到「纯下降」的组件栈。折叠后才可能做前缀归属判断——
    ///    `root/a/../..` 折叠成 `..`（库根之外），折叠前它的组件序列仍以 `root` 开头。
    /// 2. **再从顶端找最深的「真实存在」前缀并 canonicalize**，把 symlink 解析掉。
    ///    只做这一步不够：折叠前拿到的是未解析的原始路径，`root/link/../x` 里的 link 不会被解析。
    ///
    /// 注意不能沿用 `Path::file_name()` 逐层向上剥：它对以 `..` 结尾的路径返回 `None`，
    /// 会让循环提前终止、留下一段未解析的 `..` —— 第一版实现就是这么被 s02 抓到的。
    pub fn resolve(&self, p: &Path) -> Result<PathBuf, FsError> {
        let joined = if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) };

        // ① 逻辑折叠：`..` 弹栈（根之上仍在根），`.` 忽略
        let mut stack: Vec<OsString> = Vec::new();
        for comp in joined.components() {
            match comp {
                Component::RootDir | Component::Prefix(_) => stack.push(comp.as_os_str().to_os_string()),
                Component::CurDir => {}
                Component::ParentDir => { if stack.len() > 1 { stack.pop(); } }
                Component::Normal(x) => stack.push(x.to_os_string()),
            }
        }

        // ② 找最深存在的真实前缀并 canonicalize
        let mut probe = PathBuf::new();
        let mut deepest = 0usize;
        for (i, comp) in stack.iter().enumerate() {
            probe.push(comp);
            match std::fs::canonicalize(&probe) {
                Ok(_) => deepest = i + 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => break,
                Err(e) => return Err(FsError::Io { stage: "解析路径", source: e }),
            }
        }
        let mut canon_prefix = PathBuf::new();
        for comp in stack.iter().take(deepest) {
            canon_prefix.push(comp);
        }
        if deepest > 0 {
            canon_prefix = std::fs::canonicalize(&canon_prefix)
                .map_err(|e| FsError::Io { stage: "解析路径", source: e })?;
        } else {
            canon_prefix = PathBuf::from("/"); // 首个组件都不存在 → 必然不在库根下
        }

        // ③ 归属判断：已解析前缀必须是库根的祖先（或库根本身）
        let comps_of = |p: &Path| -> Vec<OsString> { p.components().map(|c| c.as_os_str().to_os_string()).collect() };
        let root_comps = comps_of(&self.canonical);
        let canon_comps = comps_of(&canon_prefix);
        if !canon_comps.starts_with(&root_comps) {
            return Err(FsError::Escape { requested: joined, root: self.canonical.clone() });
        }
        for comp in stack.iter().skip(deepest) {
            canon_prefix.push(comp);
        }
        Ok(canon_prefix)
    }

    pub fn read_file(&self, p: &Path) -> Result<Vec<u8>, FsError> {
        let target = self.resolve(p)?;
        std::fs::read(&target).map_err(|e| FsError::Io { stage: "读取文件", source: e })
    }

    pub fn write_file(&self, p: &Path, data: Vec<u8>) -> Result<(), FsError> {
        let target = self.resolve(p)?;
        std::fs::write(&target, &data).map_err(|e| FsError::Io { stage: "写入文件", source: e })
    }

    /// rename 的**两端**都要过闸 —— 只校验目标会留下"从外部搬进来 / 搬出去"的口子。
    pub fn rename(&self, from: &Path, to: &Path) -> Result<(), FsError> {
        let a = self.resolve(from)?;
        let b = self.resolve(to)?;
        std::fs::rename(&a, &b).map_err(|e| FsError::Io { stage: "重命名", source: e })
    }

    pub fn copy(&self, from: &Path, to: &Path) -> Result<u64, FsError> {
        let a = self.resolve(from)?;
        let b = self.resolve(to)?;
        std::fs::copy(&a, &b).map_err(|e| FsError::Io { stage: "复制文件", source: e })
    }

    pub fn remove_file(&self, p: &Path) -> Result<(), FsError> {
        let target = self.resolve(p)?;
        std::fs::remove_file(&target).map_err(|e| FsError::Io { stage: "删除文件", source: e })
    }

    pub fn create_dir_all(&self, p: &Path) -> Result<(), FsError> {
        let target = self.resolve(p)?;
        std::fs::create_dir_all(&target).map_err(|e| FsError::Io { stage: "创建目录", source: e })
    }

    pub fn is_file(&self, p: &Path) -> bool { self.resolve(p).map(|c| c.is_file()).unwrap_or(false) }
    pub fn metadata(&self, p: &Path) -> Result<std::fs::Metadata, FsError> {
        let target = self.resolve(p)?;
        std::fs::metadata(&target).map_err(|e| FsError::Io { stage: "读取元数据", source: e })
    }

    /// 递归列出库内文件（返回**相对库根**的路径，可直接再交给 resolve/read_file）。
    ///
    /// 走遍过程中每个条目都会经过库根校验：symlink 指向库外的条目会被跳过，
    /// 而不是被当成可读文件暴露给上层。
    pub fn list_files(&self, dir_rel: &str, exts: &[&str]) -> Result<Vec<PathBuf>, FsError> {
        let dir = self.resolve(Path::new(dir_rel))?;
        if !dir.is_dir() {
            return Err(FsError::NotADirectory { path: dir });
        }
        let exts = exts.iter().map(|s| s.trim_start_matches('.').to_ascii_lowercase()).collect::<Vec<_>>();
        let mut out = Vec::new();
        walk(&dir, &self.canonical, &exts, &mut out)?;
        Ok(out)
    }
}

fn walk(dir: &Path, root: &Path, exts: &[String], out: &mut Vec<PathBuf>) -> Result<(), FsError> {
    let entries = std::fs::read_dir(dir).map_err(|e| FsError::Io { stage: "列目录", source: e })?;
    for entry in entries {
        let entry = match entry { Ok(e) => e, Err(_) => continue };
        let full = entry.path();
        // 跟随 symlink 后再判归属：指向库外的连接直接跳过
        let canon = match std::fs::canonicalize(&full) {
            Ok(c) => c,
            Err(_) => continue, // 断链 symlink / 无权限
        };
        if !canon.starts_with(root) {
            continue;
        }
        if canon.is_dir() {
            walk(&canon, root, exts, out)?;
        } else if canon.is_file() {
            let ext = canon.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
            if exts.is_empty() || exts.iter().any(|e| *e == ext) {
                if let Ok(rel) = canon.strip_prefix(root) {
                    if !rel.as_os_str().is_empty() {
                        out.push(rel.to_path_buf());
                    }
                }
            }
        }
    }
    Ok(())
}
