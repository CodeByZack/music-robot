import { useCallback, useState } from 'react';
import { Dialog } from '@/components/dialog.tsx';
import { OverlayShell } from '@/components/overlay.tsx';
import { api } from '@/lib/client.ts';
import { messageOf, useSession } from '@/lib/session.tsx';
import { useAsync } from '@/lib/use-async.tsx';

/**
 * 用户管理页（全屏浮层页面，**和设置一样**，仅 admin 可达）。
 *
 * 入口是顶栏右上角的下拉菜单。路由本身不拦权限 —— 后端那两条接口都挂 `AdminUser`
 * 提取器，普通用户进来只会看到一个 403 错误，不是数据泄漏。前端在菜单里
 * **不对普通用户显示这一项**（点进去再收 403 是更差的体验）。
 *
 * 为什么必须有「列表」这一半：`POST /api/admin/users` 是注册关闭后唯一的建号入口，
 * 但以前只有建号没有列表 —— 建完就查不到「现在有谁」，管理界面等于一张孤零零的表单。
 *
 * **不做**的：改密码、删号。前者后端没有接口；后者会牵动 playlists / favorites /
 * history / song_requests 一串 CASCADE，没有需求就不动。
 */
export default function UsersPage() {
  const { user: me } = useSession();
  const load = useCallback(() => api.users.list(), []);
  const { data, error, loading, reload } = useAsync(load, []);

  const [creating, setCreating] = useState(false);
  const items = data?.items ?? [];

  return (
    <OverlayShell
      title="用户管理"
      subtitle={data ? <span className="text-cap text-ink-4">{data.total} 个</span> : undefined}
      actions={
        <button
          type="button"
          onClick={() => setCreating(true)}
          className="h-8 rounded-full bg-accent px-3.5 text-cap font-medium text-white transition-colors hover:brightness-110"
        >
          新建用户
        </button>
      }
    >
      {error ? (
        /* 普通用户直接敲地址进来会看到这个。把话说清楚，别只说「读取失败」。 */
        <p className="py-10 text-center text-nav text-ink-3">{error}</p>
      ) : loading ? (
        <p className="py-10 text-center text-nav text-ink-4">读取中…</p>
      ) : (
        <div className="flex flex-col gap-2">
          {items.map((u) => (
            <div key={u.id} className="flex items-center gap-3.5 rounded-xl bg-surface px-4 py-3">
              <span className="flex size-9 shrink-0 items-center justify-center rounded-full bg-surface-hover text-note text-ink-2">
                {u.username.trim().charAt(0).toUpperCase()}
              </span>
              <span className="min-w-0 flex-1">
                <span className="flex items-baseline gap-2">
                  <b className="truncate text-nav font-normal text-ink">{u.username}</b>
                  {u.id === me?.id && <span className="text-micro text-ink-4">你</span>}
                </span>
                <span className="block text-cap text-ink-4">
                  {u.last_login ? `上次登录 ${fmtTime(u.last_login)}` : '从未登录'}
                </span>
              </span>
              <span
                className={[
                  'shrink-0 rounded-full px-2.5 py-1 text-micro',
                  u.role === 'admin' ? 'bg-accent-soft text-accent' : 'bg-surface-hover text-ink-3',
                ].join(' ')}
              >
                {u.role === 'admin' ? '管理员' : '普通用户'}
              </span>
            </div>
          ))}
        </div>
      )}

      {creating && (
        <CreateUserDialog
          onClose={() => setCreating(false)}
          onDone={() => {
            setCreating(false);
            reload();
          }}
        />
      )}
    </OverlayShell>
  );
}

/**
 * 建号弹窗。
 *
 * 校验只做**前端该做的那一份**（非空、口令长度），真正的规则在后端
 * （`normalize_username` / `validate_password`）—— 前端这层是为了少一次往返，
 * 不是安全边界。用户名重复由后端回 409，文案直接用它给的。
 */
function CreateUserDialog({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState('');
  const [pw, setPw] = useState('');
  const [role, setRole] = useState<'user' | 'admin'>('user');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const ok = name.trim().length > 0 && pw.length >= 8;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!ok || busy) return;
    setBusy(true);
    setErr(null);
    try {
      await api.users.create(name.trim(), pw, role);
      onDone();
    } catch (e2) {
      setErr(messageOf(e2));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog title="新建用户" onClose={onClose}>
      <form onSubmit={submit} className="p-4">
        <div className="flex flex-col gap-2">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="用户名"
            autoComplete="off"
            className="h-9 w-full rounded-lg border-0 bg-surface px-3 text-nav text-ink outline-0 placeholder:text-ink-4"
          />
          <input
            type="password"
            value={pw}
            onChange={(e) => setPw(e.target.value)}
            placeholder="口令（至少 8 位）"
            autoComplete="new-password"
            className="h-9 w-full rounded-lg border-0 bg-surface px-3 text-nav text-ink outline-0 placeholder:text-ink-4"
          />
          <div className="flex rounded-full bg-surface p-0.5">
            {(['user', 'admin'] as const).map((r) => (
              <button
                key={r}
                type="button"
                onClick={() => setRole(r)}
                className={[
                  'flex-1 rounded-full px-3 py-1.5 text-cap transition-colors',
                  role === r ? 'bg-surface-press text-ink' : 'text-ink-3 hover:text-ink',
                ].join(' ')}
              >
                {r === 'user' ? '普通用户' : '管理员'}
              </button>
            ))}
          </div>
        </div>
        <p className="mt-2.5 text-micro leading-4 text-ink-4">
          管理员能改标签、跑扫描刮削、管理用户与点歌请求；普通用户只能用曲库与播放。
        </p>
        {err && <p className="mt-2.5 text-cap text-accent">{err}</p>}
        <div className="mt-4 flex gap-2">
          <button
            type="submit"
            disabled={!ok || busy}
            className="h-9 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110 disabled:opacity-40"
          >
            {busy ? '创建中…' : '创建'}
          </button>
          <button
            type="button"
            onClick={onClose}
            className="h-9 rounded-full px-4 text-nav text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
          >
            取消
          </button>
        </div>
      </form>
    </Dialog>
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
