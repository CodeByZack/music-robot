/**
 * 令牌存储 —— **core 里唯一需要宿主实现的接口**。
 *
 * 为什么不能直接用 `localStorage`：RN 上没有它。Web 用 `localStorage` /
 * `sessionStorage`，将来 mobile 用 `expo-secure-store`，各写一份放在
 * `apps/<app>/src/adapters/`（如 `apps/web/src/adapters/`），core 只依赖这个接口
 * —— 这样加移动端时 core 一行不用改。
 */
export interface TokenStore {
  get(): string | null;
  set(token: string): void;
  clear(): void;
}

/**
 * 内存实现。给测试、SSR、以及「不想持久化」的场景用。
 * Web 的 `localStorage` 实现放 `apps/web/src/adapters/token-store.web.ts`。
 */
export function createMemoryTokenStore(initial: string | null = null): TokenStore {
  let token = initial;
  return {
    get: () => token,
    set: (next) => {
      token = next;
    },
    clear: () => {
      token = null;
    },
  };
}
