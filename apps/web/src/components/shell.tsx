import { useEffect, useState, type ReactNode } from 'react';
import { Link, NavLink, useNavigate } from 'react-router';
import { MenuButton } from '@/components/menu.tsx';
import { api } from '@/lib/client.ts';
import { useSession } from '@/lib/session.tsx';

/**
 * 应用外壳：可收起侧边栏 + 常驻顶栏 + 内容区。
 *
 * 形态**对齐飞牛音乐**（2026-10-01 对着它的生产页面实测的尺寸）：
 *   · 侧边栏 **展开 229px / 收起 64px 图标导轨**，底部一颗「收起 / 展开」
 *   · 顶栏 **76px 常驻**：搜索胶囊 + 头像菜单（以前头像孤零零浮在右上角）
 *   · 窄屏（<900px）侧边栏整个消失，藏进左上角汉堡按钮的抽屉
 *
 * 静默的秘密（docs/design.md §7.5）：选中态只是一层 8% 白底的圆角块，
 * **不用粗体、不用左侧色条、不用大字号** —— 那些才是「后台感」的来源。
 */

const NAV: { to: string; label: string; d: string }[] = [
  { to: '/', label: '首页', d: 'M2.5 7 8 2.5 13.5 7v6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z' },
  // 音乐库 = 层叠图标。**以前是两根竖条**（`M3 3.5h3.2v9H3zM9.8 3.5H13v9H9.8z`）——
  // 跟暂停键长得一模一样，用户 2026-10-02 反馈认错。
  { to: '/library', label: '音乐库', d: 'M8 2.4 14.2 5.8 8 9.2 1.8 5.8z M2.6 9.6 8 12.5l5.4-2.9' },
  { to: '/albums', label: '专辑', d: 'M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM8 6.4a1.6 1.6 0 1 0 0 3.2 1.6 1.6 0 0 0 0-3.2z' },
  { to: '/artists', label: '歌手', d: 'M8 2.8a2.6 2.6 0 1 0 0 5.2 2.6 2.6 0 0 0 0-5.2zM3.2 13.4a4.9 4.9 0 0 1 9.6 0' },
  { to: '/playlists', label: '歌单', d: 'M2 4h12M2 8h12M2 12h7' },
  { to: '/favorites', label: '收藏', d: 'M8 13.5S2.5 10.2 2.5 6.4A2.9 2.9 0 0 1 8 5a2.9 2.9 0 0 1 5.5 1.4c0 3.8-5.5 7.1-5.5 7.1z' },
];

/** 侧边栏收起与否的持久化键。**UI 偏好，不进 core**（core 不碰 localStorage）。 */
const RAIL_KEY = 'music-robot.sidebar.rail';

function readRail(): boolean {
  try {
    return localStorage.getItem(RAIL_KEY) === 'rail';
  } catch {
    // 隐私模式 / 禁用存储 —— 读不到就按展开态，不影响使用
    return false;
  }
}

/** 导航图标。尺寸走 `.ico-*`（值来自 `--icon-*`），不写死像素。 */
function Icon({ d, size = 'sm' }: { d: string; size?: 'xs' | 'sm' | 'md' | 'lg' }) {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={`ico-${size} shrink-0`}
    >
      <path d={d} />
    </svg>
  );
}

/**
 * 侧边栏本体。桌面常驻（`rail` 控制图标导轨），窄屏放在抽屉里 ——
 * **同一份，不抄两遍**（抽屉里永远传 `rail={false}`，图标导轨在窄屏没意义）。
 */
