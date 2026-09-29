//! S1 · storage 抽象 —— 曲库文件读写的统一入口。
//!
//! 本模块只做两件事：
//!   1. 用 trait 把「存储后端」这件事抽出来（未来换 Cloud / WebDAV 只换 impl）；
//!   2. 给本地文件系统一个实现 FileStorage，路径安全完全复用 crate::fs::PathSandbox，
//!      不在这里另写一套路径校验（两套校验必然会在某次改动后不一致）。
//!
//! ## 多库根隔离模型
//!
//! FileStorage 管理多个曲库根（来自 crate::config::StorageConfig::library_roots）。
//! 每个根各自持有一个 PathSandbox，寻址方式有两种：
//!
//!   · StoragePath::InRoot { root, rel } —— 显式指定根下标 + 根内相对路径。
//!     解析只用那一个根的沙箱；rel 里的 「..」 或指向根外的 symlink 一律 Escape，
//!     绝不会「换一个根再试一次」。这是隔离性的结构性保证：
//!     一个根下的请求不可能被解析到另一个根。
//!   · StoragePath::Absolute(p) —— 绝对路径，由 FileStorage 判断它落在哪个根。
//!     绝对路径按「canonical 之后落在哪个根」归属；若多个根嵌套，取最深的那个根。
//!     不属于任何根 → OutsideAnyRoot。
//!
//!   locate(&Path) 回答「这个绝对路径属于哪个根」。
//!
//! ## 两个必须写明的取舍
//!
//! ### 1. read_range 的 offset 语义：offset >= 文件长度 → 明确报错（而非返回空）
//!    offset + len 超出文件尾按 HTTP 语义截断到文件尾；但 offset 本身 >= 文件长度时
//!    返回 StorageError::RangeOutOfBounds。理由：RFC 9110 (HTTP Range) 规定
//!    first-byte-pos >= length 属于 unsatisfiable range，服务端应回 416。
//!    S18 的流式播放拿到这个错误可以直接映射成 416，不需要先 stat 一次再自己判断，
//!    也避免了「offset 越界返回空 200/206」这种会让客户端把错误当成功的假成功路径。
//!    例外：len == 0 是空请求，永远返回空字节（offset 任意值都不报错）。
//!
//! ### 2. 构造时某个根不存在 → 直接报错，不静默跳过
//!    PathSandbox::new 会 canonicalize 根目录，因此根必须真实存在。
//!    这里选择 fail-closed：任何一个根不存在就返回 StorageError::RootMissing。
//!    理由：库根列表是运维配置，静默跳过 = 静默丢掉整个曲库——用户看到的是「扫描结果
//!    少了半个库」，而不是一条明确的启动错误。相比之下启动直接失败至少能定位问题。

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::StorageConfig;
use crate::fs::{FsError, PathSandbox};

