# AGENT-HANDOFF.md —— 接手 music-robot 剩余工作

> **这份是给「下一个 agent」的**：边界、现状、剩下什么、不能破的规矩、我踩过的坑。
> 深度设计文档是 [`HANDOFF.md`](HANDOFF.md)（1200 行，含移植史与设计推演），
> 需要「为什么这么设计」时再翻它。**这份里的所有数字都是 2026-10-01 在
> commit `63be874` 上重新实测的**，不是抄的 —— 过期数字在这个项目里算 bug。

---

## 0. 30 秒判断你该不该接

| | |
|---|---|
| 项目 | 自托管音乐服务器。Rust 后端（axum + rusqlite，标签读写**全自研**）+ Web 前端（Vite + React） |
| 后端 | **已完成**，645 个用例全绿，接口 33 条路径 / 42 个操作 |
| 前端 | **主体已跑通**：15 条路由、15 个页面，真数据真播放。差收尾（见 §3） |
| 你大概率要做的 | ① 前端收尾与 UI 库决策 ② 标签编辑（**卡后端**）③ 点歌请求页 ④ S27/S28 |
| 不该做的 | 别重写标签引擎，别「顺手修好」§5.2 列出的那些**故意的**行为 |

---

## 1. 跑起来（这三条命令都实测过，照抄即可）

```bash
git clone https://github.com/CodeByZack/music-robot && cd music-robot
cargo build --release            # 首次慢：rusqlite 从 C 源码编 SQLite 静态库

cp env.example .env              # serve 会自动读**当前目录**的 .env，不用 source
# 编辑 .env，至少这两行：
#   MR_JWT_SECRET=<随便一串长的>
#   MR_LIBRARY_ROOTS=<绝对路径>   ← 手边没音乐就写 <仓库>/fixtures（9 个真音频，扫进去 8 首）

./target/release/music-robot serve --host 127.0.0.1 --port 8080

# 另开一个终端
pnpm install                     # 必须 pnpm（workspace:* 协议）
pnpm --filter @music-robot/web dev --host 127.0.0.1
# → http://127.0.0.1:5173
```

**第一次登录前必须先建号**（`/api/auth/register` 只在库空时可用，第一个用户自动是 admin）：

```bash
curl -X POST localhost:8080/api/auth/register -H 'content-type: application/json' \
  -d '{"username":"me","password":"至少八位"}'      # → 201
```

登录后到**设置页点「重新扫描」**才会入库。前端 dev 代理写死指向 `127.0.0.1:8080`。

### 踩坑速查（都是实测，不是猜的）

| 现象 | 真相 |
|---|---|
| 注册返回 **500 `AUTH_NOT_CONFIGURED`**，而 `/healthz` 是 200 | 没设 `MR_JWT_SECRET`。服务照常启动，只有注册挂，**日志里一个字不提** |
| 扫描「完成：遍历 0」、库是空的 | `MR_LIBRARY_ROOTS` 指错了。**目录不存在不会启动失败**（外接盘没挂上时还得能起来），启动日志搜「曲库根不存在」 |
| `127.0.0.1:5173` 连不上但 `localhost:5173` 能开 | Vite 默认只绑 `localhost`（本机解析成 `[::1]`）。加 `--host 127.0.0.1` |
| `ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION` | 你机器上开了 pnpm 24h 供应链策略，而 lockfile 里有当天发的包。见 §6.3 |
| `pnpm <任意命令>` 都被这个策略拦 | `pnpm <script>` 前会跑一次隐式 install 检查，**只给 `install` 加参数不管用** |

---

## 2. 现状（2026-10-01，commit `63be874`）

