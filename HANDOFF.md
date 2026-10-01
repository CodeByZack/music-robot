# HANDOFF.md — 接手这个项目的指南

面向**新的 agent 会话 / 新接手的人**。**读完这一份就够**——不需要重读历史，也不需要再找别的文档。

> ### 📌 本仓库只有这一份文档
>
> 原来的 `MIGRATION.md`（TS→Rust 移植记录）已于 2026-09 **并入本文 §11 附录并删除**。
> 它当时引用的一堆文件（`tests/differential.rs`、`tests/diff_write.rs`、`tsfixtures/`、
> `SCRIPTC-NOTES.md`）**早已不存在**，还写着「产品代码零依赖」「仅 dev-dependency 有 serde_json」
> 这类与现状**完全相反**的话。其中仍有价值的部分（踩坑、变异测试记录、未决疑点）全部保留在附录。
>
> **架构设计不在 .md 里**，全在画布 `music-server-architecture.excalidraw`。要看设计看画布。

---

## 0. 一句话现状

**`music-robot` —— 自托管的音乐服务器**（Rust）。标签读写层**全自研**，服务端跑在 axum + tokio 上。

- **后端已全部完成**：画布 28 步里的 **S1–S23 共 23 步**。能力覆盖：
  曲库扫描入库 + 多根去重 · 标签读写（CLI）· JWT 认证 · 分页查询与搜索 · 扫描 / 刮削任务 ·
  HTTP Range 流式播放 · ffmpeg 按需转码 + 缓存 · 封面 · 播放列表 · 播放历史 / 收藏 / 设置 ·
  点歌请求 · **刮削插件子系统**。
- **未完成**：S24–S26 前端（React + Vite）· S27 文件整理 · S28 集成打包；
  外加一个**明确的能力缺口**：`provider`（下载）插件 kind 未实现（见 §3「两个已知缺口」）。

### 关键数字（**最新实测，别照抄旧数字**）

| 项 | 值 |
|---|---|
| `cargo check --lib` | **0 警告** |
| 库用例（`cargo test --lib`） | **500 passed / 0 failed / 1 ignored** |
| 集成用例（`tests/`，11 个文件） | **125 passed / 0 failed** |
| 合计 | **625 passed / 0 failed / 1 ignored** |
| 端到端 API 脚本 | `node scripts/api_test.mjs` → **140 通过 / 0 失败** |
| `Cargo.lock` 包数 | **107** |
| 画布进度 | **23/28** |

### 怎么继续下一步

**28 步计划与进度全在画布** `music-server-architecture.excalidraw` 的区块 ⑫。
每张 `st_X_Y` 卡 = 一步，`backgroundColor === "#bbf7d0"` = 已完成：

```js
const els = JSON.parse(fs.readFileSync('music-server-architecture.excalidraw','utf8')).elements;
els.filter(e => /^st_\d+_\d+$/.test(e.id) && e.backgroundColor === '#bbf7d0').map(e => e.id)
```

> ⚠️ 画布里 `st_X_Y_t` 是**卡片的文字元素**、`st_phN` 是**阶段标题**，都不是步骤，别数错。

**每步的固定流程**（本项目已按这个跑完了 23 步）：

1. 从画布读该步卡片（交付 / UT / ⚠ 补充项），**再到相关区块读详细设计** ——
   卡片往往只写一半，真正的规则（比如刮削的 0.80 阈值、去重的胜负规则）藏在别的区块里。
2. 派一个**后台子代理**实现（`subagent` + `run_in_background: true`）。prompt 里必须写全：
   构建环境、**可读但不可改**的文件清单、画布规格原文、硬性规范（**生产路径无可达 unwrap**，
   口径见 §7.5 / 中文注释 / 不加依赖 / 不用 `#[allow]`）、测试要求、以及**当前测试基线**（弄红就是回归）。
3. 子代理交付后**自己独立复核**，不要采信它的自述：
   `cargo check --lib` 零警告 · `cargo test --lib` 对得上基线 · 生产路径无可达 unwrap（§7.5 有口径与复核命令）·
   关键不变量**自己做一次变异测试**（故意改坏实现，看对应测试是否变红）。
4. **值得的步骤把服务真跑起来打请求**（见 §9）。这一步抓到过单测全绿但真实存在的漏洞，别省。
5. 画布标绿 + 更新本文件的进度与基线。

**三条硬性约定**（都是踩过坑换来的）：

- **任何 cargo 命令都加 `timeout`** —— 有过测试死锁挂死 7 分半、只能手动 kill 的事故；
- **重依赖编译串行做、不与其它任务并行**（用户明确要求，NAS 扛不住）；
- **一步只允许改自己那一小块 + 一个 `mod` 文件**，且**明确禁止改其它模块**；
  发现别处有 bug 要先上报，不要顺手改。

---

## 1. 环境（先读这个，否则会卡在第一步）

**Rust 是装在 dsh 用户下的全局工具链**（非系统级，因为没 root）：

```
/vol1/@appshare/dsh/tools/rust/
├── setup.sh          # source 一下即可用 cargo/rustc
├── rustup/           # toolchains/stable-aarch64（rustc 1.98.1）
└── cargo/            # cargo 本体 + registry 依赖缓存
```

`/usr/bin/rustc` 是 apt 装的 1.63，太旧用不了。已放开 755，**其他用户也能读**。

```bash
# 方式一：项目内（附带 mt 别名）
# ⚠️ env.sh 是本机专属文件、不入库；新克隆的仓库里没有它，只有 env.example
cd /vol1/@appshare/dsh/data/music-robot && source env.sh
mt -h

# 方式二：任意位置
source /vol1/@appshare/dsh/tools/rust/setup.sh
cargo --version        # cargo 1.98.1
```

DSH 的 agent 会话（`dsh` 用户）已写进 `~/.bashrc`，**自动生效，不用手动 source**。
真人 `zackdk` 想要全局可用：`sudo ln -sfn /vol1/@appshare/dsh/tools/rust/cargo/bin/cargo /usr/local/bin/cargo`

或者直接跑编译好的二进制（**不需要 cargo**）：

```bash
B=./target/debug/music-robot      # 或 target/release/music-robot
$B -h
```

### ⚠️ 跑 cargo 一律加 `timeout`

本机是 NAS，**挂死的进程会一直占着资源**。本项目发生过一次真实事故：`cargo test`
因为一个**可重入 Mutex 自死锁**挂了 **7 分 38 秒**不返回（内核等待点 `futex_do_wait`），
只能手动 kill。形态是：

```rust
if let Some(x) = *lock(&m) {
    // ↑ if let 的临时 MutexGuard 会活到整个语句结束
    *lock(&m) = None;   // ← 同一把 std Mutex 不可重入 → 永久死锁
}
```

所以：**跑测试/构建一律写成 `timeout 300 cargo test ...`**；写代码时注意 std 的 `Mutex`
不可重入，且**绝不要在持锁状态下调用 `WorkerPool::acquire`**（它会等 worker 归还）。

### 构建/测试耗时（ARM NAS，性能有限）

| 操作 | 耗时 |
|---|---|
| 增量 `cargo check --lib` | ~15 秒 |
| 增量 `cargo build`（改了库） | ~57 秒 |
| 全量 `cargo test`（库 + 集成） | ~90 秒 |
| 单个测试文件 | 1–15 秒 |

⚠️ **本机约定（AGENTS.md）**：NAS 资源有限，**不要随意全量 build / 全量 test**。
优先 `cargo test --test <文件名>` 只跑改动的部分，验证完再全量跑一次。

#### ⚠️ 移动项目目录后必须强制重建

测试里大量使用 `env!("CARGO_MANIFEST_DIR")`，它是**编译期常量**。
`mv` 会**保留文件 mtime**，cargo 的指纹因此不变 → 复用旧二进制 →
测试拿着旧路径去找 fixture，报 `No such file or directory`。

```bash
touch tests/*.rs src/lib.rs   # 骗过 cargo 指纹，触发重建
cargo test
```

（2026-09 从 `rust-test/` 搬到 `data/` 时踩的坑，17 个用例假红。）

#### ⚠️ `scripts/api_test.mjs` 会拒绝跑陈旧二进制

它启动前比对 `target/debug/music-robot` 与 `src/**/*.rs` 的 mtime，**二进制比源码旧就拒绝运行**。
这是被坑出来的：曾拿一个比 `stream.rs` 旧 4 分钟的二进制跑脚本，转码断言全假失败。
改完 `.rs` 先 `cargo build`，再跑脚本。

---

## 2. 目录结构

