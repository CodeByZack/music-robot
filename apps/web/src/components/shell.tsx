import type { ReactNode } from 'react';
import { NavLink } from 'react-router';

/**
 * 应用外壳：安静侧边栏 + 顶栏 + 内容区。
 *
 * 形态来源见 docs/design.md §7.5：**"不像后台"的秘密不在有没有侧边栏，
 * 而在侧边栏长什么样** —— 窄、图标+小字、选中态只是一个 8% 白底的圆角块，
 * 不用粗体、不用色条。
 */

interface NavItem {
  to: string;
  label: string;
  /** 16x16 viewBox 的 path d。 */
  d: string;
}

const NAV: NavItem[] = [
  { to: '/', label: '首页', d: 'M2.5 7 8 2.5 13.5 7v6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z' },
  { to: '/library', label: '音乐库', d: 'M3 3.5h3.2v9H3zM9.8 3.5H13v9H9.8z' },
  { to: '/albums', label: '专辑', d: 'M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM8 6.4a1.6 1.6 0 1 0 0 3.2 1.6 1.6 0 0 0 0-3.2z' },
  { to: '/artists', label: '歌手', d: 'M8 2.8a2.6 2.6 0 1 0 0 5.2 2.6 2.6 0 0 0 0-5.2zM3.2 13.4a4.9 4.9 0 0 1 9.6 0' },
  { to: '/playlists', label: '歌单', d: 'M2 4h12M2 8h12M2 12h7' },
  { to: '/favorites', label: '收藏', d: 'M8 13.5S2.5 10.2 2.5 6.4A2.9 2.9 0 0 1 8 5a2.9 2.9 0 0 1 5.5 1.4c0 3.8-5.5 7.1-5.5 7.1z' },
  { to: '/settings', label: '设置', d: 'M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM8 5v3.2l2.2 1.4' },
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

export function Shell({ children }: { children: ReactNode }) {
  return (
    <div className="flex h-full">
      <aside className="flex w-[158px] shrink-0 flex-col border-r border-line-weak bg-black/15 p-[14px_10px_10px] max-[900px]:w-[60px] max-[900px]:p-[14px_8px_10px]">
        <div className="flex items-center gap-[9px] px-2 pt-1 pb-4 max-[900px]:justify-center max-[900px]:px-0">
          <span className="flex size-[26px] shrink-0 items-center justify-center rounded-md bg-accent text-white">
            <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
              <path d="M6 12.5a2 2 0 1 1-1.5-1.94V4.2l7-1.6v7.4a2 2 0 1 1-1.5-1.94V5.1L6 6.1z" />
            </svg>
          </span>
          <b className="text-sm font-semibold tracking-[.1px] max-[900px]:hidden">music-robot</b>
        </div>

        <nav className="flex flex-col gap-px">
          {NAV.map((item) => (
            <NavLink
              key={item.to}
              to={item.to}
              end={item.to === '/'}
              title={item.label}
              className={({ isActive }) =>
                [
                  'flex items-center gap-[10px] rounded-md px-[10px] py-2 text-[13px] transition-colors',
                  'max-[900px]:justify-center max-[900px]:px-0 max-[900px]:py-[9px]',
                  isActive
                    ? 'bg-surface text-ink'
                    : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
                ].join(' ')
              }
            >
              <Icon d={item.d} />
              <span className="max-[900px]:hidden">{item.label}</span>
            </NavLink>
          ))}
        </nav>

        <span className="flex-1" />
        <div className="flex items-center gap-2 px-[10px] py-2 text-xs text-ink-4 max-[900px]:justify-center max-[900px]:px-0">
          <Icon d="M10 4 6 8l4 4" size={13} />
          <span className="max-[900px]:hidden">收起</span>
        </div>
      </aside>

      <main className="flex min-w-0 flex-1 flex-col">{children}</main>
    </div>
  );
}

export function TopBar({ children }: { children?: ReactNode }) {
  return (
    <header className="flex h-[58px] shrink-0 items-center gap-3 px-[22px] max-[900px]:px-[14px]">
      {children}
    </header>
  );
}

export { Icon };