function SideNav({ rail, onNavigate }: { rail: boolean; onNavigate?: () => void }) {
  return (
    <>
      <Link
        to="/"
        onClick={onNavigate}
        title="music-robot"
        className={[
          'flex items-center pt-1 pb-4',
          rail ? 'justify-center' : 'gap-2 px-2',
        ].join(' ')}
      >
        <span className="flex size-[26px] shrink-0 items-center justify-center rounded-md bg-accent text-white">
          <svg className="ico-sm" viewBox="0 0 16 16" fill="currentColor">
            <path d="M6 12.5a2 2 0 1 1-1.5-1.94V4.2l7-1.6v7.4a2 2 0 1 1-1.5-1.94V5.1L6 6.1z" />
          </svg>
        </span>
        {!rail && <b className="text-sm font-semibold tracking-[.1px]">music-robot</b>}
      </Link>

      <nav className="flex flex-col gap-px">
        {NAV.map((item) => (
          <NavLink
            key={item.to}
            to={item.to}
            end={item.to === '/'}
            title={rail ? item.label : undefined}
            onClick={onNavigate}
            className={({ isActive }) =>
              [
                'flex items-center rounded-md py-2 text-nav transition-colors',
                rail ? 'justify-center' : 'gap-3 px-3',
                isActive ? 'bg-surface text-ink' : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
              ].join(' ')
            }
          >
            <Icon d={item.d} />
            {!rail && <span>{item.label}</span>}
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
          <div className="text-nav text-ink">{user?.username ?? '未登录'}</div>
          <div className="text-cap text-ink-4">{user?.role === 'admin' ? '管理员' : '普通用户'}</div>
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
      <span className="flex size-7 items-center justify-center rounded-full bg-surface text-note font-medium text-ink-2">
        {initial}
      </span>
    </MenuButton>
  );
}

/**
 * 顶栏的搜索胶囊。回车跳到 `/search?q=`。
 *
 * 以前搜索框在「音乐库」页自己的 header 里 —— 只有那一页能搜。飞牛把它提到了
 * 全局顶栏（每页都在），这里照做：**搜的是整个曲库，不是当前列表的本地过滤**。
 */
function SearchBox() {
  const navigate = useNavigate();
  const [q, setQ] = useState('');

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        const v = q.trim();
        if (v) navigate(`/search?q=${encodeURIComponent(v)}`);
      }}
      className="flex h-9 w-full max-w-[460px] items-center gap-2 rounded-full bg-surface px-4 text-nav text-ink-4 transition-colors focus-within:bg-surface-hover"
    >
      <svg className="ico-sm" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round">
        <circle cx="7" cy="7" r="4.6" />
        <path d="m10.6 10.6 3 3" />
      </svg>
      <input
        value={q}
        onChange={(e) => setQ(e.target.value)}
        placeholder="搜索歌曲 / 歌手 / 专辑"
        className="min-w-0 flex-1 border-0 bg-transparent text-ink outline-0 placeholder:text-ink-4"
      />
    </form>
  );
}

/** 常驻顶栏。窄屏左边多一颗汉堡（侧边栏收进抽屉）。 */
function TopBar({ onMenu }: { onMenu: () => void }) {
  return (
    <header className="flex h-[76px] shrink-0 items-center gap-3 px-5 max-[1024px]:px-4 max-[900px]:h-[60px] max-[900px]:gap-2 max-[900px]:px-3">
      <button
        type="button"
        title="菜单"
        aria-label="打开菜单"
        onClick={onMenu}
        className="flex size-9 shrink-0 items-center justify-center rounded-full text-ink-2 transition-colors hover:bg-surface-hover hover:text-ink min-[901px]:hidden"
      >
        <Icon d="M2 4h12M2 8h12M2 12h12" size="md" />
      </button>

      <SearchBox />
      <span className="flex-1" />
      <UserMenu />
    </header>
  );
}

export function Shell({ children }: { children: ReactNode }) {
  // 窄屏抽屉。桌面侧边栏是常驻的，这个状态只在 <900px 有意义。
  const [drawer, setDrawer] = useState(false);
  // 桌面侧边栏是否收成图标导轨（持久化，见 RAIL_KEY）
  const [rail, setRail] = useState(readRail);

  useEffect(() => {
    try {
      localStorage.setItem(RAIL_KEY, rail ? 'rail' : 'full');
    } catch {
      /* 存不了就存不了，下次回来按展开态 */
    }
  }, [rail]);

  // 抽屉开着时按 Esc 关掉
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
      {/* 桌面侧边栏：<900px 整个不渲染（不是缩窄）。宽度在展开 / 图标导轨之间切 */}
      <aside
        className={[
          'hidden shrink-0 flex-col border-r border-line-weak bg-black/15 p-[14px_10px_10px] transition-[width] duration-200 min-[901px]:flex',
          rail ? 'w-[64px]' : 'w-[229px]',
        ].join(' ')}
      >
        <SideNav rail={rail} />
        <span className="flex-1" />
        <button
          type="button"
          onClick={() => setRail((v) => !v)}
          title={rail ? '展开' : '收起'}
          aria-label={rail ? '展开侧边栏' : '收起侧边栏'}
          className={[
            'flex items-center rounded-md py-2 text-nav text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink',
            rail ? 'justify-center' : 'gap-3 px-3',
          ].join(' ')}
        >
          <Icon d={rail ? 'M6 3.5 11 8l-5 4.5' : 'M10 3.5 5 8l5 4.5'} />
          {!rail && <span>收起</span>}
        </button>
      </aside>

      {/* 窄屏抽屉：盖在内容上，点遮罩 / 点导航 / 按 Esc 都关 */}
      {drawer && (
        <div className="fixed inset-0 z-50 min-[901px]:hidden">
          <div className="absolute inset-0 bg-black/60" onClick={() => setDrawer(false)} />
          <aside className="absolute inset-y-0 left-0 flex w-[240px] flex-col border-r border-line bg-[#141218] p-[14px_10px_10px] shadow-[0_0_40px_#000a]">
            <SideNav rail={false} onNavigate={() => setDrawer(false)} />
          </aside>
        </div>
      )}

      <main className="relative flex min-w-0 flex-1 flex-col">
        <TopBar onMenu={() => setDrawer(true)} />
        {children}
      </main>
    </div>
  );
}

export { Icon };