/// 存储层错误。
///
/// 风格与 crate::fs::FsError 保持一致：枚举 + 中文 Display + std::error::Error。
#[derive(Debug)]
pub enum StorageError {
    /// 路径解析到库根之外（含 symlink 逃逸、「..」穿越、根外绝对路径）
    Escape { requested: PathBuf, root: PathBuf },
    /// 目标存在但不是文件（目录、FIFO 等）
    NotAFile { path: PathBuf },
    /// 目标存在但不是目录
    NotADirectory { path: PathBuf },
    /// 绝对路径不属于任何一个曲库根
    OutsideAnyRoot { path: PathBuf },
    /// 库根列表为空
    EmptyRoots,
    /// 配置里的库根不存在（fail-closed，见模块文档）
    RootMissing { path: PathBuf },
    /// InRoot 寻址给了不存在的根下标
    UnknownRoot { root: usize },
    /// InRoot 变体的 rel 必须是相对路径，却给了绝对路径
    RelativePathRequired { path: PathBuf },
    /// Absolute 变体必须是绝对路径，却给了相对路径
    AbsolutePathRequired { path: PathBuf },
    /// 区间起点的 offset 已到/超过文件尾，无法满足（HTTP 语义对应 416）
    RangeOutOfBounds { path: PathBuf, offset: u64, file_len: u64 },
    /// 路径不存在
    NotFound { path: PathBuf },
    /// 其它 IO 错误
    Io { stage: &'static str, source: io::Error },
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::Escape { requested, root } => write!(
                f,
                "路径越界：{} 不在允许的库根 {} 之内",
                requested.display(),
                root.display()
            ),
            StorageError::NotAFile { path } => write!(f, "不是文件：{}", path.display()),
            StorageError::NotADirectory { path } => write!(f, "不是目录：{}", path.display()),
            StorageError::OutsideAnyRoot { path } => {
                write!(f, "路径不属于任何曲库根：{}", path.display())
            }
            StorageError::EmptyRoots => write!(f, "曲库根列表为空，至少需要配置一个库根"),
            StorageError::RootMissing { path } => write!(f, "曲库根不存在：{}", path.display()),
            StorageError::UnknownRoot { root } => write!(f, "未知的曲库根下标：{root}"),
            StorageError::RelativePathRequired { path } => write!(
                f,
                "库根内路径必须是相对路径，收到绝对路径：{}",
                path.display()
            ),
            StorageError::AbsolutePathRequired { path } => write!(
                f,
                "绝对路径寻址模式不接受相对路径：{}",
                path.display()
            ),
            StorageError::RangeOutOfBounds { path, offset, file_len } => write!(
                f,
                "字节区间越界：{} 长度为 {file_len}，请求起点为 {offset}（HTTP 语义对应 416）",
                path.display()
            ),
            StorageError::NotFound { path } => write!(f, "路径不存在：{}", path.display()),
            StorageError::Io { stage, source } => {
                write!(f, "文件系统操作 {stage} 失败：{source}")
            }
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StorageError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<FsError> for StorageError {
    fn from(e: FsError) -> Self {
        match e {
            FsError::Escape { requested, root } => StorageError::Escape { requested, root },
            FsError::Io { stage, source } => StorageError::Io { stage, source },
            FsError::NotADirectory { path } => StorageError::NotADirectory { path },
        }
    }
}

/// 存储寻址方式。见模块文档「多库根隔离模型」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoragePath {
    /// 绝对路径：后端自行判定它属于哪个库根
    Absolute(PathBuf),
    /// 显式指定库根（library_roots 里的下标）+ 根内相对路径
    InRoot { root: usize, rel: PathBuf },
}

impl StoragePath {
    /// 绝对路径寻址。
    pub fn abs<P: AsRef<Path>>(p: P) -> StoragePath {
        StoragePath::Absolute(p.as_ref().to_path_buf())
    }

    /// 指定库根 + 根内相对路径寻址。
    pub fn in_root<P: AsRef<Path>>(root: usize, rel: P) -> StoragePath {
        StoragePath::InRoot { root, rel: rel.as_ref().to_path_buf() }
    }
}

impl From<PathBuf> for StoragePath {
    fn from(p: PathBuf) -> Self {
        StoragePath::Absolute(p)
    }
}

impl From<&Path> for StoragePath {
    fn from(p: &Path) -> Self {
        StoragePath::Absolute(p.to_path_buf())
    }
}

/// 文件元信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    /// 解析后的绝对路径（symlink 已展开）
    pub path: PathBuf,
    /// 字节数
    pub len: u64,
    /// 是否普通文件
    pub is_file: bool,
    /// 是否目录
    pub is_dir: bool,
    /// 最后修改时间；平台不支持时为 None
    pub modified: Option<SystemTime>,
}

/// 目录项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryInfo {
    /// 条目名（不含父目录）
    pub name: String,
    /// 解析后的绝对路径（symlink 已展开）
    pub path: PathBuf,
    /// 是否普通文件
    pub is_file: bool,
    /// 是否目录
    pub is_dir: bool,
}

/// 存储后端抽象。
///
/// 方法都接收 StoragePath，路径安全由实现方负责；FileStorage 把这一职责
/// 完全交给 crate::fs::PathSandbox。
pub trait StorageBackend {
    /// 读取整个文件。
    fn read(&self, path: &StoragePath) -> Result<Vec<u8>, StorageError>;

