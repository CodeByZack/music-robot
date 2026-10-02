import { useCallback, useState } from 'react';
import { api } from '@/lib/client.ts';
import { messageOf, useSession } from '@/lib/session.tsx';
import { useAsync } from '@/lib/use-async.tsx';

/**
 * 用户管理面板（**顶栏右上角 popover 里**，仅 admin 能打开）。
 *
 * 为什么必须有「列表」这一半：`POST /api/admin/users` 是注册关闭后唯一的建号入口，
 * 但以前只有建号没有列表 —— 建完就查不到「现在有谁」，管理界面等于一张孤零零的表单。
 * 后端因此补了 `GET /api/admin/users`（只读，且复用 `user_json`，绝不带 password_hash）。
 *
 * **不做**的：改密码、删号。前者后端没有接口；后者会牵动 playlists / favorites /
 * history / song_requests 一串 CASCADE，没有需求就不动。
 */
export function UsersPanel() {
  const { user: me } = useSession();
  const load = useCallback(() => api.users.list(), []);
  const { data, error, loading, reload } = useAsync(load, []);

  const [name, setName] = useState('');
  const [pw, setPw] = useState('');
  const [role, setRole] = useState<'user' | 'admin'>('user');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [ok, setOk] = useState<string | null>(null);

  async function create(e: React.FormEvent) {
    e.preventDefault();
    if (busy || !name.trim() || !pw) return;
    setBusy(true);
    setErr(null);
    setOk(null);
    try {
      const r = await api.users.create(name.trim(), pw, role);
      setOk(`已创建 ${r.user.username}（${r.user.role === 'admin' ? '管理员' : '普通用户'}）`);
      // 口令立刻从内存里抹掉 —— 建完还留在输入框里没有理由
      setName('');
      setPw('');
      setRole('user');
      reload();
    } catch (e2) {
      setErr(messageOf(e2));
    } finally {
      setBusy(false);
    }
  }

  const items = data?.items ?? [];

  return (
    <div className="flex max-h-[min(72vh,560px)] flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-line-weak px-3.5 py-2.5">
        <b className="text-nav font-medium">用户</b>
        <span className="flex-1" />
        <span className="text-cap text-ink-4">{data ? `共 ${data.total} 个` : ''}</span>
      </div>

      <div className="min-h-0 flex-1 overflow-auto p-1.5">
        {error ? (
          <p className="px-2 py-6 text-nav text-ink-3">{error}</p>
        ) : loading ? (
          <p className="px-2 py-6 text-nav text-ink-4">读取中…</p>
        ) : (
          items.map((u) => (
            <div
              key={u.id}
              className="flex items-center gap-2 rounded-lg px-2 py-2 transition-colors hover:bg-surface"
            >
              <span className="flex size-7 shrink-0 items-center justify-center rounded-full bg-surface text-cap text-ink-2">
                {u.username.trim().charAt(0).toUpperCase()}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate text-nav text-ink">
                  {u.username}
                  {u.id === me?.id && <span className="ml-1.5 text-micro text-ink-4">你</span>}
                </span>
                <span className="block text-micro text-ink-4">
                  {u.last_login ? `上次登录 ${fmtTime(u.last_login)}` : '从未登录'}
                </span>
              </span>
              <span
                className={[
                  'shrink-0 rounded-full px-2 py-0.5 text-micro',
                  u.role === 'admin' ? 'bg-accent-soft text-accent' : 'bg-surface text-ink-3',
                ].join(' ')}
              >
                {u.role === 'admin' ? '管理员' : '普通用户'}
              </span>
            </div>
          ))
        )}
      </div>

      <form onSubmit={create} className="shrink-0 border-t border-line-weak p-3">
        <div className="mb-2 text-micro tracking-[.24em] text-ink-4 uppercase">新建用户</div>
        <div className="flex flex-col gap-1.5">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="用户名"
            autoComplete="off"
            className="h-8 rounded-md border-0 bg-surface px-2.5 text-cap text-ink outline-0 placeholder:text-ink-4"
          />
          <input
            type="password"
            value={pw}
            onChange={(e) => setPw(e.target.value)}
            placeholder="口令（至少 8 位）"
            autoComplete="new-password"
            className="h-8 rounded-md border-0 bg-surface px-2.5 text-cap text-ink outline-0 placeholder:text-ink-4"
          />
          <div className="flex rounded-full bg-surface p-0.5">
            {(['user', 'admin'] as const).map((r) => (
              <button
                key={r}
                type="button"
                onClick={() => setRole(r)}
                className={[
                  'flex-1 rounded-full px-2.5 py-1 text-cap transition-colors',
                  role === r ? 'bg-surface-press text-ink' : 'text-ink-3 hover:text-ink',
                ].join(' ')}
              >
                {r === 'user' ? '普通用户' : '管理员'}
              </button>
            ))}
          </div>
        </div>
        {err && <p className="mt-2 text-cap text-accent">{err}</p>}
        {ok && <p className="mt-2 text-cap text-ink-3">{ok}</p>}
        <button
          type="submit"
          disabled={busy || !name.trim() || !pw}
          className="mt-2 h-8 w-full rounded-full bg-accent text-cap font-medium text-white transition-colors hover:brightness-110 disabled:opacity-40"
        >
          {busy ? '创建中…' : '创建用户'}
        </button>
      </form>
    </div>
  );
}

/**
 * 毫秒时间戳 → `2026-10-02 14:33`。
 *
 * 后端所有时间戳都是**毫秒**（`crate::db::now_unix_ms()`，见 `repos/history.rs`），
 * 不是秒 —— 少乘 1000 会得到 1970 年。
 */
function fmtTime(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