```
music-robot/
├── Cargo.toml          # 依赖与引入理由都写在注释里（107 个锁包）
├── LICENSE-MIT / LICENSE-APACHE   # 双许可 MIT OR Apache-2.0
├── env.example         # 环境变量示例（cp 成 env.local 改完再 source）
│                       # ⚠️ env.sh（工具链 + mt 别名）是**本机专属**的，已从版本库移除并 gitignore
├── src/
│   ├── main.rs         # 唯一可执行入口，只做 stdout/stderr 桥接
│   ├── lib.rs          # 库根（13 个模块）
│   ├── tag/            # 标签读写引擎（read/ + write/），全自研，不碰任何服务端依赖（20 文件）
│   │   ├── read/       #   probe → id3v2 / id3v1 / apev2 / flac / wav + gbk_sniff + warnings
│   │   └── write/      #   intent 意图层 → mp3/flac/wav writer + atomic 原子替换
│   ├── cli/            # 7 个子命令 read/write/blank/scan/doctor/wash/serve + args/io（10 文件）
│   ├── fs.rs           # PathSandbox 路径沙箱（服务端隔离的底层）
│   ├── scanner.rs      # 递归枚举 + 四分级 + 流式 AudioWalker/WalkStats
│   ├── logger.rs       # CLI wash 事件流 + NDJSON sink（**不是**服务端日志）
│   ├── serverlog.rs    # 服务端日志：分级 + 按天写文件 + 保留清理（全自研）
│   ├── storage.rs      # StorageBackend trait + FileStorage（多根隔离，内部复用 PathSandbox）
│   ├── config.rs       # 配置模块（默认值逐项对齐画布；MR_* 环境变量；含 config 段）
│   ├── db/             # pool / migrations（v1–v3）/ models / repos/（8 个 repo）（13 文件）
│   ├── plugin/         # manifest / protocol / pool / sandbox / error / hash / registry（7 文件）
│   ├── watcher/        # suppress / classify / watch（生产主循环）/ inotify_poc（6 文件）
│   ├── service/        # library（扫描编排+去重）/ scrape（刮削编排+BatchRunner）（3 文件）
│   ├── audio/          # stream（Range 解析）/ cover（封面挑选）/ transcode（ffmpeg+缓存+清理）（4 文件）
│   └── server/         # state / error / auth / jobs / tests / routes/{...}（15 文件）
├── tests/              # 11 个文件，124 集成用例（含 plugin_e2e 真拉起 node/python3/sh）
├── plugins/            # musicbrainz.js（真实刮削插件，**已入库**）；用户自己的插件被 gitignore
│                       # 只扫这一层、不递归；按文件名升序尝试、命中即停
│   └── examples/       # example.{js,py,sh}（假数据，给集成测试 / api_test 当夹具，见 §6.3）
├── examples/           # 人工排查用的 `cargo run --example`：dump（逐文件打印全字段）
│                       # + native（MR_NO_FFPROBE=1 强制本地兜底通道）。**不是测试**、不参与 cargo test，
│                       # 内部直接用了 unwrap —— examples/ 不算生产路径（§7.5 只约束 src/）
├── .dsh/skills/         # **项目级 agent skill**（DSH 扫这一层，`<名>/SKILL.md`，不递归）
│                       # 24 个 Expo 官方 skill（expo-*/eas-*），来源与更新方式见 .dsh/README.md
├── scripts/api_test.mjs # 端到端 API 脚本（起临时服务逐条断言，140 项；非交互，给回归用）
├── scripts/api_cli.mjs  # 交互式 API 客户端（方向键菜单，连**已在跑**的服务；只用 node:readline，零依赖）
│                       # 曲库菜单里有：扫描入库 / 刮削 / 重刮失败项 / 重刮单曲
├── fixtures/           # 9 个样本：6 音乐 + 3 小 WAV，65M（**不入库**，重建见 §8.3）
├── music-server-architecture.excalidraw   # ★ 全部架构设计 + 28 步实施计划
└── HANDOFF.md          # ← 本文件（**仓库里唯一的文档**）
```

### 全部 HTTP 路由（**30 条路径 / 38 个操作**，与 `src/server/routes/mod.rs` 一一对应）

| 分组 | 路由 |
|---|---|
| 公开 | `GET /healthz` · `POST /api/auth/login` |
| 引导 | `POST /api/auth/register` —— **仅库空时可用**（初始化出首个 admin），之后一律 403 |
| 认证 | `GET /api/auth/me` · `POST /api/admin/users`（admin 建号，注册关闭后唯一入口）· `GET /api/admin/ping`（admin 占位） |
| 曲库 | `GET /api/library` · `GET /api/songs/{id}` · `GET /api/albums/{id}` · `GET /api/artists/{name}` · `GET /api/search` |
| 任务 | `POST/GET /api/scan` · `GET /api/scan/{batch_id}` · `POST/GET /api/scrape` · `GET /api/scrape/{batch_id}` · `GET /api/jobs` |
| ↳ 刮削队列选择 | `POST /api/scrape` 的 body **可选**：不给 = `pending` 队列；`{"mode":"failed"}` = 重刮失败项；`{"song_ids":[1,2]}` = 只听点名的（**含已 done 的**，这是唯一的「重新刮削」入口）。字段名写错一律 400，**绝不静默退化成刮全库** |
| 音频 | `GET /api/stream/{id}`（Range · `?format=mp3` 转码）· `GET /api/songs/{id}/cover` |
| 歌单 | `/api/playlists` 共 8 条 CRUD + 排序 |
| 播放周边 | `/api/history` · `/api/favorites` · `/api/settings` 共 7 条 |
| 点歌 | `POST/GET /api/requests` · `PATCH /api/requests/{id}` · `POST /api/requests/{id}/fetch｜link` |

### 核心设计：库层是纯函数

```rust
pub fn run(argv: &[String], io: &dyn CommandIO) -> i32   // 返回退出码，不 exit()
pub trait CommandIO { fn log(&self, m: &str); fn error(&self, m: &str); }
```

**所有命令都是「返回退出码的纯函数」**，不 spawn 子进程、不写死 stdout。
好处：① 测试直接调函数断言退出码（比 spawn 快得多）② 服务端能直接 import 复用。
`main.rs` 只做桥接 + `std::process::exit`。

退出码约定：`0` 成功 / `1` 运行期失败 / `2` 用法错误。
`wash`：`0` 全部 applied / `1` 有 failed。

---

## 3. 已完成 vs 未完成

### ✅ 标签引擎（自研，`src/tag/`）

| 模块 | 说明 |
|---|---|
| 读侧 | id3v2（含 USLT / APIC / TXXX / unsync / 三版本帧头）· id3v1（148 项流派表）· flac（Vorbis Comment + PICTURE）· wav（LIST INFO + 内嵌 id3 chunk）· apev2 · probe · ffprobe 双通道 |
| 写侧 | mp3（局部编辑，**保留未知帧**）· flac（键级局部编辑 + PICTURE 重建）· wav（INFO 子项级编辑 + RIFF size 重算）· `atomic_replace` 原子替换 + 音频 hash 校验 |
| 意图层 | `merge_fields` / `diff_fields` / `preview_view` / `sniff_image_mime` |
| 告警检测 | 广告词 / GBK 乱码 / ID3v1 垃圾尾 / 非白名单帧 |
| 事件流 | `WashEvent` + `console_sink` / `ndjson_sink` |
| 参数解析 | 零依赖手写（**没有引 clap**） |

### ✅ CLI（`src/cli/`，7 个子命令）

`read` · `write` · `blank` · `scan` · `doctor` · `wash` · **`serve`**（起 HTTP 服务）。
全部是「返回退出码的纯函数」，测试**在内存里直接调**，不 spawn 子进程。

### ✅ 插件与 watcher 子系统（**用例数是实测值**）

| 模块 | 行数 | 用例 | 说明 |
|---|---|---|---|
| `plugin/manifest.rs` | 733 | 32 | 插件头部清单解析（`@music-robot ... @end`）|
| `plugin/protocol.rs` | 1723 | 57 | 请求 / 响应契约，`action` 判别式（**只能**靠 action 判别，不靠字段有无）|
| `plugin/pool.rs` | 1448 | 19 | 同步 WorkerPool：懒启动 / 复用 / 空闲回收 / 超时 killpg |
| `plugin/sandbox.rs` | 822 | 19 | setsid + 6×setrlimit + NO_NEW_PRIVS + PDEATHSIG + 降权 |
| `plugin/registry.rs` | 490 | 6 | 扫 `plugins.dir` → 解析清单 → 按**文件名升序**组装插件表 |
| `plugin/error.rs` | 137 | 3 | 中文错误与来源链 |
| `watcher/`（3 文件） | 2075 | 37 | 自写抑制注册表 + 三闸门分类 + inotify 实证 PoC + 生产主循环 |
| **合计** | **~7400** | **173** | |

**插件运行方式（用户已定，不要再讨论替代方案）**：子进程运行时，**不做嵌入式 JS / WASM / .so**。
插件是**单文件脚本**（`plugins/*.js|.py|.sh`），命令由扩展名推断（`.js→node`、`.py→python3`、`.sh→sh`），
**零外部依赖**，无声明式 YAML。协议是 stdin/stdout 一行一个 JSON（`protocol: 1`），
响应必须原样回显 `action`，诊断信息一律走 stderr。

### ✅ 服务端（`src/server/`，S14–S23）

AppState / 统一 ApiError 形状 / build_router / 优雅关闭 · JWT（HS256，拒绝 alg:none）+
argon2id 口令哈希 + `require_auth` 中间件 + `AdminUser` 403 守卫 ·
后台长任务（单例锁，并发触发恰好 1 个成功、其余 409）· 30 条路径（见 §2）·
`scripts/api_test.mjs` 端到端覆盖。

### ❌ 未完成

