import { useCallback, useState } from 'react';
import type { User } from '@music-robot/core';
import { OverlayShell, type OverlaySection } from '@/components/overlay.tsx';
import { Panel, PanelRow } from '@/components/panel.tsx';
import { api } from '@/lib/client.ts';
import { messageOf, useSession } from '@/lib/session.tsx';
import { useAsync } from '@/lib/use-async.tsx';

/**
 * 用户管理页（全屏浮层页面，**骨架与设置页一致**：顶栏 + 左侧分节 + 右侧 Panel）。
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
 *
 * 分节：三个按角色筛的列表 + 一个建号表单。**建号不做成弹窗** —— 它是一整块带说明的
 * 表单，正是设置页「一块面板里几行」的形态；弹窗只用在「填完就走」的小输入上
 * （比如点歌请求那个，见 `request-dialog.tsx`）。
 */
const SECTIONS: OverlaySection[] = [
  { id: 'all', label: '全部用户' },
  { id: 'admin', label: '管理员' },
  { id: 'user', label: '普通用户' },
  { id: 'new', label: '新建用户' },
];

export default function UsersPage() {
  const { user: me } = useSession();
  const load = useCallback(() => api.users.list(), []);
  const { data, error, loading, reload } = useAsync(load, []);

  const [section, setSection] = useState<string>('all');
  const items = data?.items ?? [];

  // 角色筛选在**前端**做：用户表本来就小（一台自托管服务几个人），
  // 为它加一个后端 query 参数不划算。
  const shown = section === 'all' ? items : items.filter((u) => u.role === section);
  const current = SECTIONS.find((s) => s.id === section) ?? SECTIONS[0]!;

  return (
    <OverlayShell
      title="用户管理"
      sections={SECTIONS}
      section={section}
      onSection={setSection}
    >
      {section === 'new' ? (
        <CreateUserForm
          onDone={() => {
            reload();
            // 建完切回列表 —— 用户要看到「刚建的那个人出现了」
            setSection('all');
          }}
        />
      ) : (
        <Panel title={current.label}>
          {error ? (
            /* 普通用户直接敲地址进来会看到这个。把话说清楚，别只说「读取失败」。 */
            <p className="py-8 text-center text-nav text-ink-3">{error}</p>
          ) : loading ? (
            <p className="py-8 text-center text-nav text-ink-4">读取中…</p>
          ) : shown.length === 0 ? (
            <p className="py-8 text-center text-nav text-ink-4">
              {section === 'all' ? '还没有用户。' : '这一节里没有用户。'}
            </p>
          ) : (
            shown.map((u) => <UserRow key={u.id} user={u} isMe={u.id === me?.id} />)
          )}
        </Panel>
      )}
    </OverlayShell>
  );
}

/**
 * 一个用户 = 设置页的一行（**左边用户名 + 上次登录，右边角色**）。
 *
 * 刻意**不做头像**：设置页「账号」那一块就是「用户名 + 角色 + 右侧按钮」，
 * 加了头像反而不像同一套界面。首字头像留着给顶栏那颗按钮（那儿没有文字）。
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
 * 建号表单 —— **就是一块 Panel 加几行**，不另做表单样式。
 *
 * 校验只做**前端该做的那一份**（非空、口令长度），真正的规则在后端
 * （`normalize_username` / `validate_password`）—— 前端这层是为了少一次往返，
 * 不是安全边界。用户名重复由后端回 409，文案直接用它给的。
 */
function CreateUserForm({ onDone }: { onDone: () => void }) {
  const [name, setName] = useState('');
  const [pw, setPw] = useState('');
  const [role, setRole] = useState<'user' | 'admin'>('user');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [ok, setOk] = useState<string | null>(null);

  const canSubmit = name.trim().length > 0 && pw.length >= 8;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!canSubmit || busy) return;
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
      onDone();
    } catch (e2) {
      setErr(messageOf(e2));
    } finally {
      setBusy(false);
    }
  }

  const INPUT =
    'h-9 w-[220px] shrink-0 rounded-lg border-0 bg-surface-hover px-3 text-nav text-ink outline-0 placeholder:text-ink-4 max-[560px]:w-[140px]';

  return (
    <form onSubmit={submit}>
      <Panel title="新建用户">
        <PanelRow label="用户名" hint="最多 64 个字符，登录名区分大小写">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="比如 zhangsan"
            autoComplete="off"
            className={INPUT}
          />
        </PanelRow>
        <PanelRow label="口令" hint="至少 8 位。后端用 argon2 存哈希，不会明文落库">
          <input
            type="password"
            value={pw}
            onChange={(e) => setPw(e.target.value)}
            placeholder="至少 8 位"
            autoComplete="new-password"
            className={INPUT}
          />
        </PanelRow>
        <PanelRow
          label="角色"
          hint="管理员能改标签、跑扫描刮削、管理用户与点歌请求；普通用户只能用曲库与播放"
        >
          <div className="flex shrink-0 rounded-full bg-surface-hover p-0.5">
            {(['user', 'admin'] as const).map((r) => (
              <button
                key={r}
                type="button"
                onClick={() => setRole(r)}
                className={[
                  'rounded-full px-3.5 py-1.5 text-cap transition-colors',
                  role === r ? 'bg-surface-press text-ink' : 'text-ink-3 hover:text-ink',
                ].join(' ')}
              >
                {r === 'user' ? '普通用户' : '管理员'}
              </button>
            ))}
          </div>
        </PanelRow>

        {err && <p className="mt-3 text-note text-accent">{err}</p>}
        {ok && <p className="mt-3 text-note text-ink-3">{ok}</p>}

        <div className="mt-4">
          <button
            type="submit"
            disabled={!canSubmit || busy}
            className="h-[34px] rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {busy ? '创建中…' : '创建用户'}
          </button>
        </div>
      </Panel>
    </form>
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
