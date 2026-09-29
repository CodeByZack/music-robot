//! 插件注册表 —— 扫描插件目录、解析清单、组装**有序**的刮削插件表。
//!
//! 这是把 S9（清单解析）/ S10（协议）/ S11（worker 池）/ S12（沙箱）真正接起来的
//! 那一环：没有它，`ScrapeService` 只能拿到空插件表，刮削必然每一首都失败。
//!
//! ## 顺序有语义
//!
//! **用户靠文件名前缀控制尝试顺序**（如 `10-netease.js`、`20-qqmusic.js`）：
//! 画布的刮削是「顺序回退、命中即停」，所以本模块**按文件名升序**排序后再组装，
//! 绝不依赖 `read_dir` 的原始顺序（那个顺序由文件系统决定，不可复现）。
//!
//! ## 容错口径
//!
//! 单个插件坏掉**不能拖垮整体**：清单缺失 / JSON 非法 / 协议版本不认 / 类型不是
//! scraper / 工作目录建不出来，一律记进 [`LoadReport::skipped`]（带中文原因）后
//! 继续处理下一个文件。只有「整个目录读不了」才会在 report 里留下一条以目录为名的记录。
//!
//! ## 目录不存在不是错误
//!
//! 全新安装本来就没有插件。`dir` 不存在（或不是目录）时返回**空报告**，
//! 由调用方（`server::state`）决定怎么提示；这里不 panic、不报错。
//!
//! ## 只扫一层
//!
//! 插件就是目录下的一层普通文件：不递归子目录，隐藏文件（`.` 开头）与非普通文件
//! （目录 / 符号链接 / 设备……）直接忽略。符号链接一并忽略，避免顺着链接跑出插件目录。
//!
//! ## 路径会转成绝对路径
//!
//! worker 池 spawn 时会用 `current_dir(work_dir)` 把子进程切到工作目录，所以插件命令里
//! 的脚本路径**必须绝对**，否则 `node plugins/x.js` 会在 work_dir 下找不到文件。
//! 本模块在扫描前把 `dir` 转成绝对路径（相对路径按进程 cwd 解析），清单里的
//! `meta.path` 与据此推断的命令自然也是绝对路径。
//!
//! ## 与分层的一处偏离
//!
//! 返回值是 [`crate::service::ScrapePlugin`]，即 plugin 层引用了 service 层的类型，
//! 与画布「plugin → service 单向依赖」的约定相反。这样做的原因是 `ScrapePlugin`
//! （清单 + 沙箱 + 工作目录根）定义在 `service::scrape`，而本次改动不允许触碰
//! `src/service/`，无法把它下沉到 plugin 层；与其复制一个同形结构再由 state 转换，
//! 不如直接复用它，避免两处定义漂移。

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::plugin::manifest::{parse_manifest, PluginKind};
use crate::service::ScrapePlugin;

use super::sandbox::SandboxConfig;

/// 一次插件目录扫描的结果：装了什么、跳过了什么及为什么。
///
/// `loaded` 的顺序**就是**刮削的尝试顺序（顺序回退、命中即停）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadReport {
    /// 成功加载的插件，元素是**文件名**（与 [`PluginRegistry::plugins`] 一一对应、同序）。
    pub loaded: Vec<String>,
    /// 被跳过的文件：`(文件名, 中文原因)`。目录整体读不了时第一项是目录路径。
    pub skipped: Vec<(String, String)>,
}

impl LoadReport {
    /// 一个插件都没加载到（全新安装 / 目录配错都会是这个状态）。
    pub fn is_empty(&self) -> bool {
        self.loaded.is_empty()
    }

    /// 加载到的插件数。
    pub fn loaded_count(&self) -> usize {
        self.loaded.len()
    }

    /// 跳过的文件数。
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }
}

/// 已加载的刮削插件表 + 本次加载报告。
///
/// 为什么不是 `load` 直接返回 [`LoadReport`]：调用方同时需要**有序的插件表**
/// （交给 `ScrapeService`）和**报告**（启动日志 / 管理端展示），两样都得拿到。
/// 各自用 [`PluginRegistry::plugins`] / [`PluginRegistry::report`] 取。
#[derive(Debug, Clone)]
pub struct PluginRegistry {
    plugins: Vec<ScrapePlugin>,
    report: LoadReport,
}

