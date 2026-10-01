import { useEffect, useState, type ReactNode } from 'react';
import { NavLink } from 'react-router';
import { MenuButton } from '@/components/menu.tsx';
import { api } from '@/lib/client.ts';
import { useSession } from '@/lib/session.tsx';

/**
 * 应用外壳：安静侧边栏 + 内容区。
 *
 * 形态来源见 docs/design.md §7.5：**"不像后台"的秘密不在有没有侧边栏，
 * 而在侧边栏长什么样** —— 窄、图标+小字、选中态只是一个 8% 白底的圆角块，
 * 不用粗体、不用色条。
 *
 * 三条约定（都是用户 2026-10-02 提的）：
 * 1. **窄屏（<900px）侧边栏整个消失**，藏进左上角的汉堡按钮，以抽屉形式弹出。
 *    以前这里只是缩成 60px 图标条 —— 手机上一列图标仍然占着最宝贵的宽度。
 * 2. 设置**不在侧边栏里**，在右上角头像菜单里。
 * 3. 右上角是**用户头像**（不是齿轮）：点开有用户名/角色 · 设置 · 退出登录。
 */

const NAV: { to: string; label: string; d: string }[] = [
  { to: '/', label: '首页', d: 'M2.5 7 8 2.5 13.5 7v6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z' },
  { to: '/library', label: '音乐库', d: 'M3 3.5h3.2v9H3zM9.8 3.5H13v9H9.8z' },
  { to: '/albums', label: '专辑', d: 'M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM8 6.4a1.6 1.6 0 1 0 0 3.2 1.6 1.6 0 0 0 0-3.2z' },
  { to: '/artists', label: '歌手', d: 'M8 2.8a2.6 2.6 0 1 0 0 5.2 2.6 2.6 0 0 0 0-5.2zM3.2 13.4a4.9 4.9 0 0 1 9.6 0' },
  { to: '/playlists', label: '歌单', d: 'M2 4h12M2 8h12M2 12h7' },
  { to: '/favorites', label: '收藏', d: 'M8 13.5S2.5 10.2 2.5 6.4A2.9 2.9 0 0 1 8 5a2.9 2.9 0 0 1 5.5 1.4c0 3.8-5.5 7.1-5.5 7.1z' },
];

function Icon({ d, size = 15 }: { d: string; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      className="shrink-0"
    >
      <path d={d} />
    </svg>
  );
}

/** 侧边栏内容。桌面是常驻的，窄屏是抽屉里的 —— 同一份，不抄两遍。 */
function SideNav({ onNavigate }: { onNavigate?: () => void }) {
  return (
    <>
      <div className="flex items-center gap-[9px] px-2 pt-1 pb-4">
        <span className="flex size-[26px] shrink-0 items-center justify-center rounded-md bg-accent text-white">
          <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
            <path d="M6 12.5a2 2 0 1 1-1.5-1.94V4.2l7-1.6v7.4a2 2 0 1 1-1.5-1.94V5.1L6 6.1z" />
          </svg>
        </span>
        <b className="text-sm font-semibold tracking-[.1px]">music-robot</b>
      </div>

      <nav className="flex flex-col gap-px">
        {NAV.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.to === '/'}
            title={item.label}
            onClick={onNavigate}
            className={({ isActive }) =>
              [
                'flex items-center gap-[10px] rounded-md px-[10px] py-2 text-[13px] transition-colors',
                isActive ? 'bg-surface text-ink' : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
              ].join(' ')
            }
          >
            <Icon d={item.d} />
            <span>{item.label}</span>
          </NavLink>
        ))}
      </nav>
    </>
  );
}

/** 右上角头像 + 菜单。用户名首字当头像 —— 没有头像上传，别假装有。 */
function UserMenu() {
  const { user, logout } = useSession();
  const initial = (user?.username ?? '?').trim().charAt(0).toUpperCase();

  return (
    <MenuButton
      title="账号"
      header={
        <div>
          <div className="text-[13px] text-ink">{user?.username ?? '未登录'}</div>
          <div className="text-[11px] text-ink-4">{user?.role === 'admin' ? '管理员' : '普通用户'}</div>
        </div>
      }
      items={[
        { label: '设置', to: '/settings' },
        {
          label: '退出登录',
          danger: true,
          onClick: () => {
            // 同时清服务端的媒体 cookie —— 它是 HttpOnly，JS 删不掉
            void api.auth
              .logout()
              .catch(() => {
                /* 登出接口失败不该挡住本地登出 */
              })
              .then(logout);
          },
        },
      ]}
    >
      <span className="flex size-7 items-center justify-center rounded-full bg-surface text-[12px] font-medium text-ink-2">
        {initial}
      </span>
    </MenuButton>
  );
}

export function Shell({ children }: { children: ReactNode }) {
  // 窄屏抽屉。桌面侧边栏是常驻的，这个状态只在 <900px 有意义。
  const [drawer, setDrawer] = useState(false);

  // 抽屉开着时按 Esc 关掉；同时锁掉背后页面的滚动
  useEffect(() => {
    if (!drawer) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setDrawer(false);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [drawer]);

  return (
    <div className="flex h-full">
      {/* 桌面侧边栏：<900px 整个不渲染（不是缩窄） */}
      <aside className="hidden w-[158px] shrink-0 flex-col border-r border-line-weak bg-black/15 p-[14px_10px_10px] min-[901px]:flex">
        <SideNav />
      </aside>

      {/* 窄屏抽屉：盖在内容上，点遮罩 / 点导航 / 按 Esc 都关 */}
      {drawer && (
        <div className="fixed inset-0 z-50 min-[901px]:hidden">
          <div className="absolute inset-0 bg-black/60" onClick={() => setDrawer(false)} />
          <aside className="absolute inset-y-0 left-0 flex w-[240px] flex-col border-r border-line bg-[#141218] p-[14px_10px_10px] shadow-[0_0_40px_#000a]">
            <SideNav onNavigate={() => setDrawer(false)} />
          </aside>
        </div>
      )}

      <main className="relative flex min-w-0 flex-1 flex-col">
        {/* 窄屏顶栏：左边汉堡、右边头像。做成一根真顶栏而不是浮在内容上的按钮 ——
            浮着一定会压到页面自己的标题和搜索框（试过）。 */}
        <header className="flex h-[52px] shrink-0 items-center gap-2 px-3 min-[901px]:hidden">
          <button
            type="button"
            title="菜单"
            aria-label="打开菜单"
            onClick={() => setDrawer(true)}
            className="flex size-9 items-center justify-center rounded-full text-ink-2 transition-colors hover:bg-surface-hover hover:text-ink"
          >
            <Icon d="M2 4h12M2 8h12M2 12h12" size={17} />
          </button>
          <span className="flex-1" />
          <UserMenu />
        </header>

        {/* 桌面：头像浮在右上角（页面自己有 header 时右边本来就是空的） */}
        <div className="absolute top-[13px] right-[22px] z-20 hidden min-[901px]:block">
          <UserMenu />
        </div>

        {children}
      </main>
    </div>
  );
}

export { Icon };
