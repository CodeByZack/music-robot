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
 * **现在就是原样返回** —— 这里曾经有一段「带令牌 fetch 成 blob 再喂给标签」的绕法，
 * 因为 `/api/stream` 与 `/api/songs/{id}/cover` 当初在 `require_auth` 后面，
 * 而 `<audio src>` / `<img src>` 发的是浏览器自发的裸 GET，带不了 `Authorization` 头。
 *
 * 后端已改为：媒体端点走独立中间件 `require_auth_media`，除了请求头还接受登录时
 * 下发的 **HttpOnly cookie**（浏览器自动携带）。于是绕法可以删掉，
 * 恢复了真正的流式 Range 与 seek —— 不再「整个文件下完才播」。
 *
 * 保留这一层（而不是直接拼 URL）是因为它是宿主接缝：
 * 将来 mobile 端在这里换成别的东西时，上层不用动。
 */
export async function resolveMediaUrl(path: string): Promise<string> {
  return path;
}
