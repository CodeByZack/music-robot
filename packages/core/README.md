# @music-robot/core —— 两端共用的逻辑

**唯一的铁律：这里不出现宿主 API。**

| 禁止出现 | 因为 |
|---|---|
| `react` / `react-dom` | UI 两端各写各的 |
| `window` / `document` / `localStorage` | Web 有，RN 没有 |
| `<audio>` / `Audio` / `HTMLMediaElement` | Web 用 `expo-audio`，不是一回事 |
| `import.meta.env` | Vite 专有；配置由宿主注入 |

判据一句话：**这段代码在 Node 里能跑吗？** 能跑就放这儿，不能就留在 `apps/*/src/`。

碰宿主的部分一律走 `apps/*/src/adapters/`（`token-store`、`audio` 各实现一份）。
这样将来加移动端时，`core` **一行逻辑都不用改**。

## 里面放什么

```
src/
├─ http.ts           fetch 封装：JWT 头、401 处理、错误归一
├─ token-store.ts    interface TokenStore（宿主注入实现）
├─ types.ts          33 条路由的请求/响应类型，集中一处
├─ player-queue.ts   播放队列状态机（顺序/随机/循环/断点续播）—— 不含播放器
└─ api/              每个路由组一个文件，纯函数
```

导出 **TS 源码，不建 dist**：Vite 和 Metro 都能直接编，省一层构建和 watch。