| 项 | 实测值 | 怎么复现 |
|---|---|---|
| `cargo test --lib` | **514 passed / 0 failed / 1 ignored** | `cargo test` |
| `cargo test`（`tests/`，11 个文件） | **131 passed / 0 failed** | 同上 |
| Rust 合计 | **645 passed / 1 ignored**（另有 3 条 doc test 是 ignored） | 同上 |
| 编译警告 | **0** | `cargo check --lib --bins` |
| 端到端 API 脚本 | **195 通过 / 1 失败**（那 1 条是既有的「封面同样认 cookie」） | `node scripts/api_test.mjs` |
| core 单测（零依赖，node:test） | **34 通过 / 0 失败** | `cd packages/core && node --test 'src/**/*.test.ts'` |
| Web 构建 | **121 modules**，JS 317.50 kB（gzip 97.10）· CSS 30.57 kB（gzip 6.57） | `pnpm --filter @music-robot/web build` |
| HTTP 接口 | **34 条路径 / 43 个操作**（`/api/admin/users` 的 GET 与 `/api/history/stats` 是 2026-10-02 新增） | 对着 `src/server/routes/mod.rs` 数 |
| 前端页面 | **15 条路由 = 15 个页面文件**（含 3 个全屏浮层页：`/settings` `/now` `/songs/:id/tags`） | `ls apps/web/src/pages/` |
| 画布进度 | **26 / 28**（`st_*` 卡里 `backgroundColor === "#bbf7d0"` 的个数）；未绿的是 **S27 文件整理** 与 **S28 集成打包** | `music-server-architecture.excalidraw` |
| 代码量 | Rust 39864 行 · `apps/web` 2671 行 · `packages/core` 1097 行 | `wc -l` |
| 前端测试 | **0 个**（`find apps/web -name '*.test.*'` → 空） | 见 §3.4 |

### 前端依赖（**就这四个**，别乱加）

```
dependencies:     @music-robot/core(workspace) · react 19 · react-dom 19 · react-router 8
devDependencies:  vite 8 · @vitejs/plugin-react · tailwindcss 4 + @tailwindcss/vite · typescript 7 · @types/*
```

**没有** axios、没有 UI 组件库、没有 react-query、没有状态管理库。
`packages/core` 是**零依赖零构建**的（源码直接被 Vite/Metro 编，不建 dist）。

---

## 3. 还剩什么

### 3.1 卡在**后端**的（前端做不了，得先加接口）

**① 标签编辑页 —— ✅ 已完成（2026-10-02）**
`GET /api/songs/{id}/tags` + `PATCH`（仅 admin，**`dry_run` 默认 true**），前端
`/songs/:id/tags` 全屏浮层：强制「先预览、改动即作废旧预览」，可勾选写前备份。
不用再设计接口了 —— 现有的那套把「不可撤销」这件事处理得很细（四态：未改动 /
未预览 / 预览已过期 / 已预览 N 处），改之前先读 `src/server/routes/tags.rs` 的头注释。

**② 点歌请求 —— ✅ 已完成（2026-10-02）**
**设置页的一个分节**（不是独立页面，用户 2026-10-02 明确要求并进去）。
需求侧入口在搜索页搜不到时的「请求这首歌」（弹窗填表、预填搜索词）。
`packages/core` 的 `api.requests.*`（5 条）已补齐。代码在
`apps/web/src/pages/settings/requests-section.tsx`。
注意：`POST /api/requests/{id}/fetch` **恒返回 503 是故意的** —— 它要的是
`provider`（下载）插件 kind，而注册表目前只加载 `kind = "scraper"`。别当 bug 修。
（前端**故意没放这个按钮**：点了只会弹一条 503。）

**②b 用户管理 —— ✅ 已完成（2026-10-02，含一个后端新接口）**
也是**设置页的一个分节**（标了 `adminOnly`，普通用户看不到该项）。
`GET /api/admin/users`（仅 admin，**复用 `user_json`、绝不带 password_hash**）
是本次补的；`POST` 那条本来就有。代码在
`apps/web/src/pages/settings/users-section.tsx`。
**不做**改密码 / 删号：前者后端没接口，后者会牵动 playlists / favorites /
history / song_requests 一串 CASCADE —— 没有需求就别动。

