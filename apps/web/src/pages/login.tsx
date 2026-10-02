import { useState } from 'react';
import { messageOf, useSession } from '@/lib/session.tsx';

/**
 * 登录页。形态照 fnOS 那版（不对称构图 + 宽字距全大写小标签 + 白底主按钮），
 * 理由见 docs/design.md §7.1 与 §7.2。
 */
export default function LoginPage() {
  const { login } = useSession();
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      await login(username.trim(), password);
    } catch (err) {
      setError(messageOf(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="fixed inset-0 z-50 flex">
      {/* 角落的宽字距小标签：成本几乎为零，但立刻显得"有人设计过" */}
      <span className="absolute top-[38px] left-[40px] text-micro tracking-[.28em] text-ink-4 uppercase max-[640px]:hidden">
        Personal Music Archive
      </span>
      <span className="absolute bottom-[38px] left-[40px] text-micro tracking-[.28em] text-ink-4 uppercase max-[640px]:hidden">
        Tracks // Albums // Artists
      </span>
      <span className="absolute top-1/2 right-[34px] -translate-y-1/2 text-micro tracking-[.3em] text-ink-4 uppercase [writing-mode:vertical-rl] max-[640px]:hidden">
        music-robot
      </span>

      <form
        onSubmit={submit}
        className="mr-[14%] ml-auto w-[340px] self-center max-[900px]:mx-auto max-[640px]:w-[calc(100%-40px)]"
      >
        <div className="mb-[34px] flex items-center gap-[13px]">
          <span className="flex size-[52px] items-center justify-center rounded-[14px] bg-accent text-white shadow-[0_1px_4px_#0006]">
            <svg className="ico-xl" viewBox="0 0 16 16" fill="currentColor">
              <path d="M6 12.5a2 2 0 1 1-1.5-1.94V4.2l7-1.6v7.4a2 2 0 1 1-1.5-1.94V5.1L6 6.1z" />
            </svg>
          </span>
          <span>
            <b className="block text-brand leading-8 font-semibold">music-robot</b>
            <span className="text-xs text-ink-3">自建音乐服务器</span>
          </span>
        </div>

        <label className="mb-4 block">
          <span className="mb-[7px] block text-xs text-ink-3">用户名</span>
          <input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoComplete="username"
            placeholder="请输入用户名"
            className="h-11 w-full rounded-lg border-0 bg-surface px-[14px] text-ink outline-0 transition-shadow placeholder:text-ink-4 focus:shadow-[0_0_0_3px_color-mix(in_srgb,var(--color-accent)_34%,transparent)]"
          />
        </label>

        <label className="mb-4 block">
          <span className="mb-[7px] block text-xs text-ink-3">密码</span>
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="current-password"
            placeholder="请输入密码"
            className="h-11 w-full rounded-lg border-0 bg-surface px-[14px] text-ink outline-0 transition-shadow placeholder:text-ink-4 focus:shadow-[0_0_0_3px_color-mix(in_srgb,var(--color-accent)_34%,transparent)]"
          />
        </label>

        {error && (
          <p className="mb-4 rounded-md bg-accent-soft px-[14px] py-3 text-note leading-[18px] text-accent">
            {error}
          </p>
        )}

        <div className="mt-1 mb-[22px] flex items-center justify-between text-xs text-ink-3">
          <label className="flex cursor-pointer items-center gap-[7px]">
            <input type="checkbox" className="accent-accent" />
            记住账号
          </label>
        </div>

        <button
          type="submit"
          disabled={busy || !username || !password}
          className="h-11 w-full rounded-lg bg-ink text-sm font-medium text-[#14121b] transition-opacity hover:opacity-[.88] disabled:cursor-not-allowed disabled:opacity-40"
        >
          {busy ? '登录中…' : '登录'}
        </button>
      </form>
    </div>
  );
}