    /// 读取字节区间。
    ///
    /// offset + len 超出文件尾时截断到文件尾（HTTP 语义）；
    /// offset >= 文件长度 且 len > 0 时返回 StorageError::RangeOutOfBounds；
    /// len == 0 时返回空字节。详见模块文档。
    fn read_range(&self, path: &StoragePath, offset: u64, len: u64)
        -> Result<Vec<u8>, StorageError>;

    /// 写入整个文件（父目录不存在时自动创建）。
    fn write(&self, path: &StoragePath, data: &[u8]) -> Result<(), StorageError>;

    /// 列出一级目录项，按名字升序。
    fn list(&self, path: &StoragePath) -> Result<Vec<DirEntryInfo>, StorageError>;

    /// 取元信息（大小 / 是否文件 / 修改时间）。
    fn stat(&self, path: &StoragePath) -> Result<FileStat, StorageError>;

    /// 删除文件（不递归删目录）。
    fn delete(&self, path: &StoragePath) -> Result<(), StorageError>;

    /// 回答「这个绝对路径属于哪个库根」，返回 library_roots 里的下标。
    fn locate(&self, path: &Path) -> Result<usize, StorageError>;
}

/// 本地文件系统实现，多库根隔离，路径校验复用 PathSandbox。
#[derive(Debug, Clone)]
pub struct FileStorage {
    /// 每个根一个独立沙箱，顺序与配置里的 library_roots 一致
    roots: Vec<PathSandbox>,
}

impl FileStorage {
    /// 从库根字符串构造。
    ///
    /// · 空列表 → StorageError::EmptyRoots；
    /// · 任一根不存在/不是目录 → StorageError::RootMissing 或对应错误（fail-closed，
    ///   理由见模块文档）。
    pub fn new(roots: &[String]) -> Result<FileStorage, StorageError> {
        if roots.is_empty() {
            return Err(StorageError::EmptyRoots);
        }
        let mut out = Vec::with_capacity(roots.len());
        for r in roots {
            let pb = PathBuf::from(r);
            match PathSandbox::new(&pb) {
                Ok(sandbox) => out.push(sandbox),
                Err(FsError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                    return Err(StorageError::RootMissing { path: pb });
                }
                Err(e) => return Err(StorageError::from(e)),
            }
        }
        Ok(FileStorage { roots: out })
    }

    /// 从配置构造，库根取自 storage.library_roots。
    pub fn from_config(cfg: &StorageConfig) -> Result<FileStorage, StorageError> {
        FileStorage::new(&cfg.library_roots)
    }

    /// 配置里的库根原始路径（顺序即下标）。
    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|s| s.root().to_path_buf()).collect()
    }

    /// 把寻址方式解析为「根下标 + 绝对路径」。
    fn resolve(&self, path: &StoragePath) -> Result<(usize, PathBuf), StorageError> {
        match path {
            StoragePath::InRoot { root, rel } => {
                if rel.is_absolute() {
                    return Err(StorageError::RelativePathRequired { path: rel.clone() });
                }
                let sandbox = self
                    .roots
                    .get(*root)
                    .ok_or(StorageError::UnknownRoot { root: *root })?;
                // 只用一个根的沙箱：越界就是越界，绝不回退到别的根
                let resolved = sandbox.resolve(rel)?;
                Ok((*root, resolved))
            }
            StoragePath::Absolute(p) => {
                if !p.is_absolute() {
                    return Err(StorageError::AbsolutePathRequired { path: p.clone() });
                }
                self.locate_resolved(p)
            }
        }
    }

    /// 绝对路径归属判定：取 canonical 前缀命中的最深库根。
    fn locate_resolved(&self, p: &Path) -> Result<(usize, PathBuf), StorageError> {
        let mut best: Option<(usize, PathBuf, usize)> = None;
        for (i, sandbox) in self.roots.iter().enumerate() {
            if let Ok(resolved) = sandbox.resolve(p) {
                let depth = sandbox.canonical_root().components().count();
                if best.as_ref().map_or(true, |(_, _, d)| depth > *d) {
                    best = Some((i, resolved, depth));
                }
            }
        }
        best.map(|(i, resolved, _)| (i, resolved))
            .ok_or_else(|| StorageError::OutsideAnyRoot { path: p.to_path_buf() })
    }
}

