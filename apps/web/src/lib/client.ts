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