impl PluginRegistry {
    /// 扫 `dir`、逐个解析清单、组装有序插件表，并为每个插件建好独立工作目录。
    ///
    /// `work_root` 是工作目录的**根**：每个插件用 `<work_root>/<文件名>/`，由本方法
    /// `create_dir_all` 建出来。这个目录是插件写封面等产物的地方，**必须可写**；
    /// 建不出来就把该插件记进 `skipped`（跑不了的插件不该假装加载成功）。
    ///
    /// 任何单个文件的问题都只进 `report.skipped`，不影响其它插件继续加载。
    /// 目录不存在 / 不是目录 → 空表 + 空报告（不是错误）。
    pub fn load(dir: &Path, sandbox: SandboxConfig, work_root: PathBuf) -> PluginRegistry {
        let mut report = LoadReport::default();
        let mut plugins: Vec<ScrapePlugin> = Vec::new();

        // 目录不存在 / 不是目录：全新安装的常态，空报告返回，不报错。
        if !dir.is_dir() {
            return PluginRegistry { plugins, report };
        }
        let base = absolute_path(dir);

        let entries = match fs::read_dir(&base) {
            Ok(entries) => entries,
            Err(e) => {
                // 目录存在却读不了（权限等）：如实记一条，别静默当成「没插件」。
                report
                    .skipped
                    .push((base.display().to_string(), format!("读取插件目录失败：{e}")));
                return PluginRegistry { plugins, report };
            }
        };

        // 先收集再排序：read_dir 的原始顺序不可复现，尝试顺序必须由文件名决定。
        let mut files: Vec<(OsString, PathBuf)> = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    // 单个目录项读不了也要继续，不能因此丢掉整个目录。
                    report
                        .skipped
                        .push(("<目录项>".to_string(), format!("读取目录项失败：{e}")));
                    continue;
                }
            };
            let name = entry.file_name();
            // 隐藏文件（. 开头）忽略：.gitkeep / 编辑器临时文件都不该被当插件。
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            // 只收普通文件：子目录不递归，符号链接 / 设备文件一律不当插件。
            match entry.file_type() {
                Ok(ft) if ft.is_file() => {}
                Ok(_) => continue,
                Err(e) => {
                    report.skipped.push((
                        name.to_string_lossy().into_owned(),
                        format!("读取文件类型失败：{e}"),
                    ));
                    continue;
                }
            }
            files.push((name, entry.path()));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));

        for (name, path) in files {
            let file = name.to_string_lossy().into_owned();

            let meta = match parse_manifest(&path) {
                Ok(meta) => meta,
                Err(e) => {
                    // 清单缺失 / JSON 非法 / 协议版本不认……原因原样透出（Display 已是中文）。
                    report.skipped.push((file, e.to_string()));
                    continue;
                }
            };

            // 画布还有 provider / mv_provider 等 kind，本步骤只接 scraper。
            if meta.kind != PluginKind::Scraper {
                report.skipped.push((
                    file,
                    format!(
                        "插件类型是 {}，本步骤只加载 scraper（其余类型已跳过）",
                        meta.kind.as_str()
                    ),
                ));
                continue;
            }

            // 每个插件一个独立工作目录，且必须建得出来（建不出来这个插件就跑不了）。
            let plugin_work_root = work_root.join(&name);
            if let Err(e) = fs::create_dir_all(&plugin_work_root) {
                report.skipped.push((
                    file,
                    format!("创建工作目录 {} 失败：{e}", plugin_work_root.display()),
                ));
                continue;
            }

            report.loaded.push(file);
            plugins.push(ScrapePlugin {
                meta,
                sandbox: sandbox.clone(),
                work_root: plugin_work_root,
            });
        }

        PluginRegistry { plugins, report }
    }

    /// 按尝试顺序排列的插件表（交给 `ScrapeService::new`）。
    pub fn plugins(&self) -> &[ScrapePlugin] {
        &self.plugins
    }

    /// 取出插件表（消费 registry，省一次 clone）。
    pub fn into_plugins(self) -> Vec<ScrapePlugin> {
        self.plugins
    }

    /// 本次加载报告。
    pub fn report(&self) -> &LoadReport {
        &self.report
    }
}