**③ 刮削失败原因没露到 API**
单曲重刮失败时，界面上只有任务摘要「失败 1」，真正的原因
（`musicbrainz：插件报错（NOT_FOUND）：没有找到匹配的录音`）**只在服务端日志里**。
要露出就得动 `src/service/scrape.rs` 的批次 message（单曲批次带上失败原因）。

**④ provider（下载）插件 kind** —— 见上面 ②。

**⑤ S27 文件整理 · S28 集成打包** —— 未开始，画布上还是灰卡。
`token_expiry_hours` 默认 24h 且**没有 refresh 接口**（见 `apps/mobile/README.md`）。

### 3.2 前端收尾（不卡后端，可以直接做）

| 项 | 说明 |
|---|---|
| **shadcn/ui 没装** | 所有 UI 都是手写 Tailwind。**决策没落地** —— 用户说过「换吧」，但装依赖那步一直卡着（§6.3），所以到目前为止全是手写的。见 §3.3 |
| 设置页的「写文件」开关不持久化 | 内存级（`lib/scrape-prefs.ts`），F5 后回默认的安全档 `false`。要持久化就存进后端 `settings`（通用键值表，`resume:<id>` 就是这么用的） |
| 设置页的「音量 / 播放模式」是死 UI | 那两行永远显示「后端没有这个键」—— `volume` / `play_mode` **从来没有被写过**（画布 ④ 要求它们存 `user_settings`）。要么接上、要么把两行删掉 |
| ~~播放统计没有页面~~ | ✅ **已完成（2026-10-02）**：后端 `GET /api/history/stats`
（总量 / 歌榜 / 歌手榜 / 按天，带时区切天）+ 前端 `/stats`（入口在首页「最近播放」
标题行右侧）。⚠️ 「累计时长」会偏小 —— 早于上报功能的历史行是 NULL |
| ~~播放列表缺一半操作~~ | ✅ **已完成（2026-10-02）**：改名 / 描述 / 可见性、删歌单、
从歌单移除、拖动排序（含「上移 / 下移」兼容触屏与键盘）全做了。
一并补了后端的 `is_owner` —— 前端靠它决定给不给编辑入口（见 `routes::playlists.rs`） |
| `api.jobs.list` 从未被调用 | 任务列表接口有客户端，界面只轮询单个 batch |
| ~~画布 S25/S26 卡片文字过时~~ | ✅ **已修（2026-10-02）**：S25/S26 已标绿，标题带
  「已完成 · 前端 UT 未写」。⛔ 但卡片上的「交付 / UT」那两行仍是**当初的计划原文**
  （画布上所有绿卡都是这个规矩）—— 实际交付有出入，别把计划当现状：
  S26 的「请求页」现已并入设置页，不是一个页面；「前端 UT」全部未写 |

### 3.3 UI 库这件事（**接手的第一个决策点**）

用户明确说过要换 shadcn/ui，但**没落地**，原因很具体：

1. 他的机器上 `pnpm install` 卡在 24h 供应链策略（§6.3），**加了依赖等于把「能不能跑起来」压在一件没验证的事上**；
2. shadcn 的默认令牌是中性 zinc + oklch，本项目有一套从**飞牛音乐**抽出来的令牌
   （`docs/design.md` §9 是可直接粘的 CSS 变量），**真拷进来得先对齐两套色系**。

**我的建议（不是结论，你可以推翻）**：不要全量迁移。
按需拷 Radix 那几个手写不出来的：`Dialog` / `DropdownMenu` / `Select` / `Slider` / `Toast`。
本仓库现在有一处**手写的 popover**（`apps/web/src/components/menu.tsx`），
它就是第一个该换成 Radix `DropdownMenu` 的候选 —— 手写版**刻意没做键盘导航**
（上下键在菜单内移动、首字母跳转），只做了「点外面关 / Esc 关 / 点项关 / 粗略向上翻转」。

### 3.4 测试缺口（**这个项目最在意的事**）