| 项目 | 状态 |
|---|---|
| **S24–S26 前端** | 未开始 —— **动之前先问用户**（AGENTS.md：NAS 上不随意装依赖 / 跑全量 build）。**选型已定：Expo 壳 + Web 写 React DOM**，见 §6.6（画布 S24 写的 Vite+React 已过时） |
| ↳ **刮削页必须带「是否应用到文件」开关** | 🔴 **用户 2026-09-30 明确要求，做页面时别漏**。理由见 §6.3「刮削默认直写原文件」——现在 `POST /api/scrape` 是**全库无差别覆盖**，页面上必须让用户先选「只入库 / 也写文件」再动手 |
| S27 文件整理 · S28 集成打包 | 未开始 |
| **provider（下载）插件 kind** | ❌ 未实现 —— 见下面「两个已知缺口」 |
| `config.ts` 移植 | **用户明确说暂时不做**（配置改由 `src/config.rs` 承担） |
| TS 侧同步修 bug | **TS 要抛弃，不用管了**（用户已决定） |

### ⚠️ 两个已知缺口（别误当成 bug）

**① `POST /api/requests/{id}/fetch` 恒返回 503 —— 缺的是 provider 插件，不是刮削插件。**

画布对这条接口的原文是：「入参 `{song:{title,artist,album}, target_dir}` → 出参 `{ok, file_path}`；
系统接管：扫描该文件 → 入库 → 关联 song_id → done」，也就是**把音频文件本身下下来**，
属于 **provider** kind。而 `src/plugin/registry.rs` 目前**只加载 `kind = "scraper"`**。
这两条插件线互不相干，实现时别混。

503 的文案已改准（现在是「未配置下载（provider）插件，无法自动获取」）。原来写的是
「未配置刮削插件」，而刮削其实早就接通了 —— 含糊的文案会把排查方向直接带偏。
失败语义画布也定了：**保留 pending + 记录错误，可重试**。

**② MusicBrainz 插件的 `album` 几乎永远为空 —— 这是故意的，不是没实现。**

2026-09 实测两条路都堵死：recording **搜索**返回的 `releases` 是**截断的**（每个 recording
只回 1~2 个），所以「primary=Album 且 secondary 为空」的筛选几乎恒为空；补一次
`/recording/<mbid>?inc=releases+release-groups` lookup **反而更糟** —— 对 187s 的 Numb 只回
`Living Things +` @2013（再版），照单全收就是把**别人的再版**当成用户的原专辑写进库。
插件的选择是**留空**（协议里字段缺省 = 不修改）。真要做准得换思路：拿本地已有的 album 标签去
`/release` 搜索再核曲目表（Picard 那类 tagger 的做法，还要配 AcoustID 指纹），
与「简单插件」的定位冲突，所以刻意不做。

`year` 同样不可全信（Bohemian Rhapsody 实测给 1992，原版是 1975）—— 它取自
`first-release-date`，可能是某次再版的年份。

**③ MusicBrainz 的 score / 返回顺序**在中文曲库里**没有区分度**，别拿它当选主依据。**

实测（2026-10-01）：`recording:"老男孩"` 的同一条查询连打 4 次，正确歌手「筷子兄弟」
一会儿第 1、一会儿第 3，甚至掉出前 3；候选里 筷子兄弟 / 雷婷 / 羽·泉 / 赵照
**全是 score=100**。`recording:"Havana"` 更夸张：前 25 条 score 全 100，却一条都不是
Camila Cabello（Kenny G / Wimme / Frank Loesser…）。
**真正能区分的是本地的「歌手」与「时长」两个信号**，插件的 `rankKey` 就是按它们排序的。

---

## 4. 与 TS 的解耦状态

**已彻底解耦，TS 仓库可以随时删除，本仓库不受任何影响。**

- ✅ `tests/differential.rs` / `tests/diff_write.rs`（7 个会 shell 出去调 TS 的差分测试）已删除；
- ✅ `tsfixtures/` 已删除，3 个小 WAV 并入 `fixtures/`（WAV 是测试素材不是 TS 代码，改名而非删除）；
- ✅ `fixtures/` 是**真拷贝**，不依赖 TS 目录存在；
- ✅ 全部源码 / 测试已无 `tagwash-test` 路径引用；
- ✅ 记录 TS 移植过程的 `MIGRATION.md` 已并入本文 §11 并删除。

**唯一要注意的**：`fixtures/`（65M）**不在 git 里**，而它的重建脚本原本依赖 TS 仓库的 `samples/`。
TS 仓库 `/vol1/@appshare/dsh/data/tagwash-test` 目前**还在**，所以 §8.3 的配方仍可用；
但**删 TS 之前请先把 `fixtures/` 备份到别处**，否则这批真实脏数据样本就找不回来了。

---

## 5. 依赖与配置

### 5.1 依赖（`Cargo.toml` 的注释里有逐条引入理由，别绕过它）

| 依赖 | 用途 | 谁能碰 |
|---|---|---|
| `encoding_rs` | GB18030 码表（平台能力补位，非标签逻辑外包）| 标签层 |
| `sha2` | 裸音频完整性 hash（**不能用 `DefaultHasher`**，它不保证跨版本稳定）| 标签层 |
| `serde_json` | 手写 `Value`（**不用 serde derive**）：插件协议 / 服务端 JSON / CLI `--json` | 全层 |
| `dotenvy` | 读 `.env`（dotenv 风格）。**唯一理由是别让用户为了跑起来先学 source/export**；只在 `serve` 启动时读一次，且**不覆盖已存在的变量**（真环境变量优先）。零传递依赖 | 服务端 |
| `libc` | 沙箱的 `setsid` / `setrlimit` / `prctl` / `kill`。**绝不用它做标签逻辑** | 插件层 |
| `rusqlite`（bundled） | 服务端唯一存储依赖。bundled 是因为本机只有 `libsqlite3` 运行时、无开发库无 pkg-config | 服务端 |
| `tokio`（不用 full） | 异步运行时 | 服务端 |
| `axum` + `tower-http`(cors) | HTTP 层 | 服务端 |
| `hmac` + `base64` | JWT 签名 / base64url（平台能力补位）| 服务端 |
| `argon2` | 口令哈希（`argon2id`，OWASP 首推；**唯一一处刻意多花几个包**）| 服务端 |
| `tower`（dev） | 测试里对 Router 用 `ServiceExt::oneshot`，不绑端口 | 测试 |

⚠️ 标签读写层**全部自研**：不使用 lofty / id3 等任何第三方标签库。
⚠️ 后 6 项只有 `src/server/` 用，**标签层 / 插件层 / 扫描层一律不碰**：
插件是不可信**子进程**，不是 async 任务；给它们套 tokio 既没必要，也会让沙箱那套 fork/exec 逻辑难写。

⚠️ `hmac 0.13` 与 `sha2 0.11` 同属 digest 0.11 一代，**版本必须配套，不能单独升**。

### 5.2 环境变量（`MR_*`，见 `src/config.rs` 的 `apply_env`）

| 环境变量 | 字段 |
|---|---|
| `MR_JWT_SECRET` | `server.jwt_secret` |
| `MR_LIBRARY_ROOTS` | `storage.library_roots`（`:` 或 `,` 分隔）|
| `MR_DATA_DIR` | **数据根**：一次设好 `database.path` / `audio.cache_dir` / `log.dir` 三项（`<根>/music.db`、`/transcode`、`/logs`）。支持 `~` |
| `MR_DATABASE_PATH` | `database.path`（支持 `~`）⚠️ 优先于 `MR_DATA_DIR` |
| `MR_FFMPEG_PATH` | `audio.ffmpeg_path` |
| `MR_CACHE_DIR` | `audio.cache_dir`（支持 `~`）⚠️ 优先于 `MR_DATA_DIR` |
| `MR_LOG_LEVEL` | `log.level`（`error`/`warn`/`info`/`debug`/`trace`；写错会在启动前报错）|
| `MR_SANDBOX` | `plugins.sandbox` |
| `MR_PLUGIN_USER` | `plugins.plugin_user` ⚠️ **见 §6 未决疑点** |
| `MR_PLUGINS_DIR` | `plugins.dir`（支持 `~`）|

变量**未设置** = 不动；设置成**空白串** = `Empty` 错误（几乎必然是部署脚本漏填，早失败好过静默用半截配置）。
加载顺序：内置默认值 → 配置文件 → **`.env`（dotenvy，serve 启动时读）** → 环境变量 → 命令行（后者覆盖前者）。
⚠️ `.env` 是**兜底**：dotenvy 不覆盖已存在的变量，所以真环境变量总能压过它 —— 临时覆盖不用改文件。
模板见仓库根的 `env.example`（`cp env.example .env`）。

许可证：**`MIT OR Apache-2.0`**。

### 5.3 依赖编译耗时（实测，供排期参考）

| 批次 | 耗时 |
|---|---|
| `rusqlite`（bundled） | 1 分 49 秒 |
| `axum` + `tokio` + `tower-http` | 2 分 05 秒 |
| `hmac` + `base64` | 22 秒 |
| `argon2` | 21 秒 |

引入这些依赖**已获用户明确授权**，条件是**串行编译、不要与其它任务并行**（NAS 扛不住）。

---

## 6. 已知 bug 与遗留疑点

### 6.1 移植期抓到的 3 个真 bug（已修）

1. **ID3v1 genre 恒在 offset 127**，旧实现非 v1.1 时错读 126（padding 位）→
   `genre=0` 被当成 "Blues"，**blank 后 genres 残留，wash 对每个 MP3 都失败**。
   已修（`src/tag/read/id3v1.rs`）。TS 侧有同一个 bug，但 TS 要抛弃，不用管。
