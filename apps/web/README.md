# apps/web —— PC 端（主界面）

Vite + React + TS + Tailwind v4 + shadcn/ui + `@tanstack/react-table` + React Router。

```
src/
├─ adapters/      core 的宿主实现：token-store.web.ts / audio.web.ts
├─ components/ui/ shadcn 拷进来的（按需加，别一次拷一片）
├─ components/app/ 本项目自己的组合件：app-shell / song-table / player-bar
├─ pages/         11 个页面，对着后端 33 条路由
└─ lib/utils.ts   shadcn 的 cn()
design/           设计产物（不是应用代码）
   prototype.html   主页面原型：**单文件、可交互、双击就能开**（不需要服务器）
                     首页 / 音乐库 / 专辑 / 歌手 / 歌单 / 收藏 / 设置 / 正在播放 + 登录页
                     点侧边栏切页、点行播放（播放条真的走时）、搜索过滤、
                     设置页的刮削开关联动警告、播放页有歌词/队列切换
   （设计令牌与理由见 ../../docs/design.md）
```

逻辑全部来自 `@music-robot/core`（workspace 包），**这里不放 core**。

## 三条不能破的

1. **业务逻辑写进 `packages/core/`，不要写在这里。** 这里只放 UI 与适配器。
2. `adapters/` 是 core 唯一碰宿主的地方（`token-store.web.ts` / `audio.web.ts`）。
3. `components/ui/` 是 shadcn 的，手改前想清楚 —— 以后要跟上游更新。

## 跑起来

```sh
pnpm install                                   # 仓库根
pnpm --filter @music-robot/web dev             # http://127.0.0.1:5173
```

开发时 `/api` 由 Vite 代理到 `http://127.0.0.1:8080`（见 vite.config.ts）。
后端指到别处就改那一行的 target。

## 现状（2026-10-01）

**已跑通**：登录 → 自动恢复登录（令牌存 localStorage）→ 音乐库列表（真数据，含脏标签识别）
→ **点行真的出声**（底部悬浮播放条 / 上一首下一首 / 进度可拖 / 四种播放模式）。

**还没做**：专辑/歌手/歌单/收藏/设置页（占位）、正在播放大页、shadcn 组件。

## 媒体鉴权：cookie 通道（后端已解决）

`/api/stream/{id}` 和 `/api/songs/{id}/cover` 曾在 `require_auth` 后面，
而 `<audio src>` / `<img src>` 发的是浏览器自发的裸 GET，**带不了 `Authorization` 头**
—— 于是音频和封面全都 401。

**后端已改**：这两条单独挂 `require_auth_media`，除请求头外还接受登录时下发的
`HttpOnly; SameSite=Lax; Path=/api` cookie（浏览器自动携带）。
`lib/client.ts` 的 `resolveMediaUrl` 因此**简化成了原样返回**，
恢复了真正的流式 Range 与 seek（不再「整个文件下完才播」）。

设计细节与安全支点见 `HANDOFF.md` §2「媒体端点鉴权」。
