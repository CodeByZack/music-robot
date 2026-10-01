import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react';
import { ApiError, type User } from '@music-robot/core';
import { api, setUnauthorizedHandler, tokens } from '@/lib/client.ts';

interface SessionValue {
  user: User | null;
  /** 已经问过 `/auth/me` 了吗。没问完之前不要渲染登录页，否则会闪一下。 */
  ready: boolean;
  login: (username: string, password: string) => Promise<void>;
  logout: () => void;
}

const SessionContext = createContext<SessionValue | null>(null);

export function SessionProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(null);
  const [ready, setReady] = useState(false);

  const logout = useCallback(() => {
    tokens.clear();
    setUser(null);
  }, []);

  // core 拿到 401 时回调这里 —— 路由的事 core 不碰，由 Web 自己决定
  useEffect(() => {
    setUnauthorizedHandler(() => {
      tokens.clear();
      setUser(null);
    });
    return () => setUnauthorizedHandler(null);
  }, []);

  // 启动时用已有令牌换一次用户信息；失败就当没登录
  useEffect(() => {
    let alive = true;
    (async () => {
      if (!tokens.get()) {
        if (alive) setReady(true);
        return;
      }
      try {
        // ⚠️ /api/auth/me 返回的是 { user }，不是裸 User（照后端真形状改的）
        const me = await api.auth.me();
        if (alive) setUser(me.user);
      } catch {
        tokens.clear();
      } finally {
        if (alive) setReady(true);
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  const login = useCallback(async (username: string, password: string) => {
    const res = await api.auth.login(username, password);
    tokens.set(res.token);
    setUser(res.user);
  }, []);

  const value = useMemo(
    () => ({ user, ready, login, logout }),
    [user, ready, login, logout],
  );

  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>;
}

export function useSession(): SessionValue {
  const v = useContext(SessionContext);
  if (!v) throw new Error('useSession 必须在 SessionProvider 内使用');
  return v;
}

/** 把任意异常转成能给用户看的中文。ApiError 带后端的文案，直接用。 */
export function messageOf(e: unknown): string {
  if (e instanceof ApiError) return e.message;
  if (e instanceof TypeError) return '连不上服务 —— 后端起了吗？';
  if (e instanceof Error) return e.message;
  return '未知错误';
}
