# @music-robot/core —— 两端共用的逻辑

**唯一的铁律：这里不出现宿主 API。**

| 禁止出现 | 因为 |
|---|---|
| `react` / `react-dom` | UI 两端各写各的 |
| `window` / `document` / `localStorage` | Web 有，RN 没有 |
| `<audio>` / `Audio` / `HTMLMediaElement` | Web 用 `expo-audio`，不是一回事 |
| `import.meta.env` | Vite 专有；配置由宿主注入 |

判据一句话：**这段代码在 Node 里能跑吗？** 能跑就放这儿，不能就留在 `apps/<app>/src/`。

碰宿主的部分一律走 `apps/<app>/src/adapters/`（`token-store`、`audio` 各实现一份，
如 `apps/web/src/adapters/token-store.web.ts`）。这样将来加移动端时，`core` **一行逻辑都不用改**。

---

## 零依赖，零构建

- **不建 `dist`**：直接导出 TS 源码，Vite 和 Metro 都能编。
- **没有任何运行时依赖**，`package.json` 里 `dependencies` 是空的。不要为了一件小事往里加包。
- **测试不需要测试框架**：用 Node 内置 runner + Node 原生类型擦除（Node ≥ 22.6，本机 24）。

```sh
cd packages/core
node --test 'src/**/*.test.ts'     # 或者 npm test
```

## 目录

```
src/
├─ index.ts          barrel（对外只从这里进）
├─ http.ts           fetch 封装：JWT 头、401 回调、错误归一成 ApiError
├─ token-store.ts    interface TokenStore + 内存实现（宿主注入 localStorage / SecureStore）
├─ types.ts          REST 契约类型 —— **字段是从后端 json! 逐个抄的，别照感觉补**
├─ player-queue.ts   播放队列状态机（顺序/随机/单曲循环/列表循环）
└─ api/index.ts      各端点的薄封装
```

`api/index.ts` **故意是一个文件**：它们几乎是机械转写（路径 + 类型），按路由组拆成
16 个文件只会多 16 次跳转。等某一组长出真实逻辑（分页游标、重试、缓存）再拆出去。
里面**只实现当前页面用得到的**端点，剩下的等页面来了再加。

## 两个值得知道的实现细节

1. **播放队列是纯函数状态机**，`rand` 可注入 → 测试确定性可复现。
   切到随机时**当前这首歌必须留在原地**（天真实现会把它换掉，体感就是 bug），
   有专门的用例钉住，且做过变异验证。
2. **401 只回调一次、不重试** —— 后端目前**没有 refresh 接口**（只有 login/register/me），
   重试没有意义。移动端开工前要么调大 `token_expiry_hours`，要么加 refresh。