2. **FLAC 魔数成立但块序列损坏时静默降级**成「空但合法」。已改成 `Err(Unrecognized)`。
3. **扫描分级靠错误文案匹配**（`msg.contains("无法识别")`）→ 损坏 FLAC 被误判 rejected。
   已改成**按错误变体映射**。

### 6.2 已钉死的「忠实移植」边界（**不要顺手「修好」**）

这些是 TS 的既有行为，Rust 当初**刻意保持一致**。它们看起来像 bug，但改动属于**破坏性变更**，
会影响已有用户的脚本与预期：

| 现象 | 为什么不改 |
|---|---|
| 纯文本冒充 `.mp3` → scanner 判 `ok` | MP3 是 sync-marker 格式，无帧也合法。见 `s08_text_disguised_as_mp3_is_ok_like_ts` |
| `read --json <file>` 中 `--json` 会吃掉后面的文件 | `parseArgs` 的「下一参数不以 `--` 开头即取值」规则使然；正确写法 `read <file> --json`。见 `e14_boolean_flag_eats_following_positional` |
| wash 对「文本冒充 mp3」执行 blank 会改写文件 | scanner「可解析」与 wash「可处理」的语义缝隙。见 `e15_wash_on_frameless_mp3_is_a_known_limitation` |

⚠️ TS 已决定抛弃（§4），所以**不要**再去改 TS —— 要动这些行为，先跑对应测试、
确认影响面，并**先问用户**。别只改代码不看用例注释。

### 6.3 2026-09 收尾轮：修掉的空配置项 + 踩到的坑

- **`log.dir` / `log.level` 以前也是死的**（能读能校验、全代码零引用，服务端**一条请求日志都没有**）。
  现已接线：见 §7.7「服务端日志」。
- **`cache_expiry_days` 以前是个骗人的配置项** —— `src/config.rs` 里能读能校验，
  但**没有任何代码执行它**。现在 `audio::transcode::prune_cache` 在 **`serve` 启动时**
  清一次过期缓存（`src/cli/serve.rs`）。删除判据**故意极严**：只删文件名严格匹配
  `<十进制id>-<安全hash>.mp3` 的**普通文件** —— `cache_dir` 是用户配的，完全可能指向
  一个已经有别的东西的目录，无条件清目录就是删用户的文件。`expiry_days == 0` 是
  **关闭清理**而不是「删光」。常驻期不重复扫（反复读目录只白耗 IO）。
  4 个 prune 用例都做过变异验证（放宽判据 / 去掉 `is_file` 检查 / 让 0 天不再早退，
  对应断言全部变红）。
- **MusicBrainz 插件必须自带网络重试**：本机 `node fetch` 会**间歇性**抛 `fetch failed`
  （同一 URL、同一进程，下一秒就好；同一时刻 `curl` 是 200）。不重试时整首歌的刮削白跑。
  现在 `timeout_ms` 被当作**整次请求（含重试）的总预算**切分，最多 3 次、退避 400/800ms；
  实测全失败路径 **2350ms** 返回，远低于清单里的 15s（超了池子会先杀掉插件，用户看到的
  就变成「插件超时」，排查方向直接被带偏）。顺带把 node 的 `e.cause` 拼进错误文案 ——
  光一句 `fetch failed` 没有任何信息量。
- **示例插件会遮蔽真实插件**：插件按**文件名升序**尝试、命中即停，而 `example.js`
  永远返回 confidence 0.95 —— 它和真插件同目录时，`musicbrainz.js` **永远不会被执行**。
  因此仓库里的示例已退到 **`plugins/examples/`**（registry 只扫一层、不递归，等于自动失效）。
  要用示例跑 e2e 的测试自己去 `plugins/examples/` 取（`tests/plugin_e2e.rs`、`api_test.mjs`）。
  ⚠️ 别把示例挪回 `plugins/` 顶层，那会再次把真插件遮蔽掉。
- **刮削默认直写原文件，且没有任何回退手段**（2026-09-30 查证）。`commit_hit` 拿 `song.file_path`
  直接 `write_tags`，`atomic_replace` 只是「copy → 写 tmp → 校验 → rename 覆盖」——
  中途失败原文件完好，**但 rename 一成功原文件就被换掉了**。而且是**无条件覆盖**不是填空缺
  （`apply_tags_to_song` 直接赋值）。
  * 没有 `.bak`、没有 dry-run、没有二次确认；`--preview` / `--bak` **只有 CLI 的 `write` / `blank` 有**
  * `POST /api/scrape` 的 body 里能指定队列（2026-09-30 补的，见 §2），但**默认不给 body 仍是全库 pending**；
    而且**没有「只入库不写文件」的档位** —— 库里没有这个开关，前端做页面时必须自己拦一道（见「未完成」表）
  * DB 里的旧值也**没留档**（没有标签历史表，`play_history` 是播放记录不是改动记录）
  * 所以**逐曲日志（§7.7）是唯一的痕迹**，别删。前端做刮削页时必须给开关（见「未完成」表）
  * 实测对照（同一个文件）：原件 `歌手: 公众号：阿乐资源库 / 专辑: 2015江苏卫视新年演唱会`
    → 被 example.js 刮完 `歌手: 示例歌手 / 专辑: 示例专辑`。**插件写坏了标签，原值就找不回来了**
- **`plugins/musicbrainz.js` 曾被 `.gitignore` 挡住**（规则是 `/plugins/*` 只放行 example.*）。
  它是**随仓库发布的真实插件**，已加放行。别再把 `/plugins/*` 理解成「仓库里不放插件」。
- **MusicBrainz 插件不能「只查歌名」，必须 AND 优先 + 歌名兜底**（2026-10-01 改，别退回去）。
  起因：本机 6 首 fixture **全军覆没**，一查才发现**标签本身是脏的** ——
  盗版资源的上传者把广告塞进了 artist 字段（`公众号：阿乐资源库`、
  `凤凰传奇 | 音乐下载网站 yym4.com`），拿这种值 AND 必然零结果，而其中 3 首
  只按歌名查立刻 score=100 命中。**一个字段脏就判死整首歌** —— 这个策略太脆。
  现在插件的行为：
  * 身份**优先从文件名解析**（`歌名-歌手.ext`，取**第一个**分隔符），解析不出来才退回标签。
    标签脏、文件名反而干净，本机 fixture 就是活例子。
  * 查询**三段**（去重后最多 3 次请求）：`歌名 AND 歌手` → `歌名 AND 规范化歌手` → `歌名`。
    规范化 = 去掉最后一个 `-` 之后的尾巴 + `&` 两边加空格 + 驼峰拆分
    （`Camila Cabello&YoungThug-大耳兽莫慢待` → `Camila Cabello & Young Thug`，这条恰好
    命中本地那个 217s 的专辑版）。
  * ⚠️ **认领闸门**：候选必须满足「歌手吻合 **或** 时长吻合」才认，否则回 NOT_FOUND。
    **少了这道闸会写错数据**：实测 `recording:"Havana"` 的候选里
    `Brother Sun Sister Moon`《Havana》(1997) 时长恰好 215s（本地 217s），
    confidence 算出 0.9 > 服务端 0.80 阈值 —— 靠加减分**拦不住**（那条候选没有时长字段的那次
    是 `David Rudder`，也是 0.9）。闸门必须显式判。
  * 结果（6 首 fixture）：**4 首正确命中**（Havana / 牵丝戏 / 盛夏 / 老男孩，**歌手全对**），
    **2 首 MusicBrainz 里确实没有**（华夏传说 / 最美情侣），**0 假阳性**。
  * 纯函数有离线自检：`node plugins/musicbrainz.js --selftest`，
    已挂进 `cargo test`（`tests/plugin_e2e.rs::musicbrainz_plugin_passes_its_offline_selftest`）。
    **改这三个函数一定要先跑它** —— 它们决定「哪条候选会被写进用户文件」。

### 6.4 未决疑点（**用户说后面再议，别自己拍板**）

1. **ID3v2 unsync 与规范不符** —— 原 TS 实现（`id3v2.ts:33-37`）在 `FF+E0..FF` 时会多跳一字节。
   Rust 侧**刻意照搬同一行为**并注释标记。修它属破坏性变更，需要新的独立理由 + 用户点头。
2. **APEv2 的 size 字段语义，参照实现与规范不一致** ——
   规范说 footer 的 tagSize = items 区 + footer(32B)；而 TS 实现（`apev2.ts:29`
   `buf.slice(pos - info.size, pos)`）把它当 **items-only** 用。实测同一份合成字节：
   写 `items+32` 时 TS 返回 null，写 `items` 才解析成功。
   👉 **这意味着真实世界的 APEv2 文件（尤其 Picard 等规范写入器产出的）可能被判为「无 APE tag」——
   **静默漏读、不报错**。Rust 侧目前与参照保持一致（移植保真优先）。
   **建议**：拿一个真实带 APE 的 MP3 验证；若确认是 bug，改 Rust 侧（TS 已抛弃）。
3. **`plugins.sandbox` 的模式字符串与 `plugins.plugin_user`（uid/gid）尚未真正接进沙箱行为** ——
   配置能读能校验，`MR_SANDBOX` / `MR_PLUGIN_USER` 也在，但沙箱目前用的是固定默认值。
   属于「配置项存在但未生效」，与 `cache_expiry_days` 同类，**下次收尾记得处理**。
