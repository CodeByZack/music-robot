import { useState, type ReactNode } from 'react';
import { Link, NavLink } from 'react-router';

/**
 * 应用外壳：安静侧边栏 + 内容区。
 *
 * 形态来源见 docs/design.md §7.5：**"不像后台"的秘密不在有没有侧边栏，
 * 而在侧边栏长什么样** —— 窄、图标+小字、选中态只是一个 8% 白底的圆角块，
 * 不用粗体、不用色条。
 *
 * 设置**不在侧边栏里**（用户 2026-10-02 要求），在右上角那颗齿轮上 —— 见下面
 * 挂在 `<main>` 里的绝对定位按钮。侧边栏只放"去哪找歌"这件事。
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
];

const SETTINGS_D = 'M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM8 5v3.2l2.2 1.4';

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
  /**
   * 侧边栏收起。收起后的形态**跟窄屏（<900px）那档完全一样**：只剩图标。
   * 所以每条类名都是「二选一」，不叠加 —— 叠加会踩 Tailwind 的排序坑
   * （`px-0` 和 `px-[10px]` 同权重，谁赢看出现在 CSS 里的先后，不是 class 的顺序）。
   *
   * 状态只在会话内保留：刷新回到展开。要跨刷新得进 localStorage，先不做。
   */
  const [collapsed, setCollapsed] = useState(false);

  const asideCls = collapsed ? 'w-[60px] p-[14px_8px_10px]' : 'w-[158px] p-[14px_10px_10px]';
  const brandCls = collapsed ? 'justify-center px-0' : 'px-2';
  const itemCls = collapsed ? 'justify-center px-0 py-[9px]' : 'px-[10px] py-2';
  const hideWhenMini = collapsed ? 'hidden' : '';

  return (
    <div className="flex h-full">
      <aside
        className={`flex shrink-0 flex-col border-r border-line-weak bg-black/15 max-[900px]:w-[60px] max-[900px]:p-[14px_8px_10px] ${asideCls}`}
      >
        <div className={`flex items-center gap-[9px] pt-1 pb-4 max-[900px]:justify-center max-[900px]:px-0 ${brandCls}`}>
          <span className="flex size-[26px] shrink-0 items-center justify-center rounded-md bg-accent text-white">
            <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
              <path d="M6 12.5a2 2 0 1 1-1.5-1.94V4.2l7-1.6v7.4a2 2 0 1 1-1.5-1.94V5.1L6 6.1z" />
            </svg>
          </span>
          <b className={`text-sm font-semibold tracking-[.1px] max-[900px]:hidden ${hideWhenMini}`}>
            music-robot
          </b>
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
                  'flex items-center gap-[10px] rounded-md text-[13px] transition-colors',
                  'max-[900px]:justify-center max-[900px]:px-0 max-[900px]:py-[9px]',
                  itemCls,
                  isActive
                    ? 'bg-surface text-ink'
                    : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
                ].join(' ')
              }
            >
              <Icon d={item.d} />
              <span className={`max-[900px]:hidden ${hideWhenMini}`}>{item.label}</span>
            </NavLink>
          ))}
        </nav>

        <span className="flex-1" />

        {/* 以前这里是**一个没有 onClick 的 div** —— 画着好看，点了没反应（用户报的）。
            现在它真的是按钮了。 */}
        <button
          type="button"
          onClick={() => setCollapsed((v) => !v)}
          title={collapsed ? '展开侧边栏' : '收起侧边栏'}
          className={[
            'flex items-center rounded-md py-2 text-xs text-ink-4 transition-colors',
            'hover:bg-surface-hover hover:text-ink',
            'max-[900px]:justify-center max-[900px]:px-0',
            collapsed ? 'justify-center px-0' : 'gap-2 px-[10px]',
          ].join(' ')}
        >
          <Icon d={collapsed ? 'M6 4l4 4-4 4' : 'M10 4 6 8l4 4'} size={13} />
          <span className={`max-[900px]:hidden ${hideWhenMini}`}>{collapsed ? '展开' : '收起'}</span>
        </button>
      </aside>

      <main className="relative flex min-w-0 flex-1 flex-col">
        {/* 设置挪到右上角。挂在这儿而不是塞进每个页面的 TopBar：页面自己有 header
            的时候（音乐库有搜索框）右边本来就是空的，绝对定位不会挤到谁。 */}
        <Link
          to="/settings"
          title="设置"
          className="absolute top-[13px] right-[22px] z-10 flex size-8 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink max-[900px]:right-[14px]"
        >
          <Icon d={SETTINGS_D} size={16} />
        </Link>
        {children}
      </main>
    </div>
  );
}

export { Icon };