- **`apps/web` 一个测试都没有。** 画布 S25/S26 明写着要 UT（列表渲染 / 播放器状态机 /
  seek 触发 Range / 表单校验 / 轮询进度渲染），**一条没写**。
- 已覆盖的部分：纯逻辑全在 `packages/core`（34 个 node:test），
  接口层靠 `scripts/api_test.mjs`（171 项，**起真服务逐条打 HTTP**），
  Rust 侧 635 项。
- 规矩见 §7.1：**没有变异验证过的「全绿」不算证据**。

---

## 4. 目录地图（只看你要动的）

```
src/                      Rust 后端（39864 行）
  cli/                    7 个子命令，都是 run(argv, io) -> i32 的纯函数
  server/routes/          HTTP 层（mod.rs 是路由总表 + 中间件挂载）
  service/scrape.rs       刮削编排（单曲 / 批量 / 只入库）
  tag/                    自研标签引擎（ID3v2/v1 · FLAC · WAV · APEv2）
  plugin/                 插件宿主（清单 / 协议 / 进程池 / 沙箱）
apps/web/                 PC 前端（**主战场**）
  src/pages/              14 个页面，对着 14 条路由；其中 /settings /now /songs/:id/tags
                          是 `fixed inset-0` 全屏浮层
                          （标签编辑拆到 settings/ 那样子的子目录：tag-edit/）
                          点歌请求与用户管理**不是页面** —— 是设置页的两个分节
  src/components/         shell（侧边栏+抽屉+头像菜单）· player-bar · song-table · menu
  src/lib/                client / session / player / resume / scrape-prefs / use-async
  src/adapters/           **core 唯一碰宿主的地方**：token-store.web.ts · audio.web.ts
  design/prototype.html   单文件可交互原型（双击就能开，不需要服务器）
apps/mobile/              **空壳**，只有一份 README 写着开工前要补什么
packages/core/            ★ 两端共用：api / types / player-queue / resume / http / token-store
                          零依赖零构建；**不碰任何宿主 API**（判据：这段代码在 Node 里能跑吗）
docs/design.md            从飞牛音乐抽的设计系统（色板/字阶/间距/圆角/7 个手法/§9 令牌）
docs/expo-rn-ui-libs-research.md   RN 组件库调研（含「读文档 vs 实际用」的差异记录）
HANDOFF.md                ★ 深度设计文档（1200 行）
music-server-architecture.excalidraw  ★ 28 步计划与进度（区块 ⑫）
plugins/musicbrainz.js    唯一的生产插件（真实刮削）；plugins/examples/ 是示例
scripts/api_test.mjs      端到端 API 脚本（起真服务；会拒绝跑陈旧二进制）
```

### 架构上必须守住的一条线

```
packages/core   纯业务：不 import react、不碰 window/document/localStorage、不用 <audio>
apps/<app>/src/adapters/   宿主能力：token 存储、音频播放
```

将来做移动端时，**只需要新写 `token-store.native.ts` / `audio.native.ts` 两个文件**，
core 一行不用改。这是当初选「Expo 壳 + Web 写 React DOM」的直接原因
（取舍过程见 `HANDOFF.md` §6.6）。

---

## 5. 不能破的规矩

### 5.1 硬性

1. **生产路径（`src/`）不写 `unwrap` / `expect` / `panic!`**。测试里随便用。
   复核命令在 `HANDOFF.md` §7.5。
2. **中文**。注释、日志、界面文案、提交信息全是中文。
3. **`cargo` 一律套 `timeout`**（`timeout 600 cargo build`）。这台是 ARM NAS，卡住会很难看。
4. **加依赖前先问用户**。Cargo.toml 里每条依赖都写了引入理由，别绕过。
5. **提交前 0 警告**（`cargo check`）。
6. 前端：**业务逻辑写进 `packages/core/`**，`apps/web` 只放 UI 与适配器。

### 5.2 「看着像 bug，其实是故意的」（**别顺手修**）