4. **`PathSandbox` 有 TOCTOU** —— `resolve` 与真正 read/write 之间非原子，期间 symlink 被换掉
   仍可逃逸。彻底堵住需要 `openat2(AT_SYMLINK_NOFOLLOW)` 或 fd 级传递，属内核接口选择，
   本模块不做。缓解：曲库目录设为非用户可写。**服务端上线前需重新评估。**
5. **`music-robot read <file> | head -8` 会 panic**（`Broken pipe (os error 32)`）。
   根因是往 stdout 写时没处理 EPIPE，属 CLI 侧，不影响服务端。**未修。**

### 6.5 路径沙箱为什么长这样（历史，**别退回教科书写法**）

`src/fs.rs` + `tests/sandbox.rs`（10 个对抗用例）。服务端会拿**用户输入 / 数据库里的**路径读写文件，
这是越权防线。对抗测试抓出过两处真实逃逸：

1. **canonicalize 的结果没用上** —— 第一版把**原始路径**拿去 `starts_with`，
   于是 `root/../outside/secret.txt` 的组件序列确实以 `root` 开头 → 检查直接放行。
2. **`Path::starts_with` 不折叠 `..`** —— 纯组件前缀比较，`base/root/../outside` 仍以 `root` 开头。
   而修 ① 时改成的「逐层 `file_name()`/`parent()` 向上剥」还有第三处坑：
   `file_name()` 对以 `..` 结尾的路径返回 `None`，循环提前终止、留下未解析的 `..`。

最终形态是两步：**先按逻辑语义折叠 `.`/`..`**（得到纯下降组件栈，折叠后才可能做归属判断），
**再从顶端找最深存在的真实前缀并 canonicalize**（解析 symlink）。缺任一步都会被测试抓出来。

变异验证（证明断言真的会咬人）：

| 注入 | 结果 |
|---|---|
| 不做 canonicalize，直接对原始路径 `starts_with`（**教科书写法**）| ❌ 5 条红（s02/s03/s05/s08/s10）|
| 跳过逻辑折叠，保留 canonicalize | ❌ s02 红 |

⚠️ 第一条值得单独记：**这是大多数人写路径沙箱的第一反应，它是错的。**

**它不是权限模型**：只回答「路径在不在库根下」，不回答「这个用户有没有权限」。
多租户隔离要靠上层 token/tenant 映射到不同 root，别在沙箱层做。

### 6.6 前端技术选型：Expo Web 能不能做 PC 页面（2026-10-01 调研）

**背景**：画布 S24 原本写的是 `Vite + React + 路由 + axios 拦截器`。用户 2026-10-01 提出
改用 **Expo Web**，目的是**和将来的 iOS/Android 客户端共用一份代码逻辑**。以下是调研结论。

**结论：Expo 这个壳可以用，但 `react-native-web`（RNW）那一层组件不该用来写 PC 主界面。**
推荐 **Expo 单仓库 + Web 端直接写 React DOM**。

> ✅ **2026-10-01 用户已拍板：走这条（方案 B）。** 24 个 Expo skill 全留在 `.dsh/skills/`（用户选择不精简）。
> ⚠️ 画布 S24 卡片仍写着 `Vite + React + 路由 + axios 拦截器`，**已过时但尚未改**（改画布要用户点头）。
> 动手时按本节的形态与 5 条硬规则来，别照画布那句选型。

#### 证据（都是官方原文或源码，不是二手经验）

