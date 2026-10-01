# apps/web —— PC 端（主界面）

Vite + React + TS + Tailwind v4 + shadcn/ui + `@tanstack/react-table` + React Router。

```
src/
├─ adapters/      core 的宿主实现：token-store.web.ts / audio.web.ts
├─ components/ui/ shadcn 拷进来的（按需加，别一次拷一片）
├─ components/app/ 本项目自己的组合件：app-shell / song-table / player-bar
├─ pages/         11 个页面，对着后端 33 条路由
└─ lib/utils.ts   shadcn 的 cn()
design/           原型图（一次性设计产物，不是应用代码）
                  **壳已定：暗色 + 安静侧边栏 + 顶栏搜索 + 悬浮毛玻璃播放条**
                  形态参考用户自建的「飞牛音乐」（fnOS），但视觉是自己的。
```

逻辑全部来自 `@music-robot/core`（workspace 包），**这里不放 core**。

## 三条不能破的

1. **业务逻辑写进 `packages/core/`，不要写在这里。** 这里只放 UI 与适配器。
2. `adapters/` 是 core 唯一碰宿主的地方（`token-store.web.ts` / `audio.web.ts`）。
3. `components/ui/` 是 shadcn 的，手改前想清楚 —— 以后要跟上游更新。

## 还没做的

没跑 `npm create vite`、没装任何依赖（装之前先问用户）。`design/` 里是原型图，不是应用代码。