| 现象 | 真相 |
|---|---|
| `POST /api/requests/{id}/fetch` 恒 503 | 缺 `provider` 插件 kind（≠ 刮削插件），文案已改准，别改回模糊说法 |
| MusicBrainz 插件的 `album` 几乎永远为空 | **刻意留空**。搜索接口返回的 releases 是截断的，补 lookup 会把「别人的再版」当用户的原专辑写进库 |
| MusicBrainz 的 `score` / 返回顺序 | 中文曲库里**没有区分度**（实测同一查询连打 4 次，正确歌手一会儿第 1 一会儿掉出前 3，候选全是 score=100）。真正能区分的是**本地歌手 + 时长**，插件的 `rankKey` 就是按它们排的 |
| 刮削**直接写原文件**、没有备份 | 但已加了「只入库不写文件」档位（`write_files`，**默认 false**）。逐曲日志（before→after）是覆盖后唯一的痕迹，**别删** |
| 媒体端点收 cookie | 只挂 `require_auth_media` 的媒体子路由、**且只认 GET**；其余接口只认 Authorization 头（CSRF 面 = 0）。有变异测试锁死，见 `HANDOFF.md` §3「媒体端点鉴权」 |

---

## 6. 我踩过的坑（按代价排序）

### 6.1 `pkill -f` 会杀掉自己（踩了 3 次）

`pkill -f "music-robot serve"` / `pkill -f "vite/bin/vite.js"` —— **pattern 会匹配到你自己的
shell 命令行**，脚本当场自杀（表现为「工具调用被中断，结果未知」）。同理
`pgrep -x MainThread` 会匹配到 vite（甚至浏览器）的进程名。

**用这个**：

```bash
ss -ltnp | grep 5173 | grep -oP 'pid=\K[0-9]+' | head -1   # 先拿 PID，再 kill 它
pkill -x music-robot                                        # 完全匹配进程名
```

### 6.2 前端改了不生效

- `lib/use-async.ts` 里有 JSX 就必须叫 **`.tsx`** —— tsc 能过，Vite 直接 500。
- **`pnpm build` 才是真闸门，`typecheck` 不是**（踩过：tsc 全绿、页面 500）。
- 关掉 vite 再起，否则你看的是旧 bundle。

### 6.3 pnpm 24h 供应链策略（用户机器上）

现象：`ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION`，17 条 lockfile 记录。
根因：lockfile 里钉了当天刚发的 `rolldown@1.2.12`（Vite 8 的打包器）+ 15 个平台二进制 + `source-map-js@1.2.2`。
处理（已提交 `1280b51`）：在 `pnpm-workspace.yaml` 的 `overrides` 里钉到成熟版本
（`rolldown: 1.2.11`、`source-map-js: 1.2.1`）。两条都在 vite 声明的 `~1.2.9` 范围内。

**试过但走不通的路，别再试**：在项目 `.npmrc` 里写 `minimum-release-age=1440`。
rolldown 的平台绑定是**精确版本的可选依赖**，pnpm 遇到「太新」的绑定不会回退到旧版
rolldown，而是直接 `ERR_PNPM_NO_MATURE_MATCHING_VERSION` 让整个解析失败。

**没能验证的**：这台 NAS **复现不出**那个策略检查（`--config.minimum-release-age=999999`
和写进 `.npmrc` 都照装不误），所以「策略开着且通过」不是本地实测的，是推出来的。
用户那边是否已装通，**接手时先问清楚**。

### 6.4 前端联调

- **`Page.navigate` 是整页刷新**，会重置模块级内存状态（比如 `scrape-prefs`）。
  测「跨页面共享的状态」必须走**客户端路由**（点侧边栏/链接），不能 navigate。
- 浏览器里留着**旧 token**（同源不同后端、换了 `MR_JWT_SECRET`）会被顶回登录页，
  现象是「点了没反应」。**每次 UI 测试先 `localStorage.clear()`**（我被这个骗过一次，
  白跑了整轮验证）。