- 📗[Expo 官方](https://docs.expo.dev/workflow/web/)逐字：
  > 「RNW is **optional** when developing for web since you can use React DOM directly」
  > 「Building web-only components is **fully supported** by Expo」
  即 「Web 端直接写 `<div>`」是官方支持的一等用法，不是 hack。
- 📗 官方自己在 [平台分叉文档](https://docs.expo.dev/router/advanced/platform-specific-modules/)
  的示例里就写 `Platform.OS === 'web' ? <div>…<Slot/></div> : <Tabs>` —— **官方示范的就是「外壳分叉」**。
- 📗 `expo-router` 官方 skill 里也有 `_layout.web.tsx` 的独立 Web 布局做法（本地已装，见 `.dsh/skills/expo-router/references/tabs.md`）。
- 🔬 RNW 的 `forwardedProps` 白名单里**没有** `onDrop/onDragStart/onDragOver/onDrop…`，
  而不在白名单的事件（含 `onCopy/onPaste/onDoubleClick`）会被 **`pick()` 静默丢弃、不报错**。
  → HTML5 拖放、复制粘贴、双击 在 `<View>` 上不可用，必须降到 DOM 层。
- 🔬 RNW 的 role→语义标签映射表里**没有 `table/tr/td`**：`<View role="table">` 只得到一个
  `<div role="table">`，拿不到真表格布局算法。做「曲目列表」这是决定性的。
- 📗 RNW 官方明确：**不支持 `@`-rules / 伪类 / 伪元素**（所以 `:hover`、`:focus-visible`
  只能靠 JS 事件 + 重渲染）；**列表组件「not optimized for the web」**；
  `Animated` 在 Web 上**没有 `useNativeDriver`**。
- 🔬 `cursor` / `userSelect` 是 **RNW 专有 style**：放进共享 StyleSheet 后，
  **原生端 `StyleSheet.validate` 会直接抛错**。共享样式文件本身就是雷区。
- 📊 官方 24 个 skill 正文里：`native` 1199 次 vs `web` 281 次，`desktop` **只出现 1 次**
  （还是在 `expo-app-clip` 里）。**Expo 官方指导里根本没有「PC 桌面端」这个场景。**

#### 落地形态（等用户拍板后再动手）

```
apps/web/                  # Expo 项目（唯一前端）
  app/_layout.web.tsx      # PC 外壳：<div> + CSS grid 侧边栏
  app/_layout.native.tsx   # 手机外壳：<Tabs>（将来）
  app/(library)/index.web.tsx   # PC：真 <table> + 虚拟滚动
packages/core/             # 100% 共用：API client / 类型 / 播放队列状态机 / hooks
```

**S24 的交付物（按本方案改写，替代画布里那句「Vite + React + axios 拦截器」）**：

技术栈：**Expo(Metro) + Expo Router + TypeScript + Tailwind v4 + shadcn/ui**。
（2026-10-01 用户拍板：**不用 antd**，用 shadcn/ui。）

```sh
npx create-expo-app@latest          # TS + Expo Router
npx expo install tailwindcss @tailwindcss/postcss postcss --dev   # 官方 Tailwind 指南
```
`postcss.config.mjs` → `{'@tailwindcss/postcss': {}}`；`global.css` → `@import 'tailwindcss';`；
**在 `app/_layout.tsx`（根布局，不是嵌套布局）里 import `global.css`** —— 导入位置错了会让
`node_modules` 的 CSS 排在你的样式前面，样式顺序就崩了（官方明确警告）。
别在 `metro.config.js` 里关掉 CSS（`isCSSEnabled` 必须为 true）。

四条**已核实的兼容性前提**（都是官方文档，不是推测）：
1. 📗 Expo 官方 Tailwind 指南：「You can use Tailwind with **React DOM elements as-is**」
   —— 和本方案「Web 写 `<div>`」严丝合缝。
2. 📗 同一页也写了：**标准 Tailwind 只支持 Web**。将来做移动端要么上 NativeWind/Uniwind，
   要么原生 UI 另写样式 —— 与我们「UI 层各写各的」的结论一致，不是新增代价。
3. 📗 `@/*` 路径别名 **Expo 默认就开**（`experiments.tsconfigPaths`，Metro 含 web 都支持），
   正是 shadcn 的约定（`@/components`、`@/lib/utils`）；改了 `tsconfig.json` 要重启 Expo CLI。
4. shadcn 组件 = Radix + Tailwind + `cn()`，全是普通 React DOM，`className` 直接可用。

⚠️ **动手时要现场确认的一件事**：`npx shadcn@latest init` **未必认得 Expo 项目**（它默认探测
Next/Vite 等）。认不出就走**手动安装**路径（自己写 `components.json`、拷组件源码、装
`class-variance-authority` / `clsx` / `tailwind-merge` / 对应 Radix 包）。这一步没实测过，
别当已知 — 真跑的时候以实际输出为准。

⚠️ shadcn 的 registry **可以来自第三方**，社区讨论过注入风险。**只用官方 registry。**

**设计决策谁来做**（用户自述「设计 UI 头疼」，而 shadcn 恰恰不替你做设计决策）：
`shadcn init` 时**一次性定死** base color + radius，之后不再改；动手写页面前，先用本机的
`html-prototype` / `show-me` / `design-artifact` skill 出 2~3 个 HTML 变体让用户挑，
**挑完再落代码**。布局结构可直接参考 `satnaing/shadcn-admin`（Shadcn + Vite 的后台模板，
只抄布局，不抄技术栈）。

测试仍是 Vitest（Web 逻辑层用 Vitest 比 jest-expo 省事）。

硬规则（不管最后选哪个方案都成立）：
1. **PC 页面不用 `<View>/<Text>` 拼布局**，直接 `<div>` + CSS；`className` 在 RNW 组件上
   会被静默丢弃，要桥接 CSS 类得用 `style={{ $$css: true, _: 'my-class' }}`。
2. **共享样式文件里禁止出现 `cursor` / `userSelect`** —— 会让原生端崩，一律放 `.web.ts`。
3. 全局快捷键（空格播放等）用 `document.addEventListener`，写进 `.web.ts`。
4. `web.output` 用 `single`（SPA）：路由是 `/:artist/:album/:track`，穷举不了，静态渲染没意义。
5. 真要上架 App 时先做 PWA 验证，再评估 Capacitor（把 Web 包壳）而不是反过来把 Web 塞进 RN。

⚠️ **没找到可靠来源**的一条：没有任何公开复盘是「某团队用 Expo Web 做了桌面优先的复杂 Web 应用」。
官方文档全程假设「响应式 Web + 移动 App」，**没有针对桌面宽屏的专门指南**。这本身也是信号。

---

## 7. 设计约定（必须遵守）

### 7.1 测试哲学：没有变异验证过的「全绿」不算证据 ⭐

**这是本项目最重要的约定，用户明确背书。**

写任何断言之前，先回答：**「改坏什么会让这个用例变红？」**
然后真的去改坏一次，确认它红了，再改回来。

历史上抓到过 5 类「空壳断言」，都会漏测：

1. **静默跳过** —— 参照实现不可用时直接 `return`，cargo 记作 `1 passed`，等于没测；
2. **自指比较** —— `audio_hash(orig) == audio_hash(new)` 两边用同一段代码，删掉 verify 也不会红；
3. **变异没真的应用** —— 改了源码但没生效（python 脚本 assert 失败），却读到「4 passed」；
4. **前置条件没满足** —— 断言文件长度变了，但写入其实静默丢弃了字段（WAV 没有歌词槽位），
   长度恰好没变，断言碰巧通过；
5. **教科书实现就是错的** —— 见 §6.5。

**禁止为了让测试变绿而放宽预期**。要改断言，必须先证明断言错了，而不是代码错了。

### 7.2 提交规范

提交信息**写中文**，一句话概括改动 + 用例数 + 抓到的 bug。例：

```
CLI 外壳：read/write/blank/scan/doctor/wash + 事件流，120 用例；修 ID3v1 genre 错位/FLAC 静默降级/分级文案匹配三个真 bug
```

历史提交用的身份是 `migration <none@local>`（`git -c user.name=... -c user.email=... commit`）。
新工作沿用同一身份即可，保持 `git log` 一致。

### 7.3 依赖纪律

- **标签读写自己实现**，不用 lofty / id3；
- 平台能力库 OK：`encoding_rs` / `sha2` / `serde_json`（以及服务端的 `hmac`/`base64`）；
- 不引 clap（参数解析已手写，见 `src/cli/args.rs`）；
- **引新依赖前先在 `Cargo.toml` 注释里写清理由**，并**先问用户**（要串行编译）。

### 7.4 输出与事件

- **不写死 stdout**，一律走 `CommandIO` trait；
- 事件流走 `logger::EventSink`，不写死 sink 类型；
- CLI 用 `ConsoleIO`，测试用 `CollectingIO`，服务端可以注入自己的。

### 7.5 错误类型分离

- **IO 错误 ≠ 路径越权**。`FsError::Escape` 和 `FsError::Io` 必须分开、可观测 ——
  服务端要能区分「用户传了坏路径」和「磁盘出问题了」。
- **生产路径零 `unwrap` / `expect` / `panic!`**；测试里随便用。
  ⚠️ **2026-09 复核口径：这条指「生产路径无可达 panic」，不是字面零命中。**
  按下方的命令剔掉 `#[cfg(test)]` 后，`src/` 里仍有 **15 处**常编命中，逐条确认**全部不可失败**：
  - **11 处是构造上不可失败的惯用法** —— `tag/read/flac.rs`×3、`tag/read/probe.rs`×3 是
    `slice[..4].try_into().unwrap()`（上一行刚做过长度守卫）；`tag/read/dispatch.rs`×2、
    `tag/write/flac_writer.rs`×1、`tag/write/intent.rs`×1 是 `is_some()` 守卫之后的解包；
    `logger.rs`×1 是对 `json!({...})` 字面量取 `as_object_mut()`。
  - **4 处是 `cli/io.rs` 里 `CollectingIO` 的 `Mutex::lock().unwrap()`** —— **唯一真正可失败**的一类
    （Mutex 中毒即 panic），且该类型**没有 `#[cfg(test)]` 门控**，是 `pub` 且常编的「测试用」辅助类型。
    与 §1 警告的「std Mutex 不可重入」同源，动它前先想清楚影响面。
  - 另有两个文件计数很高但**不算**：`src/server/tests.rs`（39 处）和 `src/watcher/inotify_poc.rs`（6 处）——
    它们的门控写在**父模块**里（`src/server/mod.rs` 的 `#[cfg(test)] mod tests;`、
    `src/watcher/mod.rs` 的 `#[cfg(test)] mod inotify_poc;`）。
    **只在文件内部搜 `#[cfg(test)]` 会把这两个文件误判成生产代码。**

  ```bash
  # 复核：剔掉每个文件从 #[cfg(test)] 起至文件尾的测试代码，再统计
  python3 -c "
  import re,pathlib
  pat=re.compile(r'\.unwrap\(\)|\.expect\(|panic!\(|unreachable!\(')
  for p in sorted(pathlib.Path('src').rglob('*.rs')):
      s=p.read_text(encoding='utf-8',errors='replace'); i=s.find('#[cfg(test)]')
      n=len(pat.findall(s if i<0 else s[:i]))
      if n: print(n,p)
  "
  # 期望 60 命中 = 真实 15 + server/tests.rs 39 + watcher/inotify_poc.rs 6（后两者即上面的误报）
  ```
  ⚠️ `examples/`（`cargo run --example`）不算生产路径，它内部直接用了 `unwrap`，不要照 §7.5 去改它。
- 面向用户 / 客户端的错误文案**一律中文**；内部细节（服务器路径、SQL、stderr）只进日志。

### 7.7 服务端日志（`src/serverlog.rs`）

**别和 `src/logger.rs` 搞混**：那个是 CLI `wash` 的**事件流**（`WashEvent` + NDJSON sink，给 `--events` 用）。

* 行格式：`<UTC ISO8601> <LEVEL> [<target>] <正文>`，例：
  `2026-09-30T13:29:40.700Z INFO  [http] POST /api/auth/register -> 201 (2835ms)`
* `target` 是短标签，方便 grep：`server` / `db` / `auth` / `http` / `job` / `plugin` / `scrape` / `cache` / `cover` / `stream`
* **同时写文件 + 回显 stderr**。文件：`<log.dir>/music-robot-<UTC 日期>.log`，一天一个
* `log.level` 是**下限**：`error` < `warn` < `info` < `debug` < `trace`。写错会在启动前报错
* `log.keep_days`（默认 7）天：启动时清更早的日志；**`0` = 不清理**。
  只删 `music-robot-<日期>.log` 形状的**普通文件**（`log.dir` 是用户配的，可能指向已有别的东西的目录）
* **请求日志按结果分档**：5xx → `error`，4xx → `warn`，其余 → `info`；`/healthz` 探活频繁 → 降到 `debug`
* ⚠️ **绝不记请求体与 Authorization 头** —— 注册/登录的 body 里有明文口令，令牌进日志文件等于长期泄漏
* ⚠️ **登录失败不记用户名**（与「响应不泄漏存在性」同一口径：日志会被收集、会被更多人看到）
* ⚠️ 日志**写不出去绝不 panic**：目录建不出来只在 stderr 提示一次，服务照跑
* 请求日志中间件挂在 [`server::run`] 里而**不是** `build_router` —— 测试全走后者，
  挂那里会把每条用例的请求都刷出来
* **刮削逐曲一条**（`target = scrape`）—— 命中记 `info`，未命中 / 忙跳过 / 出错记 `warn`，
  写回文件失败记 `error`。命中那条**带改动明细**，例：
  ```
  INFO  [scrape] 曲目 3 命中：插件 example-js（confidence 0.95）改动 4 处：歌手「公众号：阿乐资源库」
        →「示例歌手」；年份「2017」→「2024」；专辑「《最美情侣》」→「示例专辑」；歌词「已更新 14 字」；/music/xxx.mp3
  WARN  [scrape] 曲目 1 未完成刮削：musicbrainz：插件报错（NOT_FOUND）：MusicBrainz 没有找到匹配的录音
  ```
  ⚠️ 这不是锦上添花：刮削**直接覆盖原文件**（见 §6.5），DB 旧值也被 UPDATE 掉，
  **这行日志是事后唯一能还原「插件改了什么」的地方**。改动明细由 `TagSnapshot` 比对
  （改前快照必须在 `apply_tags_to_song` **之前**取）+ 歌词的 `FieldUpdate` 分支拼成；
  专辑比**名字**不比 id（id 变了名字没变 = 用户视角没改）。
  只列真的变了的字段，一个都没变就写「无字段变化」。
* **插件收发的原始报文**（`target = plugin`，2026-10-01 加）—— 插件链路上唯一的原始证据：
  * `INFO  启动插件进程 musicbrainz：node /path/plugins/musicbrainz.js（工作目录 /tmp/…）`
    —— 起进程只发生一次（之后复用），放 info 就是为了随时能回答「到底用什么命令调的插件」。
  * `DEBUG 调用插件 musicbrainz：{完整请求 JSON}`
  * `DEBUG 插件 musicbrainz 返回：{完整响应 JSON}`
    请求体含 `song{title,artist,album,duration_ms,file_path,…}` + `want` + `work_dir`；
    响应体就是插件回的协议 JSON。**排查「插件为什么没命中」看这一对就够。**
  * 收发放 **debug**：一次刮削按「曲目 × 插件数」产生成对的行，10k 首的库放 info 会把日志淹掉。
    看它们要 `MR_LOG_LEVEL=debug`（或配置 `log.level = "debug"`）；info 下只有「启动插件进程」那行。
  * 单行超 **2000 字符**截断并注明原长（`cap_line`，按**字符**不按字节 —— 中文不能被切出乱码）。
    截断后不再是合法 JSON，日志用，没关系。
* ⚠️ **插件自己的 stderr 是 `Stdio::null()`，被直接丢掉**（`pool.rs` 的 `spawn_worker`）。
  这是刻意的：插件是不可信子进程，不能让它往服务端 stderr 里灌东西。所以插件内部的
  诊断（如 `[musicbrainz] strict 查询零结果…`）**不会进日志**；要调插件就靠上面那对收发报文。
  真需要时再改，但那时要连同「每行加前缀、限制单行长度」一起做，否则是不可信输入直灌日志。

### 7.6 服务端读写的唯一入口

**handler 里绝不要直接 `std::fs::read`**。正确姿势是走 `StorageBackend`（`FileStorage`），
它内部对每个库根复用 `PathSandbox`，会 fail-closed 地 canonicalize。
封面 / 流式播放都是这个路子：先 `storage.stat()` 拿到**已验证的规范路径**，
再拿它去 `std::fs` 读 —— 这时的路径不可能再逃逸。

同步阻塞调用分两种（`std::fs` + `rusqlite` 全是同步的）：

- **短**阻塞（一次查库 / 探活，毫秒级）→ `run_db`（内部 `spawn_blocking`）包起来再 await；
- **长**任务（扫描 / 刮削，几秒到几分钟）→ 交给 `server::jobs` 注册表，由它 spawn 独立 OS 线程。
  **不要**用 `spawn_blocking` 跑长任务，会把 blocking 线程池占死。

---

## 8. 测试约定

### 8.1 怎么跑

```bash
source /vol1/@appshare/dsh/tools/rust/setup.sh    # 每个新 shell 都要

timeout 300 cargo check --lib                       # 先看有没有警告（要求 0）
timeout 600 cargo test --lib                        # 库用例（~58 秒）
timeout 600 cargo test --test cli_scan              # 只跑一个文件（13 秒）
timeout 300 cargo test --lib audio::transcode::     # 只跑一个模块
timeout 600 cargo test --lib read id3v1_genre       # 只跑一个用例
timeout 600 cargo test --tests                      # 库 + 全部集成

timeout 600 cargo build && timeout 900 node scripts/api_test.mjs   # 端到端
```

⚠️ **`api_test.mjs` 要求二进制比源码新**，改完 `.rs` 必须先 `cargo build`（见 §1）。

### 8.2 测试文件对照（**实测数字**）

库用例（`cargo test --lib`）共 **499 passed / 0 failed / 1 ignored**。集成测试：

| 文件 | 用例 | 覆盖 |
|---|---|---|
| `tests/read.rs` | 16 | 读侧 + ID3v1 genre 回归 |
| `tests/cli_args.rs` | 14 | 参数解析 |
| `tests/cli_read.rs` | 21 | read / write / blank 命令 |
| `tests/cli_scan.rs` | 23 | scan / doctor / wash |
| `tests/mp3write.rs` | 12 | MP3 写侧 |
| `tests/intent.rs` | 7 | 意图层 |
| `tests/sandbox.rs` | 10 | 路径沙箱（**10 个对抗用例**）|
| `tests/flacwav.rs` | 9 | FLAC / WAV 写侧 |
| `tests/apev2.rs` | 5 | APEv2 读 |
| `tests/wav.rs` | 4 | WAV 读 |
| `tests/plugin_e2e.rs` | 3 | **真拉起 `node` / `python3` / `sh`** 跑插件协议 |
| **合计** | **124** | |

另有 `scripts/api_test.mjs`：起临时服务、逐条打 HTTP，**140 通过 / 0 失败**，
分 9 组（鉴权 / 扫描 / 曲库 / Range / 封面 / 转码 / 歌单 / 播放周边 / 点歌）。
它自带两个防呆：**拒绝陈旧二进制**（§1）、**转码缓存目录已隔离**（不会写脏 `~/.local/share/music-robot/transcode`）。

### 8.3 测试数据（`fixtures/`）

`fixtures/`（65M，9 个文件）**不入库**（`.gitignore` 忽略）。内容是：

| 文件 | 用途 |
|---|---|
| 6 × mp3/flac | 真实脏数据样本（广告词 / GBK 乱码 / ID3v1 垃圾尾），scan / read 告警检测素材 |
| `tagged.wav` / `plain.wav` / `oddpad.wav` | WAV 读写用例（有标签 / 无标签 / **奇数 padding**）|

**重建方式**（仅当文件丢失时；依赖 TS 仓库的 `samples/`，所以删 TS 前务必先备份 `fixtures/`）：

```bash
cd /vol1/@appshare/dsh/data/music-robot
rm -rf fixtures && mkdir fixtures
cp -a /vol1/@appshare/dsh/data/tagwash-test/samples/*            fixtures/   # 6 个音乐样本，64M
cp -a /vol1/@appshare/dsh/data/tagwash-test/tests/fixtures/*.wav fixtures/   # 3 个小 WAV，712K
chmod 755 fixtures && chmod 644 fixtures/*
```

WAV 样本的原始配方（**可复现，勿手工摆字节**）：

```bash
cd /vol1/@appshare/dsh/data/tagwash-test && mkdir -p tests/fixtures
# 无标签：验证「空」与「解析失败」可区分（占位实现曾对此假成功）
ffmpeg -y -i "samples/华夏传说 - 凤凰传奇.mp3" -t 2 -map_metadata -1 -c:a pcm_s16le -ar 44100 -ac 2 tests/fixtures/plain.wav
# 全字段 LIST INFO：INAM/IART/IPRD/ICRD/IGNR/ICMT 映射
ffmpeg -y -i "samples/华夏传说 - 凤凰传奇.mp3" -t 2 -c:a pcm_s16le -ar 44100 -ac 2 \
  -metadata title="测试标题" -metadata artist="测试歌手" -metadata album="测试专辑" \
  -metadata date="2019" -metadata genre="Rock" -metadata comment="测试备注" tests/fixtures/tagged.wav
# 奇数长度 INFO 子项（单字符标题 + 8kHz 单声道）：专门踩 RIFF 偶对齐 pad
ffmpeg -y -i "samples/华夏传说 - 凤凰传奇.mp3" -t 1 -c:a pcm_s16le -ar 8000 -ac 1 \
  -metadata title="奇" -metadata artist="短名A" tests/fixtures/oddpad.wav
```

Ground truth（ffprobe）：plain/tagged = 2.000s / 44100Hz / 16bit / 2ch；oddpad = 1.000s / 8000Hz / 16bit / 1ch。

⚠️ **不要在 `fixtures/` 上直接跑 `wash --apply`** —— 那是**对照基线**，改坏了得重拷。
测试代码都自己 `cp` 到 `target/` 下再折腾。

---

## 9. 手动试用（最短路径）

### 9.1 CLI

```bash
source env.sh
B=./target/debug/music-robot

rm -rf /tmp/try && mkdir /tmp/try && cp fixtures/* /tmp/try/

$B -h
$B read /tmp/try/盛夏-毛不易.mp3
$B read "/tmp/try/牵丝戏 - 白兀.flac" --json          # 注意 --json 放最后！见 §6.2
$B scan /tmp/try                 # 四级分级 + 告警码
$B doctor /tmp/try               # 环境体检
$B write /tmp/try/华夏传说*.mp3 --title 测试 --preview   # 不落盘
$B wash /tmp/try --blank                          # 默认 preview
$B wash /tmp/try --blank --bak --apply           # 真写
```

**验证安全的两个点**：

```bash
md5sum /tmp/try/*.mp3 > /tmp/h1
$B wash /tmp/try --blank          # preview
md5sum /tmp/try/*.mp3 > /tmp/h2 && diff /tmp/h1 /tmp/h2 && echo "✓ preview 零写入"

$B wash /tmp/try --unset title --bak --apply
md5sum /tmp/try/*.bak > /tmp/b1
$B wash /tmp/try --unset title --bak --apply
md5sum /tmp/try/*.bak > /tmp/b2 && diff /tmp/b1 /tmp/b2 && echo "✓ 首备份保留"
```

注意：文件名带空格要加引号（`牵丝戏 - 白兀.flac`）。

### 9.2 服务端（起真服务打请求）

```bash
source env.sh
B=./target/debug/music-robot

rm -rf /tmp/srv && mkdir -p /tmp/srv/music
cp fixtures/*.mp3 "/tmp/srv/music/" 2>/dev/null; cp fixtures/*.flac /tmp/srv/music/ 2>/dev/null

export MR_DATABASE_PATH=/tmp/srv/music.db
export MR_LIBRARY_ROOTS=/tmp/srv/music
export MR_JWT_SECRET=dev-secret
export MR_PLUGINS_DIR=$PWD/plugins          # 示例已退到 plugins/examples/，不会遮蔽真插件（§6.3）

$B serve --host 127.0.0.1 --port 18099 &
curl -s localhost:18099/healthz               # 顺带看 plugins 字段：加载了几个、跳过几个

# 注册 / 登录 → 拿 token → 扫描 → 刮削 → 播放 / 封面 / 转码 / 歌单 / 点歌
curl -s -H 'Content-Type: application/json' -d '{"username":"root","password":"pass1234"}' \
  localhost:18099/api/auth/register
TOK=$(curl -s -H 'Content-Type: application/json' -d '{"username":"root","password":"pass1234"}' \
  localhost:18099/api/auth/login | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])')
curl -s -X POST -H "Authorization: Bearer $TOK" localhost:18099/api/scan
sleep 3 && curl -s -H "Authorization: Bearer $TOK" 'localhost:18099/api/library?page=1&page_size=5'
```

⚠️ **`/healthz` 上的 `plugins` 字段只给文件名与跳过数量，不给原因** —— 原因是中文但含服务器
绝对路径，而 `/healthz` 免鉴权。要看原因翻启动 stderr。

更省事的做法是直接跑 `scripts/api_test.mjs`，它把上面这些全做了一遍。

---

## 10. 快速参考

| 想知道什么 | 看哪里 |
|---|---|
| 架构 / 数据库 / 插件协议 / 28 步计划 | 画布 `music-server-architecture.excalidraw` |
| 服务端怎么组装、每个请求共享什么 | `src/server/state.rs` |
| 路由表与鉴权分组 | `src/server/routes/mod.rs` |
| 统一错误形状与状态码映射 | `src/server/error.rs` |
| 配置项默认值与 `MR_*` | `src/config.rs` |
| 依赖为什么引、能不能不引 | `Cargo.toml` 的注释（很详细） |
| 扫描四分级逻辑 | `src/scanner.rs` —— **分级按错误变体不是文案** |
| 扫描入库 + 多根去重 | `src/service/library.rs` |
| 刮削编排（顺序回退 / 阈值 / 冷却） | `src/service/scrape.rs` |
| 插件清单解析 / 协议 / 池 / 沙箱 | `src/plugin/{manifest,protocol,pool,sandbox}.rs` |
| 插件目录扫描与加载报告 | `src/plugin/registry.rs` |
| 服务端读写的唯一入口 | `src/storage.rs` + `src/fs.rs`（见 §7.6） |
| 转码与缓存清理 | `src/audio/transcode.rs` |
| 服务端日志（分级 / 落盘 / 清理） | `src/serverlog.rs`（约定见 §7.7）|
| 端到端证据 | `scripts/api_test.mjs`（非交互回归）/ `scripts/api_cli.mjs`（交互式手测） |
| 本机工作约定（NAS 资源保护 / docs 权限 / 子代理复用） | `$DSH_HOME/AGENTS.md` |

---

## 11. 附录：TS→Rust 移植史（原 `MIGRATION.md`，2026-09 并入）

> 这一节是**历史**。TS 参照实现已决定抛弃，所以下面提到的 TS 行号 / 文件路径只作考古用，
> **不要照着去改 TS**。仍然有效的未决疑点已提到 §6.4。

### 11.1 当初的环境与依赖策略

| 项 | 值 |
|---|---|
| 工具链 | rustup，装在用户目录，**无需 root**；`source /vol1/@appshare/dsh/tools/rust/setup.sh` |
| CPU | ARM Cortex-A55 ×4 @1992MHz，7.4G 内存 + zram |
| ⚠️ 禁止 | 构建产物放 `/tmp`（tmpfs 仅 3.7G）；apt 的 rustc 1.63 太旧 |

**依赖策略**（当时的表述是「产品代码零依赖」，现在早已不成立 —— 见 §5.1）：
标签读写全自研；`encoding_rs` / `sha2` / `serde_json` 只作为**平台能力补位**
（TS 侧由 Node 运行时免费提供，不是标签逻辑外包）；不引 clap。

`serde_json` 当初只为差分测试而引，后来差分测试删了、它反而成了服务端与插件协议的正式依赖。

### 11.2 一条「全绿」的假阳性（写侧，最值钱的一条）

`w02_bare_audio_hash_invariant` 最初报通过，但把 `verify` 改成**恒真**后它**仍然通过** ——
因为它比较的是 `audio_hash(原) == audio_hash(新)`，两侧都用同一个被测函数。
若 `mp3_audio_region` 本身算错（例如恒返回同一区间），这条断言永远成立。

修法是**加独立锚点**，不再只依赖被测代码自身：

- `w02b`：verify 恒假 → 必须拒绝落盘且原文件**逐字节不变**（验证闸门真的存在）；
- `w02c`：用「原始音频字节区间是否在新文件中逐字节可见」来判定，**绕开 hash 实现**
  （注入 off-by-one 吞掉 1 字节音频后，此条与另外 4 条同时红 ✓）。

写侧变异验证汇总：

| 注入 | 结果 |
|---|---|
| `raw_start = 0`（不跳 ID3v2）| ❌ 8 条红 |
| tag_size off-by-one（吞 1 字节音频）| ❌ 5 条红（含 w02c 独立锚点）|
| 多歌手改借位 TPE2 | ❌ 差分测试红：TS=`["甲","乙"]` vs Rust=`["2"]` |
| 完全摘掉 verify 传参 | ✅ 仍绿 —— **可接受**：它单独不损坏数据（写入内容未变，只是少了一道自检），而前两条证明真实损坏会被别的测试抓住 |

### 11.3 APEv2 重写：清警告时挖出的真问题

原 `apev2.rs` 是凭印象写的第一版，与参照有多处偏差；为消除一个未使用变量告警而逐行对照才发现：

| 偏差 | 后果 |
|---|---|
| **漏了 key 小写化** | TS `parseItems` 有 `.toLowerCase()`（APEv2 键大小写不敏感），下游按 `title`/`artist` 取值。**Rust 侧会全部取不到值** —— 真实功能缺陷 |
| 用 `for _ in 0..count` 驱动遍历 | TS 用 `while p+8<=len`。声明 count 与实际不符的畸形文件会被截断或越界 |
| 缺两处逃逸条件 | ① `key_end >= len` break ② 零长值且 key 贴末尾必须显式跳出，否则 **p 不前进 → 死循环** |
| `valueStart > len` 检查顺序颠倒 | 先算 valueEnd 再查 start，越界 panic 风险 |
| `parse_ape_tag` 里误用 `?` | 第一个候选位置失败即中止整函数，**永远试不到 ID3v1 之前那个位置**（TS 是 continue）。短文件还会因 `checked_sub(160)` 下溢直接返回 None |

变异验证：去掉 `.to_lowercase()` → ❌ 红（`left "Title"` vs `right "title"`）。

### 11.4 FLAC / WAV 写侧抓到的两个问题（都是自己造的）

**① 双重块头（实现缺陷）**：TS 的 `pieces` 存的是**含头完整块**，末尾 `subarray(4)` 剥头再重贴；
改成「Piece 只存 payload」后却把 `build_picture_block()`（带头）塞了进去 → PICTURE 头上再套一个头，
读侧解出 `type=0x06000031`、封面数 0。修法：内部统一用不带头的 `picture_payload()`。
**这类「移植时改了数据结构但没改全所有生产者」的错误，只有靠真实读回测试才能抓到。**

**② 又一个空壳断言**：`w03` 验证「RIFF size 必须重算」，注入陈旧 size 后仍绿。
诊断发现 `diff=0` —— 用的是 `lyrics` 字段，而 **WAV 的 LIST INFO 根本没有歌词槽位**，
writer 静默丢弃 → 文件尺寸没变 → 陈旧值恰好等于正确值。
修法：改用 `comment`(ICMT) 并**加前置断言** `assert_ne!(b.len(), orig.len())` ——
先证明「这次写入真的改变了尺寸」，再断言 size 被重算。

### 11.5 COMM 帧的坑（实测记录）

盛夏 / 老男孩等样本有 **2 个 COMM 帧**：第一帧 desc 本身是 UTF-16 乱码（`ÿþ`）、text 也是垃圾；
第二帧才是 `desc="ID3v1 Comment"`、text 为真 GBK（解出「酷我音乐」）。
定位必须**按 desc 内容匹配**（TS/Rust 同一判据），**不能取第一个 COMM** —— 否则会拿垃圾当结论。

### 11.6 已删除的差分测试留下了什么

`tests/differential.rs` / `tests/diff_write.rs` 是**跨语言实时对拍**（调 node 读 TS 输出、逐字段比对）。
它们完成了「证明 Rust 移植与参照一致」的使命后被删除。留下的两条教训仍然有效：

1. **测试静默 skip 会伪装成通过** —— `ts_read_json()` 返回 None 时 `return`，cargo 记作 `1 passed`。
   注入错误断言后仍绿才发现。**参照实现不可用即 panic，绝不允许静默跳过**（见 §7.1 第 1 条）。
2. **不能按 key 出现顺序切 JSON 区间** —— 带封面的 MP3 里 `"streams"` 排在 format 的 `duration`
   **之前**，切片后取不到 duration → 时长 0。改成花括号配平定位对象。

其他移植期踩的坑（UTF-8 字节边界 panic、FLAC PICTURE 大端、STREAMINFO 位域丢进位）
都已随实现修正，不再赘述。
