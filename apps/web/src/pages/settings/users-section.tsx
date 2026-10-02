import { useCallback, useState } from 'react';
import type { User } from '@music-robot/core';
import { Dialog } from '@/components/dialog.tsx';
import { Panel, PanelRow } from '@/components/panel.tsx';
import { api } from '@/lib/client.ts';
import { messageOf, useSession } from '@/lib/session.tsx';
import { useAsync } from '@/lib/use-async.tsx';

/**
 * 设置 → 用户管理（**是设置页的一个分节，不是独立页面**，仅 admin 可见）。
 *
 * 用户 2026-10-02 要求并入设置页。分节本身对普通用户不显示（`settings.tsx` 的
 * `SECTIONS` 里标了 `adminOnly`）—— 后端那两条接口也各自挂了 `AdminUser` 提取器，
 * 所以就算直接改 URL 也拿不到数据，不是靠前端拦。
 *
 * 为什么必须有「列表」这一半：`POST /api/admin/users` 是注册关闭后唯一的建号入口，
 * 但以前只有建号没有列表 —— 建完就查不到「现在有谁」，管理界面等于一张孤零零的表单。
 *
 * **不做**的：改密码、删号。前者后端没有接口；后者会牵动 playlists / favorites /
 * history / song_requests 一串 CASCADE，没有需求就不动。
 */
export function UsersSection() {
  const { user: me } = useSession();
  const load = useCallback(() => api.users.list(), []);
  const { data, error, loading, reload } = useAsync(load, []);

  const items = data?.items ?? [];
  const [creating, setCreating] = useState(false);

  return (
    <>
      <Panel
        title="用户"
        actions={
          <button
            type="button"
            onClick={() => setCreating(true)}
            className="h-[34px] shrink-0 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110"
          >
            新建用户
          </button>
        }
      >
        {error ? (
          <p className="py-8 text-center text-nav text-ink-3">{error}</p>
        ) : loading ? (
          <p className="py-8 text-center text-nav text-ink-4">读取中…</p>
        ) : items.length === 0 ? (
          <p className="py-8 text-center text-nav text-ink-4">还没有用户。</p>
        ) : (
          items.map((u) => <UserRow key={u.id} user={u} isMe={u.id === me?.id} />)
        )}
      </Panel>

      {creating && (
        <CreateUserDialog
          onClose={() => setCreating(false)}
          onDone={() => {
            setCreating(false);
            reload();
          }}
        />
      )}
    </>
  );
}

/**
 * 一个用户 = 设置页的一行（**左边用户名 + 上次登录，右边角色**）。
 *
 * 刻意**不做头像**：设置页「账号」那一块就是「用户名 + 角色 + 右侧按钮」，
 * 加了头像反而不像同一套界面。
 */
function UserRow({ user, isMe }: { user: User; isMe: boolean }) {
  return (
    <PanelRow
      label={
        <>
          {user.username}
          {isMe && <span className="ml-2 text-micro text-ink-4">你</span>}
        </>
      }
      hint={user.last_login ? `上次登录 ${fmtTime(user.last_login)}` : '从未登录'}
    >
      <span
        className={[
          'shrink-0 rounded-full px-2.5 py-1 text-micro',
          user.role === 'admin' ? 'bg-accent-soft text-accent' : 'bg-surface-hover text-ink-3',
        ].join(' ')}
      >
        {user.role === 'admin' ? '管理员' : '普通用户'}
      </span>
    </PanelRow>
  );
}

/**
 * 建号弹窗 —— 与「点一首」用同一套弹窗外壳（`components/dialog.tsx`），
 * 站内的「填完就走」都用它（用户 2026-10-02 要求统一）。
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

  const canSubmit = name.trim().length > 0 && pw.length >= 8;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!canSubmit || busy) return;
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

  const INPUT =
    'h-9 w-full rounded-lg border-0 bg-surface px-3 text-nav text-ink outline-0 placeholder:text-ink-4';

  return (
    <Dialog title="新建用户" onClose={onClose}>
      <form onSubmit={submit} className="p-4">
        <div className="flex flex-col gap-2">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="用户名（最多 64 个字符）"
            autoComplete="off"
            className={INPUT}
          />
          <input
            type="password"
            value={pw}
            onChange={(e) => setPw(e.target.value)}
            placeholder="口令（至少 8 位）"
            autoComplete="new-password"
            className={INPUT}
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
        {/* 两种角色的差别写出来 —— 它是这张表里唯一需要解释的一项 */}
        <p className="mt-2.5 text-micro leading-4 text-ink-4">
          {role === 'admin'
            ? '管理员能改标签、跑扫描刮削、管理用户与点歌请求。'
            : '普通用户只能用曲库、播放与点歌。口令后端用 argon2 存哈希，不会明文落库。'}
        </p>
        {err && <p className="mt-2.5 text-cap text-accent">{err}</p>}
        <div className="mt-4 flex gap-2">
          <button
            type="submit"
            disabled={!canSubmit || busy}
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
