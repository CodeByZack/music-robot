import { createApi, createHttp, type Api, type User } from '@music-robot/core';
import { createWebTokenStore } from '@/adapters/token-store.web.ts';

/**
 * 全应用唯一的 API 客户端。
 *
 * 开发时 `baseUrl` 留空 —— 走 Vite 的 `/api` 代理（见 vite.config.ts），
 * 这样浏览器眼里是同源，不需要后端配 CORS。
 */
export const tokens = createWebTokenStore();

/** 401 时谁来跳登录：由 App 在启动时注册（core 不碰路由）。 */
let onUnauthorized: (() => void) | null = null;
export function setUnauthorizedHandler(fn: (() => void) | null): void {
  onUnauthorized = fn;
}

export const http = createHttp({
  baseUrl: '',
  tokens,
  onUnauthorized: () => onUnauthorized?.(),
});

export const api: Api = createApi(http);

export type { User };

/**
 * 把后端媒体路径解析成 `<audio>` / `<img>` 能直接用的 URL。
 *
 * ⚠️ **这是一个绕法，根因在后端。** `/api/stream/{id}` 与 `/api/songs/{id}/cover`
 * 都在 `require_auth` 后面，但 `<audio src>` / `<img src>` 发的是**裸 GET，
 * 带不了 `Authorization` 头**（实测：裸 GET → 401，带令牌 → 200）。
 *
 * 所以先带令牌 fetch 成 blob，再把 blob URL 交给标签。代价：
 * - **整个文件下完才开始播**，没有真正的流式 Range（LAN 上 9MB ≈ 0.2s，40MB FLAC ≈ 1s）
 * - blob 占内存
 *
 * 正确的修法是后端给**媒体类端点**（stream / cover）另开一条鉴权通道：
 * ① 登录时下发一个 `HttpOnly; SameSite=Lax; Path=/api` 的 cookie，媒体端点认 cookie；
 * ② 或者发短时效的签名 URL（`?t=<ticket>`）。
 * 两条都比把 JWT 放进查询串好 —— 查询串会进 nginx 与反代的访问日志。
 *
 * 后端改完，**把这里换成 `async (p) => p` 即可**，其余代码一行不用动。
 */
let lastObjectUrl: string | null = null;

export async function resolveMediaUrl(path: string): Promise<string> {
  const res = await fetch(path, {
    headers: tokens.get() ? { Authorization: `Bearer ${tokens.get()}` } : {},
  });
  if (!res.ok) throw new Error(`媒体请求失败：HTTP ${res.status}`);

  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  // 一次只播一首，所以只保留最后一个 blob URL；旧的立刻回收，否则每换一首漏一份内存
  if (lastObjectUrl) URL.revokeObjectURL(lastObjectUrl);
  lastObjectUrl = url;
  return url;
}