- fygo-browser 的 CDP 在 `127.0.0.1:16003`，**约 1 分钟空闲就被回收**，
  每个脚本都要先唤醒（`/tmp/cdp-run.mjs` 里有现成的逻辑，20 行，没有就自己写一份）。
- `<audio>` 是 `new Audio()` 出来的**游离元素**，`document.querySelector('audio')` 找不到 ——
  测播放只能走 UI（点行播放 / 点进度条 seek / 点暂停），位置从播放条文字读。

### 6.5 推送（**顺序错了会把 commit 弄丢**）

```bash
node "$DSH_HOME/skills/github-access/gh-bridge.mjs" push
git rev-parse origin/main        # 必须等于本地；不等就别 sync
node "$DSH_HOME/skills/github-access/gh-bridge.mjs" sync
```

- **`push` 失败不会阻止你接着敲 `sync`**，而 `sync` 会把本地分支指到远端 SHA ——
  等于把手上的 commit 从分支上抹掉（这个坑在本项目上吃过两次）。
- `push` 的成败**一律问 API**，不匹配 git 的输出文字（网络错误的信息曾经
  恰好匹配上「already up to date」）。
- **桥接可能回退到 Git Data API，从而造出一个新 SHA**（我这次就是：本地 `d21626c`
  → 远端 `63be8741`）。这时**必须验证「树 SHA + 提交信息」一致**再 sync：

```bash
git rev-parse HEAD^{tree}
gh api repos/CodeByZack/music-robot/commits/<远端SHA> --jq '.commit.tree.sha,.commit.message'
```

---

## 7. 工作方式（这个项目的口径）

### 7.1 证据 ≠ 声称 ⭐

- **没有变异验证过的「全绿」不算证据**：改一处逻辑，确认测试真的变红，再改回来。
  （本轮 `packages/core/src/resume.ts` 就跑了 6 个变异，全部被杀。）
- **手写 API 转写层必须对着真服务验**：从源码读出来的路径/方法/出参形状，
  在这个项目上已经错过 **6 次**（`favorites.add` 是 path 不是 body、`playlists.list`
  返回 `total` 不是 `count`、`addSong` 收单首不是数组……）。已全部改正并被
  `api_test.mjs` 的 smoke 段覆盖。
- **文档里的数字要重测再写**。HANDOFF §0 明写「最新实测，别照抄旧数字」。
- 报告时**分清楚「实测的」和「推出来的」**。本文件 §6.3 末尾就是个例子。

### 7.2 风格

用户对这个项目做过明确要求：**精简**。少写抽象、少加依赖、能删就删；
但**不许为了短而简化掉安全、明确的需求、或理解本身**。
（仓库里有个 `ponytail` 技能就是这个口径；`AGENTS.md` 在 `$DSH_HOME` 下。）

### 7.3 每步的固定流程

1. 改 → 2. `cargo check` 0 警告 / `pnpm build` 过 → 3. 跑相关测试 →
4. **把服务真跑起来打请求或开浏览器看一眼**（这一步抓到过单测全绿但真实存在的漏洞）→
5. 更新画布对应卡片（做完了就 `backgroundColor = "#bbf7d0"`）→ 6. commit + push + sync。

---

## 8. 交接状态

| | |
|---|---|
| 分支 | `main`，本地 = 远端 = **`63be874`** |
| 工作树 | **干净** |
| 最近三个提交 | `63be874` 前端四处界面反馈 · `3a70a47` 界面缺口修复 · `1280b51` 依赖钉版本 |
| 未提交/未跟踪 | 无 |
| 待用户确认 | ① `pnpm install` 在他机器上到底过没过 ② shadcn 换不换、换哪几个 |

**建议的第一步**：先跑 §1 的命令把服务起起来，用 `fixtures/` 当曲库（9 个真音频，
扫进去 8 首），把 14 个页面点一遍。**亲眼看过再动手** —— 这个项目里
「看着能跑」和「真能跑」差得挺远的。
