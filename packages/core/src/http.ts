import type { TokenStore } from './token-store.ts';
import type { ApiErrorBody } from './types.ts';

/**
 * 统一的 API 错误。`status` 是 HTTP 码，`code` 是后端 `error.code`（拿不到时按状态码兜底）。
 *
 * 为什么要一个类而不是 `throw new Error(msg)`：UI 要按 `code` 分支
 * （401 → 跳登录、409 → 提示「已有任务在跑」、403 → 提示没权限），
 * 靠匹配中文文案迟早出错。
 */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly details: unknown;

  constructor(status: number, code: string, message: string, details?: unknown) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.details = details;
  }

  /** 令牌失效 —— UI 层据此清登录态并跳登录页。 */
  get isUnauthorized(): boolean {
    return this.status === 401;
  }
}

export interface HttpOptions {
  baseUrl: string;
  tokens: TokenStore;
  /** 注入点：测试塞假 fetch，RN 上也不用改。 */
  fetch?: typeof globalThis.fetch;
  /** 收到 401 时回调一次（清登录态 / 跳登录）。core 不碰路由。 */
  onUnauthorized?: () => void;
  /** 单次请求超时（毫秒）。默认 15s。 */
  timeoutMs?: number;
}

/** 有 body 时才算的响应；204 与空响应体返回 undefined。 */
export type Http = {
  request<T>(method: string, path: string, body?: unknown): Promise<T>;
  get<T>(path: string): Promise<T>;
  post<T>(path: string, body?: unknown): Promise<T>;
  put<T>(path: string, body?: unknown): Promise<T>;
  del<T>(path: string): Promise<T>;
};

/**
 * 建一个 API 客户端。
 *
 * 三个刻意的决定：
 * 1. **不带 baseUrl 的尾斜杠拼接坑**：两边都归一化，`/api` + `/songs` 不会拼成 `//`。
 * 2. **401 只回调一次、不重试**：后端没有 refresh 接口（见 types.ts 的说明），
 *    重试没有意义，直接让 UI 跳登录。
 * 3. **错误响应体解析失败不吞掉状态码**：后端返回 HTML（比如反代 502）时，
 *    仍然要给出一个带 status 的 ApiError，而不是 `SyntaxError`。
 */
export function createHttp(options: HttpOptions): Http {
  const base = options.baseUrl.replace(/\/+$/, '');
  const doFetch = options.fetch ?? globalThis.fetch;
  const timeoutMs = options.timeoutMs ?? 15_000;

  async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const url = base + (path.startsWith('/') ? path : `/${path}`);
    const headers: Record<string, string> = { Accept: 'application/json' };

    const token = options.tokens.get();
    if (token) headers.Authorization = `Bearer ${token}`;
    // 只有真有 body 才带 Content-Type：GET 带上去有些代理会不高兴
    const hasBody = body !== undefined;
    if (hasBody) headers['Content-Type'] = 'application/json';

    // AbortSignal.timeout 在 RN 上未必有；没有就不设超时，别为它引 polyfill
    const signal = typeof AbortSignal !== 'undefined' && 'timeout' in AbortSignal
      ? AbortSignal.timeout(timeoutMs)
      : undefined;

    const res = await doFetch(url, {
      method,
      headers,
      body: hasBody ? JSON.stringify(body) : undefined,
      ...(signal ? { signal } : {}),
    });

    if (res.status === 204) return undefined as T;

    const text = await res.text();
    let parsed: unknown;
    if (text) {
      try {
        parsed = JSON.parse(text);
      } catch {
        parsed = undefined;
      }
    }

    if (!res.ok) {
      const errBody = parsed as ApiErrorBody | undefined;
      const code = errBody?.error?.code ?? `HTTP_${res.status}`;
      const message = errBody?.error?.message ?? `请求失败（HTTP ${res.status}）`;
      if (res.status === 401) options.onUnauthorized?.();
      throw new ApiError(res.status, code, message, errBody?.error?.details);
    }

    return parsed as T;
  }

  return {
    request,
    get: (path) => request('GET', path),
    post: (path, body) => request('POST', path, body),
    put: (path, body) => request('PUT', path, body),
    del: (path) => request('DELETE', path),
  } as Http;
}