impl StorageBackend for FileStorage {
    fn read(&self, path: &StoragePath) -> Result<Vec<u8>, StorageError> {
        let (_, target) = self.resolve(path)?;
        let meta =
            std::fs::metadata(&target).map_err(|e| io_err("读取元数据", &target, e))?;
        if !meta.is_file() {
            return Err(StorageError::NotAFile { path: target });
        }
        std::fs::read(&target).map_err(|e| io_err("读取文件", &target, e))
    }

    fn read_range(
        &self,
        path: &StoragePath,
        offset: u64,
        len: u64,
    ) -> Result<Vec<u8>, StorageError> {
        let (_, target) = self.resolve(path)?;
        let meta =
            std::fs::metadata(&target).map_err(|e| io_err("读取元数据", &target, e))?;
        if !meta.is_file() {
            return Err(StorageError::NotAFile { path: target });
        }
        // len == 0 是空请求：不做区间可行性判断，直接返回空字节
        if len == 0 {
            return Ok(Vec::new());
        }
        let file_len = meta.len();
        if offset >= file_len {
            return Err(StorageError::RangeOutOfBounds { path: target, offset, file_len });
        }
        // 截断到文件尾，不越读一个字节
        let want = len.min(file_len - offset);
        let mut file = std::fs::File::open(&target).map_err(|e| io_err("打开文件", &target, e))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| io_err("定位读偏移", &target, e))?;
        let mut buf = Vec::new();
        // 用 take 限制上界：即使调用方给了巨大的 len 也不会多读
        file.take(want)
            .read_to_end(&mut buf)
            .map_err(|e| io_err("读取区间", &target, e))?;
        Ok(buf)
    }

    fn write(&self, path: &StoragePath, data: &[u8]) -> Result<(), StorageError> {
        let (_, target) = self.resolve(path)?;
        if target.is_dir() {
            return Err(StorageError::NotAFile { path: target });
        }
        // target 已解析到库根内，其父目录必然也在库根内
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err("创建父目录", parent, e))?;
        }
        std::fs::write(&target, data).map_err(|e| io_err("写入文件", &target, e))
    }

    fn list(&self, path: &StoragePath) -> Result<Vec<DirEntryInfo>, StorageError> {
        let (idx, target) = self.resolve(path)?;
        let meta =
            std::fs::metadata(&target).map_err(|e| io_err("读取目录元数据", &target, e))?;
        if !meta.is_dir() {
            return Err(StorageError::NotADirectory { path: target });
        }
        let canonical_root = self.roots[idx].canonical_root().to_path_buf();
        let entries = std::fs::read_dir(&target).map_err(|e| io_err("列目录", &target, e))?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            // 跟随 symlink 后再判归属：指向库根之外（含另一个库根）的条目直接跳过，
            // 与 fs.rs::walk 的策略一致，避免列目录把根外条目暴露给上层。
            let canon = match std::fs::canonicalize(entry.path()) {
                Ok(c) => c,
                Err(_) => continue, // 断链 symlink / 无权限
            };
            if !canon.starts_with(&canonical_root) {
                continue;
            }
            out.push(DirEntryInfo {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_file: canon.is_file(),
                is_dir: canon.is_dir(),
                path: canon,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn stat(&self, path: &StoragePath) -> Result<FileStat, StorageError> {
        let (_, target) = self.resolve(path)?;
        let meta =
            std::fs::metadata(&target).map_err(|e| io_err("读取元数据", &target, e))?;
        Ok(FileStat {
            path: target,
            len: meta.len(),
            is_file: meta.is_file(),
            is_dir: meta.is_dir(),
            modified: meta.modified().ok(),
        })
    }

    fn delete(&self, path: &StoragePath) -> Result<(), StorageError> {
        let (_, target) = self.resolve(path)?;
        let meta =
            std::fs::metadata(&target).map_err(|e| io_err("读取元数据", &target, e))?;
        if !meta.is_file() {
            return Err(StorageError::NotAFile { path: target });
        }
        std::fs::remove_file(&target).map_err(|e| io_err("删除文件", &target, e))
    }

    fn locate(&self, path: &Path) -> Result<usize, StorageError> {
        if !path.is_absolute() {
            return Err(StorageError::AbsolutePathRequired { path: path.to_path_buf() });
        }
        self.locate_resolved(path).map(|(i, _)| i)
    }
}

/// 把 io::Error 映射为 StorageError：NotFound 单独成类，其余落到 Io。
fn io_err(stage: &'static str, path: &Path, e: io::Error) -> StorageError {
    if e.kind() == io::ErrorKind::NotFound {
        StorageError::NotFound { path: path.to_path_buf() }
    } else {
        StorageError::Io { stage, source: e }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 测试用的临时根目录：Drop 时自动清理。
    ///
    /// 照抄 plugin/pool.rs 的 tmp_root 写法，避免裸清理在 /tmp 里漏文件。
    struct TempRoot(PathBuf);

    impl std::ops::Deref for TempRoot {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp_root(tag: &str) -> TempRoot {
        let dir = std::env::temp_dir()
            .join(format!("music-robot-storage-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建测试临时目录失败");
        TempRoot(dir)
    }

    /// 在 root 下写一个文件（自动建父目录）。
    fn write_at(root: &Path, rel: &str, data: &[u8]) -> PathBuf {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("建测试父目录失败");
        }
        std::fs::write(&p, data).expect("写测试文件失败");
        p
    }

    fn storage_with(roots: &[&Path]) -> FileStorage {
        let strs: Vec<String> = roots
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        FileStorage::new(&strs).expect("构造 FileStorage 失败")
    }

    /// 建 base/lib 两层，返回 (base guard, lib)。
    fn base_and_lib(tag: &str) -> (TempRoot, PathBuf) {
        let base = tmp_root(tag);
        let lib = base.join("lib");
        std::fs::create_dir_all(&lib).expect("建库根失败");
        (base, lib)
    }

    // ─────────────────────── 路径越界 ───────────────────────

    #[test]
    fn escape_via_parent_dir_is_rejected() {
        let (base, lib) = base_and_lib("escape-dotdot");
        write_at(&base, "secret.txt", b"TOP SECRET");
        let s = storage_with(&[lib.as_path()]);

        let err = s
            .read(&StoragePath::in_root(0, "../secret.txt"))
            .unwrap_err();
        assert!(matches!(err, StorageError::Escape { .. }), "实际错误：{err:?}");
    }

    #[test]
    fn escape_via_absolute_path_outside_any_root_is_rejected() {
        let (base, lib) = base_and_lib("escape-abs");
        let secret = write_at(&base, "secret.txt", b"TOP SECRET");
        let s = storage_with(&[lib.as_path()]);

        let err = s.read(&StoragePath::abs(&secret)).unwrap_err();
        assert!(
            matches!(err, StorageError::OutsideAnyRoot { .. }),
            "实际错误：{err:?}"
        );
        let err = s.stat(&StoragePath::abs(&secret)).unwrap_err();
        assert!(
            matches!(err, StorageError::OutsideAnyRoot { .. }),
            "实际错误：{err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn escape_via_symlink_out_of_root_is_rejected() {
        let (base, lib) = base_and_lib("escape-symlink");
        let secret = write_at(&base, "secret.txt", b"TOP SECRET");
        std::os::unix::fs::symlink(&secret, lib.join("link.txt")).expect("建 symlink 失败");
        let s = storage_with(&[lib.as_path()]);

        // 根内相对路径寻址：symlink 指向根外 → Escape
        let err = s.read(&StoragePath::in_root(0, "link.txt")).unwrap_err();
        assert!(matches!(err, StorageError::Escape { .. }), "实际错误：{err:?}");

        // 绝对路径寻址：canonical 后落在所有根之外 → OutsideAnyRoot
        let err = s.read(&StoragePath::abs(lib.join("link.txt"))).unwrap_err();
        assert!(
            matches!(err, StorageError::OutsideAnyRoot { .. }),
            "实际错误：{err:?}"
        );
    }

    // ─────────────────────── read / write ───────────────────────

    #[test]
    fn read_write_roundtrip() {
        let (_base, lib) = base_and_lib("rw");
        let s = storage_with(&[lib.as_path()]);

        s.write(&StoragePath::in_root(0, "a/b.bin"), b"hello").unwrap();
        assert_eq!(s.read(&StoragePath::in_root(0, "a/b.bin")).unwrap(), b"hello");
        // 绝对路径也能读到同一份内容
        assert_eq!(s.read(&StoragePath::abs(lib.join("a/b.bin"))).unwrap(), b"hello");
    }

    // ─────────────────────── read_range ───────────────────────

    fn range_fixture(tag: &str) -> (TempRoot, PathBuf, FileStorage) {
        let (base, lib) = base_and_lib(tag);
        let p = write_at(&lib, "n.bin", b"0123456789");
        let s = storage_with(&[lib.as_path()]);
        (base, p, s)
    }

    #[test]
    fn read_range_returns_requested_slice() {
        let (_base, p, s) = range_fixture("range-normal");
        assert_eq!(
            s.read_range(&StoragePath::abs(&p), 2, 4).unwrap(),
            b"2345"
        );
    }

    #[test]
    fn read_range_truncates_at_eof() {
        let (_base, p, s) = range_fixture("range-trunc");
        assert_eq!(s.read_range(&StoragePath::abs(&p), 8, 100).unwrap(), b"89");
    }

    #[test]
    fn read_range_zero_len_is_empty() {
        let (_base, p, s) = range_fixture("range-zero");
        assert_eq!(s.read_range(&StoragePath::abs(&p), 10, 0).unwrap(), b"");
        // len=0 是空请求，offset 再大也不报错
        assert_eq!(s.read_range(&StoragePath::abs(&p), 999, 0).unwrap(), b"");
    }

    #[test]
    fn read_range_offset_at_or_past_eof_is_rejected() {
        let (_base, p, s) = range_fixture("range-oob");
        let err = s.read_range(&StoragePath::abs(&p), 10, 1).unwrap_err();
        assert!(
            matches!(
                err,
                StorageError::RangeOutOfBounds { offset: 10, file_len: 10, .. }
            ),
            "实际错误：{err:?}"
        );
        let err = s.read_range(&StoragePath::abs(&p), 99, 5).unwrap_err();
        assert!(
            matches!(err, StorageError::RangeOutOfBounds { offset: 99, .. }),
            "实际错误：{err:?}"
        );
    }

    // ─────────────────────── list / stat / delete ───────────────────────

    #[test]
    fn list_returns_sorted_entries() {
        let (_base, lib) = base_and_lib("list");
        write_at(&lib, "b.txt", b"22");
        write_at(&lib, "a.txt", b"1");
        write_at(&lib, "sub/c.txt", b"333");
        let s = storage_with(&[lib.as_path()]);

        let entries = s.list(&StoragePath::abs(&lib)).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "sub"]);

        let a = entries.iter().find(|e| e.name == "a.txt").unwrap();
        assert!(a.is_file && !a.is_dir);
        assert_eq!(s.read(&StoragePath::abs(&a.path)).unwrap(), b"1");

        let sub = entries.iter().find(|e| e.name == "sub").unwrap();
        assert!(sub.is_dir && !sub.is_file);
    }

    #[test]
    fn list_on_file_is_not_a_directory() {
        let (_base, lib) = base_and_lib("list-file");
        write_at(&lib, "a.txt", b"1");
        let s = storage_with(&[lib.as_path()]);

        let err = s.list(&StoragePath::in_root(0, "a.txt")).unwrap_err();
        assert!(
            matches!(err, StorageError::NotADirectory { .. }),
            "实际错误：{err:?}"
        );
        let err = s.list(&StoragePath::in_root(0, "nope")).unwrap_err();
        assert!(
            matches!(err, StorageError::NotFound { .. }),
            "实际错误：{err:?}"
        );
    }

    #[test]
    fn stat_reports_size_kind_and_mtime() {
        let (_base, lib) = base_and_lib("stat");
        let p = write_at(&lib, "a.mp3", b"12345");
        let s = storage_with(&[lib.as_path()]);

        let st = s.stat(&StoragePath::abs(&p)).unwrap();
        assert!(st.is_file && !st.is_dir);
        assert_eq!(st.len, 5);
        assert!(st.modified.is_some());
    }

    #[test]
    fn stat_missing_path_is_not_found() {
        let (_base, lib) = base_and_lib("stat-missing");
        let s = storage_with(&[lib.as_path()]);
        let err = s.stat(&StoragePath::in_root(0, "nope.bin")).unwrap_err();
        assert!(
            matches!(err, StorageError::NotFound { .. }),
            "实际错误：{err:?}"
        );
    }

    #[test]
    fn delete_removes_file() {
        let (_base, lib) = base_and_lib("delete");
        write_at(&lib, "gone.txt", b"x");
        let s = storage_with(&[lib.as_path()]);

        s.delete(&StoragePath::in_root(0, "gone.txt")).unwrap();
        assert!(!lib.join("gone.txt").exists());
        let err = s.delete(&StoragePath::in_root(0, "gone.txt")).unwrap_err();
        assert!(
            matches!(err, StorageError::NotFound { .. }),
            "实际错误：{err:?}"
        );
    }

    #[test]
    fn delete_missing_path_is_not_found() {
        let (_base, lib) = base_and_lib("delete-missing");
        let s = storage_with(&[lib.as_path()]);
        let err = s.delete(&StoragePath::in_root(0, "nope.txt")).unwrap_err();
        assert!(
            matches!(err, StorageError::NotFound { .. }),
            "实际错误：{err:?}"
        );
    }

    // ─────────────────────── 多根隔离 ───────────────────────

    fn two_roots(tag: &str) -> (TempRoot, PathBuf, PathBuf, FileStorage) {
        let base = tmp_root(tag);
        let a = base.join("libA");
        let b = base.join("libB");
        std::fs::create_dir_all(&a).expect("建 libA 失败");
        std::fs::create_dir_all(&b).expect("建 libB 失败");
        let s = storage_with(&[a.as_path(), b.as_path()]);
        (base, a, b, s)
    }

    #[test]
    fn multi_root_same_name_files_are_isolated() {
        let (_base, a, b, s) = two_roots("iso-same-name");
        s.write(&StoragePath::in_root(0, "dup.txt"), b"AAA").unwrap();
        s.write(&StoragePath::in_root(1, "dup.txt"), b"BBB").unwrap();

        assert_eq!(s.read(&StoragePath::in_root(0, "dup.txt")).unwrap(), b"AAA");
        assert_eq!(s.read(&StoragePath::in_root(1, "dup.txt")).unwrap(), b"BBB");
        // 绝对路径寻址同样各归各的根
        assert_eq!(s.read(&StoragePath::abs(a.join("dup.txt"))).unwrap(), b"AAA");
        assert_eq!(s.read(&StoragePath::abs(b.join("dup.txt"))).unwrap(), b"BBB");
    }

    #[test]
    fn multi_root_cross_root_request_is_rejected() {
        let (_base, _a, b, s) = two_roots("iso-cross");
        s.write(&StoragePath::in_root(0, "dup.txt"), b"AAA").unwrap();
        s.write(&StoragePath::in_root(1, "dup.txt"), b"BBB").unwrap();

        // 在 A 根里用 ../ 指向 B 根 → 只用 A 的沙箱解析，越界即拒
        let err = s
            .read(&StoragePath::in_root(0, "../libB/dup.txt"))
            .unwrap_err();
        assert!(matches!(err, StorageError::Escape { .. }), "实际错误：{err:?}");

        // 直接把 B 根的绝对路径塞进 A 根 → 拒绝（InRoot 只接受相对路径）
        let err = s
            .read(&StoragePath::in_root(0, b.join("dup.txt")))
            .unwrap_err();
        assert!(
            matches!(err, StorageError::RelativePathRequired { .. }),
            "实际错误：{err:?}"
        );

        // 未知根下标
        let err = s.read(&StoragePath::in_root(9, "dup.txt")).unwrap_err();
        assert!(
            matches!(err, StorageError::UnknownRoot { root: 9 }),
            "实际错误：{err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn multi_root_symlink_into_other_root_is_rejected() {
        let (_base, a, b, s) = two_roots("iso-symlink");
        let b_file = write_at(&b, "dup.txt", b"BBB");
        std::os::unix::fs::symlink(&b_file, a.join("link.txt")).expect("建 symlink 失败");

        // A 根里的 symlink 指向 B 根文件：A 根请求必须被拒，绝不能读到 B 的内容
        let err = s.read(&StoragePath::in_root(0, "link.txt")).unwrap_err();
        assert!(matches!(err, StorageError::Escape { .. }), "实际错误：{err:?}");
    }

    #[test]
    fn locate_answers_which_root() {
        let (base, a, b, s) = two_roots("iso-locate");
        write_at(&a, "dup.txt", b"AAA");
        write_at(&b, "dup.txt", b"BBB");

        assert_eq!(s.locate(&a.join("dup.txt")).unwrap(), 0);
        assert_eq!(s.locate(&b.join("dup.txt")).unwrap(), 1);
        let err = s.locate(&base.join("nope.txt")).unwrap_err();
        assert!(
            matches!(err, StorageError::OutsideAnyRoot { .. }),
            "实际错误：{err:?}"
        );
        let err = s.locate(Path::new("relative.txt")).unwrap_err();
        assert!(
            matches!(err, StorageError::AbsolutePathRequired { .. }),
            "实际错误：{err:?}"
        );
    }

    #[test]
    fn nested_roots_pick_most_specific() {
        let base = tmp_root("iso-nested");
        let outer = base.join("lib");
        let inner = outer.join("sub");
        std::fs::create_dir_all(&inner).expect("建嵌套根失败");
        let s = storage_with(&[outer.as_path(), inner.as_path()]);
        write_at(&inner, "dup.txt", b"INNER");

        // 两个根都命中时取最深（inner，下标 1）
        assert_eq!(s.locate(&inner.join("dup.txt")).unwrap(), 1);
        assert_eq!(s.read(&StoragePath::in_root(1, "dup.txt")).unwrap(), b"INNER");
    }

    // ─────────────────────── 构造与寻址校验 ───────────────────────

    #[test]
    fn empty_root_list_is_rejected() {
        let err = FileStorage::new(&[]).unwrap_err();
        assert!(matches!(err, StorageError::EmptyRoots), "实际错误：{err:?}");
    }

    #[test]
    fn missing_root_is_rejected() {
        let base = tmp_root("missing-root");
        let roots = vec![base.join("nope").to_string_lossy().into_owned()];
        let err = FileStorage::new(&roots).unwrap_err();
        assert!(
            matches!(err, StorageError::RootMissing { .. }),
            "实际错误：{err:?}"
        );
    }

    #[test]
    fn from_config_reads_library_roots() {
        let (_base, lib) = base_and_lib("from-config");
        let cfg = StorageConfig {
            kind: "local".to_string(),
            library_roots: vec![lib.to_string_lossy().into_owned()],
        };
        let s = FileStorage::from_config(&cfg).expect("从配置构造失败");
        assert_eq!(s.roots().len(), 1);
        s.write(&StoragePath::in_root(0, "x.txt"), b"x").unwrap();
        assert_eq!(s.read(&StoragePath::in_root(0, "x.txt")).unwrap(), b"x");
    }

    #[test]
    fn absolute_variant_rejects_relative_path() {
        let (_base, lib) = base_and_lib("abs-relative");
        let s = storage_with(&[lib.as_path()]);
        let err = s.read(&StoragePath::abs("relative.txt")).unwrap_err();
        assert!(
            matches!(err, StorageError::AbsolutePathRequired { .. }),
            "实际错误：{err:?}"
        );
    }

    #[test]
    fn in_root_variant_rejects_absolute_path() {
        let (_base, lib) = base_and_lib("inroot-abs");
        let s = storage_with(&[lib.as_path()]);
        let err = s
            .read(&StoragePath::in_root(0, lib.join("x.txt")))
            .unwrap_err();
        assert!(
            matches!(err, StorageError::RelativePathRequired { .. }),
            "实际错误：{err:?}"
        );
    }
}