/// 相对路径按进程 cwd 解析成绝对路径；已经是绝对路径就原样返回。
///
/// 拿不到 cwd（理论上不会）时退回原路径：扫描仍能进行，只是命令里的脚本路径可能不是绝对。
fn absolute_path(dir: &Path) -> PathBuf {
    if dir.is_absolute() {
        return dir.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(dir),
        Err(_) => dir.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 进程内唯一的临时目录，析构即删。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mr-plugin-registry-{}-{tag}-{seq}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("建测试临时目录");
            TempDir { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// 造一个工作根（registry 只负责在其下建每个插件的工作目录）。
    fn work_root(dir: &TempDir) -> PathBuf {
        dir.path().join("work")
    }

    /// 写一个带清单的文件（json 原样嵌进注释块）。
    fn write_manifest(dir: &Path, file: &str, json: &str) {
        let text = format!("// @music-robot
// {json}
// @end
");
        fs::write(dir.join(file), text).expect("写插件文件");
    }

    /// 写一个合法的 scraper 插件。
    fn write_scraper(dir: &Path, file: &str, name: &str) {
        write_manifest(
            dir,
            file,
            &format!(r#"{{"name":"{name}","kind":"scraper","protocol":1}}"#),
        );
    }

    fn load(dir: &Path, work: &Path) -> PluginRegistry {
        PluginRegistry::load(dir, SandboxConfig::default(), work.to_path_buf())
    }

    /// 从报告里取出某个文件被跳过的原因。
    fn reason_of(report: &LoadReport, file: &str) -> String {
        report
            .skipped
            .iter()
            .find(|(name, _)| name == file)
            .map(|(_, reason)| reason.clone())
            .unwrap_or_else(|| panic!("{file} 应当出现在 skipped 里：{report:?}"))
    }

    fn assert_chinese(text: &str) {
        assert!(
            text.chars().any(|c| ('一'..='鿿').contains(&c)),
            "原因必须是中文：{text}"
        );
    }

    /// 验证：多个插件全部加载，且顺序按**文件名升序**（b.js 在 a.js 之后）。
    #[test]
    fn loads_scrapers_sorted_by_file_name() {
        let tmp = TempDir::new("order");
        let dir = tmp.path().join("plugins");
        fs::create_dir_all(&dir).expect("建插件目录");
        // 故意乱序创建，且文件名顺序与插件 name 顺序不同：断言的是**文件名**顺序。
        write_scraper(&dir, "b.js", "bbb");
        write_scraper(&dir, "a.js", "aaa");
        write_scraper(&dir, "c.py", "ccc");

        let reg = load(&dir, &work_root(&tmp));
        assert_eq!(
            reg.report().loaded,
            vec!["a.js", "b.js", "c.py"],
            "必须按文件名升序，不能依赖 read_dir 顺序"
        );
        assert!(reg.report().skipped.is_empty(), "没有坏文件不该有 skipped");
        let names: Vec<&str> = reg.plugins().iter().map(|p| p.meta.name.as_str()).collect();
        assert_eq!(names, vec!["aaa", "bbb", "ccc"], "插件表顺序与报告一致");
        assert!(!reg.report().is_empty());
        assert_eq!(reg.report().loaded_count(), 3);
        assert_eq!(reg.report().skipped_count(), 0);
    }

    /// 验证：坏插件（清单缺失 / JSON 非法 / 协议不认 / 类型不是 scraper）只进 skipped，
    /// 不影响同一目录里的好插件加载。
    #[test]
    fn bad_plugins_are_skipped_without_breaking_the_rest() {
        let tmp = TempDir::new("skips");
        let dir = tmp.path().join("plugins");
        fs::create_dir_all(&dir).expect("建插件目录");

        write_scraper(&dir, "good.js", "good");

        // 完全没有清单块。
        fs::write(dir.join("nomarker.js"), "console.log('no manifest');
").expect("写无清单插件");
        // 有标记但 JSON 非法。
        write_manifest(&dir, "badjson.js", "{ 这不是 JSON");
        // 协议版本不认。
        write_manifest(
            &dir,
            "badproto.js",
            r#"{"name":"old","kind":"scraper","protocol":2}"#,
        );
        // 类型是 provider（清单合法，但本步骤只收 scraper）。
        write_manifest(
            &dir,
            "mb.py",
            r#"{"name":"mb","kind":"provider","protocol":1}"#,
        );

        let reg = load(&dir, &work_root(&tmp));
        assert_eq!(reg.report().loaded, vec!["good.js"], "好插件必须照常加载");
        assert_eq!(reg.plugins().len(), 1, "坏插件不能混进插件表");

        let mut skipped: Vec<&str> = reg
            .report()
            .skipped
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        skipped.sort_unstable();
        assert_eq!(
            skipped,
            vec!["badjson.js", "badproto.js", "mb.py", "nomarker.js"]
        );

        for file in &skipped {
            let reason = reason_of(reg.report(), file);
            assert!(!reason.is_empty(), "{file} 的原因不能为空");
            assert_chinese(&reason);
        }
        assert!(
            reason_of(reg.report(), "nomarker.js").contains("标记"),
            "缺清单要说清是缺标记"
        );
        assert!(
            reason_of(reg.report(), "badjson.js").contains("JSON"),
            "JSON 非法要说明 JSON"
        );
        assert!(
            reason_of(reg.report(), "badproto.js").contains("协议"),
            "协议不认要说明协议"
        );
        assert!(
            reason_of(reg.report(), "mb.py").contains("provider"),
            "类型不符要说明实际类型"
        );
    }

    /// 验证：目录不存在（或不是目录）→ 空报告，且不报错、不 panic。
    #[test]
    fn missing_directory_is_an_empty_report() {
        let tmp = TempDir::new("missing");
        let missing = tmp.path().join("no-such-plugins");
        let reg = load(&missing, &work_root(&tmp));
        assert!(reg.plugins().is_empty());
        assert!(reg.report().is_empty());
        assert_eq!(reg.report().skipped, Vec::<(String, String)>::new());

        // 路径存在但是普通文件：同样按「不是目录」处理成空报告。
        let file = tmp.path().join("not-a-dir");
        fs::write(&file, b"x").expect("写普通文件");
        let reg = load(&file, &work_root(&tmp));
        assert!(reg.report().is_empty());
        assert!(reg.report().skipped.is_empty());
    }

    /// 验证：隐藏文件被忽略，子目录不被当成插件（不递归）。
    #[test]
    fn ignores_hidden_files_and_subdirectories() {
        let tmp = TempDir::new("hidden");
        let dir = tmp.path().join("plugins");
        fs::create_dir_all(&dir).expect("建插件目录");

        write_scraper(&dir, ".hidden.js", "hidden"); // 合法清单，但文件名以 . 开头
        fs::create_dir_all(dir.join("sub")).expect("建子目录");
        write_scraper(&dir.join("sub"), "inner.js", "inner"); // 子目录里的不递归
        write_scraper(&dir, "real.js", "real");

        let reg = load(&dir, &work_root(&tmp));
        assert_eq!(reg.report().loaded, vec!["real.js"]);
        assert_eq!(reg.plugins().len(), 1);
        assert!(
            reg.report().skipped.is_empty(),
            "隐藏文件与子目录是「忽略」，不是「跳过（有原因）」：{:?}",
            reg.report().skipped
        );
        assert!(!dir.join("sub/inner.js").exists() || reg.plugins()[0].meta.name == "real");
    }

    /// 验证：每个插件都有自己的工作目录，且真被创建出来（插件要往这里写产物）。
    #[test]
    fn creates_one_work_dir_per_plugin() {
        let tmp = TempDir::new("workdir");
        let dir = tmp.path().join("plugins");
        fs::create_dir_all(&dir).expect("建插件目录");
        write_scraper(&dir, "a.js", "a");
        write_scraper(&dir, "b.js", "b");

        let work = work_root(&tmp);
        let reg = load(&dir, &work);
        assert_eq!(reg.plugins().len(), 2);
        for plugin in reg.plugins() {
            assert!(
                plugin.work_root.is_dir(),
                "工作目录必须被创建：{:?}",
                plugin.work_root
            );
            assert!(
                plugin.work_root.starts_with(&work),
                "工作目录必须落在 work_root 下：{:?}",
                plugin.work_root
            );
        }
        assert_eq!(reg.plugins()[0].work_root, work.join("a.js"));
        assert_eq!(reg.plugins()[1].work_root, work.join("b.js"));
        assert_eq!(reg.plugins()[0].sandbox, SandboxConfig::default());
    }

    /// 验证：工作目录建不出来时只跳过该插件（记中文原因），不 panic、不拖垮整体。
    #[test]
    fn unusable_work_root_skips_plugin_with_reason() {
        let tmp = TempDir::new("workfail");
        let dir = tmp.path().join("plugins");
        fs::create_dir_all(&dir).expect("建插件目录");
        write_scraper(&dir, "a.js", "a");

        // 把 work_root 位置占成一个普通文件 → create_dir_all 必定失败。
        let work = tmp.path().join("work-is-a-file");
        fs::write(&work, b"x").expect("占位文件");

        let reg = load(&dir, &work);
        assert!(reg.plugins().is_empty(), "建不出工作目录就不该算加载成功");
        assert!(reg.report().is_empty());
        assert_eq!(reg.report().skipped.len(), 1);
        let (name, reason) = &reg.report().skipped[0];
        assert_eq!(name, "a.js");
        assert!(reason.contains("工作目录"), "原因要指明工作目录：{reason}");
        assert_chinese(reason);
    }
}
