import { useEffect, useState, type ReactNode } from 'react';
import { messageOf } from '@/lib/session.tsx';

export interface AsyncState<T> {
  data: T | null;
  error: string | null;
  loading: boolean;
  /** 手动重取（比如写完设置后刷新）。 */
  reload: () => void;
}

/**
 * 一次性取数 + 错误归一。
 *
 * 抽出来是因为**每个页面都要这套**（loading / error / 中文文案 / 卸载后不 setState），
 * 抄五遍迟早有一处忘记处理卸载。
 *
 * `deps` 变化会重取；`fn` 请在调用处用 useCallback 固定，否则会无限循环。
 */
export function useAsync<T>(fn: () => Promise<T>, deps: unknown[] = []): AsyncState<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [nonce, setNonce] = useState(0);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError(null);
    fn()
      .then((v) => {
        if (alive) setData(v);
      })
      .catch((e) => {
        if (alive) setError(messageOf(e));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
    // fn 由调用处 useCallback 固定；deps 是它真正的依赖
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, nonce]);

  return { data, error, loading, reload: () => setNonce((n) => n + 1) };
}

/** 页面通用的「读了但没读成」提示。 */
export function ErrorNote({ message, hint }: { message: string; hint?: ReactNode }) {
  return (
    <div className="rounded-lg bg-surface p-5 text-[13px] text-ink-2">
      {message}
      {hint && <div className="mt-2 text-xs text-ink-4">{hint}</div>}
    </div>
  );
}

/** 正在读。别用 spinner —— 一行字够了，也不会因为抽搐让人分心。 */
export function LoadingNote() {
  return <p className="py-6 text-[13px] text-ink-4">读取中…</p>;
}
