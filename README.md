# music-robot

**自托管的音乐服务器**（Rust）—— 把散落各处的音乐目录扫进库，提供检索、流式播放、按需转码、封面、歌单、播放历史与点歌，外加一套**插件化刮削**子系统。

标签读写层（ID3v2 / ID3v1 / FLAC / WAV / APEv2）**全部自研**，不使用 lofty、id3 等任何第三方标签库。

```
CLI ──┐
      ├─→ 标签引擎 (src/tag)  ← 自研读 / 写 + 原子替换 + 音频 hash 校验
HTTP ─┘        ↑
         扫描入库 / 去重 (src/service)
               ↑
         SQLite (rusqlite)     插件宿主 (src/plugin) → 不可信子进程
```

---

## ⚠️ 项目状态：后端完成，前端骨架已跑通

28 步实施计划与全部架构设计记录在画布 **[`music-server-architecture.excalidraw`](music-server-architecture.excalidraw)**（用 [Excalidraw](https://excalidraw.com) 打开）。当前进度 **24/28**。

| 层 | 状态 |
|---|---|
| 标签引擎（读 / 写）· CLI（7 个子命令） | ✅ |
| 扫描入库 · 多根去重 · 路径沙箱 | ✅ |
| 插件宿主（清单 / 协议 / 进程池 / 沙箱） | ✅ |
| HTTP 服务（**33 条路径 / 41 个操作**） | ✅ |
| 认证（JWT HS256 + argon2id）· 后台长任务 | ✅ |
| 流式播放（Range）· 按需转码 · 封面 | ✅ |
| 歌单 · 播放历史 · 收藏 · 设置 | ✅ |
| 点歌请求 | ✅ |
| **Web 前端（Vite + React，12 个页面跑真数据）** | ✅ S24 骨架完成，S25–S26 收尾中 |
| 文件整理 · 集成打包 | ❌ 未开始 |

### 两个已知缺口（不是 bug）

1. **`POST /api/requests/{id}/fetch` 恒返回 503。** 缺的是 **provider（下载）** 插件 —— 它要把音频文件本身下下来。注册表目前**只加载 `kind = "scraper"`**。这两条插件线互相独立，别混。
2. **MusicBrainz 插件的 `album` 几乎永远为空，这是刻意的。** 搜索接口返回的 `releases` 是截断的，补一次 lookup 反而会把「别人的再版」当成用户的原专辑写进库。插件的选择是**留空**（协议里字段缺省 = 不修改）。

---

## 特性

### 标签引擎（`src/tag/`，全自研）

- **读**：ID3v2（含 USLT / APIC / TXXX / unsync / 三版本帧头）· ID3v1（148 项流派表）· FLAC（Vorbis Comment + PICTURE）· WAV（LIST INFO + 内嵌 id3 chunk）· APEv2 · ffprobe 双通道
- **写**：MP3 局部编辑（**保留未知帧**）· FLAC 键级局部编辑 + PICTURE 重建 · WAV INFO 子项级编辑 + RIFF size 重算 · `atomic_replace` 原子替换 + **裸音频 hash 校验**
- **告警检测**：广告词 / GBK 乱码 / ID3v1 垃圾尾 / 非白名单帧

### CLI

`read` · `write` · `blank` · `scan` · `doctor` · `wash` · `serve`

全部实现为**返回退出码的纯函数**（`run(argv, io) -> i32`），不 `exit()`、不写死 stdout —— 所以测试可以直接在内存里调，服务端也能直接复用。

退出码约定：`0` 成功 / `1` 运行期失败 / `2` 用法错误；`wash` 为 `0` 全部 applied / `1` 有 failed。

### 服务端

- **认证**：JWT HS256（拒绝 `alg:none`）+ argon2id 口令哈希 + `require_auth` 中间件 + admin 守卫
- **后台长任务**：单例锁（并发触发恰好 1 个成功、其余 409），扫描 / 刮削跑在独立 OS 线程
- **播放**：HTTP Range 流式 + 按需 ffmpeg 转码 + 磁盘缓存（启动时清理过期缓存）
- **隔离**：历史 / 收藏 / 设置**全部按登录用户隔离**，`user_id` 只来自令牌，绝不从请求体或查询串读取

### 插件子系统

插件是**单文件脚本**（`plugins/*.js|.py|.sh`），运行时为**不可信子进程** —— 不做嵌入式 JS / WASM / .so。

- 命令由扩展名推断（`.js → node`、`.py → python3`、`.sh → sh`），**零外部依赖**，无声明式 YAML
- 协议：stdin/stdout 一行一个 JSON（`protocol: 1`），响应必须原样回显 `action`，诊断信息一律走 stderr
- 沙箱：`setsid` + 6×`setrlimit` + `NO_NEW_PRIVS` + `PDEATHSIG` + 降权
- 插件按**文件名升序**尝试、命中即停
- 开箱即用的真插件是 `plugins/musicbrainz.js`；`plugins/examples/` 里那三个是**假数据示例**
  （写死「示例歌手」），只给测试当夹具，不参与你的刮削
- **`musicbrainz.js` 优先从文件名解析「歌名-歌手」**，解析不出来才用标签 ——
  盗版资源的标签常被塞广告（`公众号：阿乐资源库`），拿它去查必然查不到，文件名反而干净。
  查询用「歌名+歌手」，查不到才退回「只查歌名」；候选还要过一道闸（歌手或时长至少一个
  对得上）才认，**宁可漏也不写错**

> ⚠️ **刮削会直接改写你的音乐文件，且没有撤销。**
> 命中后按插件给的标签**覆盖**原文件（`atomic_replace` 保证「中途失败原文件完好」，
> 但一旦成功就是新文件了），DB 里的旧值也一起被覆盖。**没有 `.bak`、没有 dry-run、没有确认框**，
> `POST /api/scrape` 不给 body 时一调就是全库 pending 队列（可以传 `song_ids` 只听点名的几首，但**没有「只入库不写文件」这个档位**）。
> 先拿几首**副本**试插件，确认它写出来的标签是你想要的，再对真库下手。
> 每次刮削都会逐曲写日志（`[scrape]`），含**改动明细**：
> `曲目 3 命中：插件 musicbrainz（confidence 0.92）改动 2 处：歌手「公众号：阿乐资源库」→「白小白」；…`
> —— 文件被覆盖后，这行日志是唯一能还原「改了什么」的地方，**别关 info 级日志**。
> 想知道**插件到底收到什么、回了什么**，用 `MR_LOG_LEVEL=debug` 起服务，日志里会有
> `调用插件 <名>：{请求 JSON}` 与 `插件 <名> 返回：{响应 JSON}` 成对的行。

---

## 快速开始

### 构建

需要 Rust stable（edition 2021，实测 1.98.1）。

```bash
cargo build --release
# 产物：target/release/music-robot
```

> 首次编译较慢：`rusqlite` 走 bundled、会从 C 源码编译 SQLite 静态库（不依赖系统开发库和 pkg-config）。

### CLI 试一下

```bash
B=./target/release/music-robot

$B -h
$B read   "song.mp3"                       # 查看标签（表格 + 告警）
$B read   "song.flac" --json               # ⚠️ --json 要放最后，见下方「已知行为」
$B scan   ./music                          # 批量分级：ok / warn / rejected / broken
$B doctor ./music                          # 环境自检 + 全库体检（只读）
$B write  "song.mp3" --title 新标题 --preview   # 只算差异，不落盘
$B wash   ./music --unset comment --bak --apply # 批量清洗，写前备份
```

`wash` 默认是 **preview**（只打印将执行的剧本），加 `--apply` 才落盘。

### 起服务

```bash
export MR_DATABASE_PATH=/tmp/music.db
export MR_LIBRARY_ROOTS=/path/to/music        # 多个根用 : 或 , 分隔
export MR_JWT_SECRET=$(head -c32 /dev/urandom | base64)
export MR_PLUGINS_DIR=$PWD/plugins

$B serve --host 127.0.0.1 --port 18099

curl -s localhost:18099/healthz               # 顺带看插件加载情况
```

配置加载顺序：**内置默认值 → 配置文件 → `.env` → `MR_*` 环境变量 → 命令行**。变量未设置 = 不动；设置成空白串会**直接报错**（早失败好过静默用半截配置）。

`.env` 由 [`dotenvy`](https://crates.io/crates/dotenvy) 在 `serve` 启动时读取（当前目录及父目录），且**不覆盖已存在的变量** —— 所以真环境变量总能压过 `.env`，临时覆盖不用改文件。

| 环境变量 | 作用 |
|---|---|
| `MR_JWT_SECRET` | JWT 签名密钥 |
| `MR_LIBRARY_ROOTS` | 曲库根目录（`:` 或 `,` 分隔） |
| `MR_DATA_DIR` | 数据根：一个变量管住数据库 / 转码缓存 / 日志（默认 `~/.local/share/music-robot`） |
| `MR_DATABASE_PATH` / `MR_CACHE_DIR` | 单独挪走数据库或缓存（优先于 `MR_DATA_DIR`） |
| `MR_FFMPEG_PATH` | ffmpeg 可执行文件（走 PATH） |
| `MR_PLUGINS_DIR` / `MR_SANDBOX` / `MR_PLUGIN_USER` | 插件目录与沙箱（⚠️ 后两者目前**能读能校验，但尚未真正接进沙箱行为**，沙箱用的是固定默认值） |
| `MR_LOG_LEVEL` | 日志级别 `error`/`warn`/`info`/`debug`/`trace`（写错启动即报错）|

完整带注释的模板见 [`env.example`](env.example)。

### 起前端（PC 页面）

后端跑起来**只**提供 API，界面在 `apps/web`（Vite + React）。两个进程，两个端口：

```bash
# ① 后端 —— 必须在 8080，前端 dev 代理写死了这个地址
cp env.example .env          # serve 会自动读当前目录的 .env，不用 source
vi .env                      # 至少改 MR_JWT_SECRET 和 MR_LIBRARY_ROOTS
./target/release/music-robot serve --host 127.0.0.1 --port 8080

# ② 前端（另开一个终端，仓库根）
pnpm install                 # 必须用 pnpm：workspace 里用的是 workspace:* 协议，npm 解析不了
pnpm --filter @music-robot/web dev --host 127.0.0.1
# → http://127.0.0.1:5173
```

**第一次登录前要先建号**：`POST /api/auth/register` **只在库空时可用**，建出来的第一个用户就是 admin。

```bash
curl -X POST localhost:8080/api/auth/register -H 'content-type: application/json' \
  -d '{"username":"me","password":"至少八位"}'
# → 201 {"user":{"id":1,"role":"admin",...}}
```

（之后再加人走 `POST /api/admin/users`，带 admin 的 token。登录后到**设置页点「重新扫描」**才会入库。）

手边没有音乐也能看界面：把 `MR_LIBRARY_ROOTS` 指到仓库自带的 `fixtures/`（9 个真音频文件，扫进去 8 首），扫一下就有内容了。

**踩坑速查**

| 现象 | 原因 |
|---|---|
| 注册返回 **500 `AUTH_NOT_CONFIGURED`** | 没设 `MR_JWT_SECRET`。服务照常启动、`/healthz` 也 200，只有注册会挂 |
| 扫描「完成：遍历 0」，库是空的 | `MR_LIBRARY_ROOTS` 指错了。**目录不存在不会启动失败**，启动日志里搜「曲库根不存在」 |
| 前端 404 / 请求全挂 | 后端不在 **8080**（`apps/web/vite.config.ts` 的代理目标写死了；改那一行也行） |
| `pnpm install` 报 `workspace:*` | 用了 npm/yarn。这个仓库只支持 pnpm |
| `http://127.0.0.1:5173` 连不上但 `localhost:5173` 能开 | 没加 `--host`。Vite 默认只绑 `localhost`，这台机器上解析成了 `[::1]`，IPv4 就没监听 |

---

## HTTP API

共 **33 条路径 / 41 个操作**，与 [`src/server/routes/mod.rs`](src/server/routes/mod.rs) 一一对应。除公开组外全部要求 `Authorization: Bearer <token>`。

| 分组 | 路径 |
|---|---|
| 公开 | `GET /healthz` · `POST /api/auth/login` |
| 引导 | `POST /api/auth/register` —— **仅库空时可用**（初始化出首个 admin），之后 403 |
| 认证 | `GET /api/auth/me` · `POST /api/admin/users`（admin 建号）· `GET /api/admin/ping` |
| 曲库 | `GET /api/library` · `/api/songs/{id}` · `GET /api/albums` · `/api/albums/{id}` · `GET /api/artists` · `/api/artists/{name}` · `/api/search` |
| 任务 | `POST\|GET /api/scan` · `GET /api/scan/{batch_id}` · `POST\|GET /api/scrape` · `GET /api/scrape/{batch_id}` · `GET /api/jobs` |
| ↳ 刮削队列 | `POST /api/scrape` body 可选：不给 = pending 队列；`{"mode":"failed"}` = 重刮失败项；`{"song_ids":[1,2]}` = 只听点名的（**已 done 的也能重刮**） |
| 音频 | `GET /api/stream/{id}`（Range · `?format=mp3` 转码）· `GET /api/songs/{id}/cover` |
| 歌单 | `/api/playlists` · `/{id}` · `/{id}/items` · `/{id}/items/{song_id}`（8 个操作） |
| 播放周边 | `/api/history` · `/api/favorites` · `/api/favorites/{song_id}` · `/api/settings`（7 个操作） |
| 点歌 | `POST\|GET /api/requests` · `PATCH /api/requests/{id}` · `POST /api/requests/{id}/fetch｜link` |

---

## 设计要点

- **`unwrap` 纪律**：生产路径（`src/`）不写 `unwrap` / `expect` / `panic!`，测试里随便用。详见 [`HANDOFF.md`](HANDOFF.md) §7.5 的口径与复核命令。
- **零第三方标签库**：`encoding_rs`（GB18030 码表）、`sha2`（音频完整性 hash）只作**平台能力补位**，不是标签逻辑外包。`Cargo.toml` 里逐条写了引入理由。
- **服务端读写的唯一入口**：handler 里绝不直接 `std::fs::read`，一律走 `StorageBackend`（内部对每个库根复用 `PathSandbox`，fail-closed 地 canonicalize）。
- **路径沙箱不是权限模型**：它只回答「路径在不在库根下」，不回答「这个用户有没有权限」。多租户隔离靠上层 token/tenant 映射到不同 root。
- **测试哲学：没有变异验证过的「全绿」不算证据。** 写断言前先回答「改坏什么会让它变红」，然后真的改坏一次确认变红。

---

## 测试

```bash
cargo test --lib            # 库用例    510 passed / 0 failed / 1 ignored
cargo test --tests          # 库 + 全部集成（集成 124）
node scripts/api_test.mjs # 端到端 API（起临时服务逐条断言，171 项）
node scripts/api_cli.mjs  # 交互式客户端（连已在跑的服务，菜单选功能）
```

`scripts/api_test.mjs` 自带两个防呆：**拒绝跑陈旧二进制**（二进制比源码旧就拒绝运行）、**转码缓存目录已隔离**（不会写脏你的 `~/.cache`）。

> 测试样本 `fixtures/`（65M）不入库。它是真实脏数据样本（广告词 / GBK 乱码 / ID3v1 垃圾尾），重建方式见 [`HANDOFF.md`](HANDOFF.md) §8.3。

---

## 已知行为（**刻意的，不要「修好」**）

这几条看起来像 bug，但属于**破坏性变更**，改动会影响已有用户的脚本与预期：

| 现象 | 原因 |
|---|---|
| 纯文本冒充 `.mp3` → `scan` 判 `ok` | MP3 是 sync-marker 格式，没有帧也合法 |
| `read --json <file>` 中 `--json` 会吃掉后面的文件 | 参数解析规则：下一个参数不以 `--` 开头即取值。正确写法 `read <file> --json` |
| `wash` 对「文本冒充 mp3」执行 blank 会改写文件 | `scan`「可解析」与 `wash`「可处理」之间的语义缝隙 |

---

## 文档

| 想看什么 | 去哪 |
|---|---|
| **架构 / 数据库 / 插件协议 / 28 步计划** | 画布 `music-server-architecture.excalidraw` |
| **接手指南**（环境、进度、测试基线、踩坑记录、未决疑点） | [`HANDOFF.md`](HANDOFF.md) |
| 依赖为什么引、能不能不引 | [`Cargo.toml`](Cargo.toml) 的注释 |

> 架构设计**不在 markdown 里**，全在画布。要看设计看画布。

---

## 许可

双许可 **[MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)**。
