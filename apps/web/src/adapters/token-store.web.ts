import type { TokenStore } from '@music-robot/core';

/**
 * core 的 `TokenStore` 在 Web 上的实现。
 *
 * 放 `localStorage` 而不是 `sessionStorage`：音乐服务是自用的，关掉标签页还要重新登录太烦。
 * 代价是 XSS 能读到令牌 —— 这个项目的攻击面是「自己一个人用 + 内容不来自第三方」，
 * 接受了。（真要更严，得后端改成 httpOnly cookie，那是后端的事。）
 *
 * 注意：`localStorage` 可能抛异常（隐私模式、配额）。**绝不能因此让整页崩掉**，
 * 读不到就当没登录。
 */
const KEY = 'music-robot.token';

export function createWebTokenStore(): TokenStore {
  return {
    get() {
      try {
        return localStorage.getItem(KEY);
      } catch {
        return null;
      }
    },
    set(token: string) {
      try {
        localStorage.setItem(KEY, token);
      } catch {
        /* 存不下也不该阻断本次会话，刷新后重新登录即可 */
      }
    },
    clear() {
      try {
        localStorage.removeItem(KEY);
      } catch {
        /* 同上 */
      }
    },
  };
}
