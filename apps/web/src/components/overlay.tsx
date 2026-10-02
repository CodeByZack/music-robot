import { useEffect, type ReactNode } from 'react';
import { isDialogOpen } from '@/components/dialog.tsx';
import { useOverlayClose } from '@/lib/use-overlay-close.ts';

/**
 * 全屏浮层页面的外壳（**设置、点歌请求、用户管理共用**）。
 *
 * 这几个页面**不占内容区**，而是 `fixed inset-0` 盖住整个应用（侧边栏和顶栏都让位）——
 * 它们是**上下文切换**，不是「主界面里的第 N 页」（`docs/design.md` §12.2）。
 *
 * # 骨架 = 设置页那套（用户 2026-10-02 明确要求）
 *
 * ```
 * ┌──────────────────────────────────────────────┐
 * │ 标题                                    [操作] ✕ │  ← 58px 顶栏
 * ├────────────┬─────────────────────────────────┤
 * │ 分节导航   │  内容区（Panel 一块块）           │
 * │ 186px      │                                  │
 * └────────────┴─────────────────────────────────┘
 * ```
 *
 * 窄屏（<701px）分节导航变成横向 tab —— 与设置页同一个断点、同一套写法。
 *
 * # 中间走过的一版弯路（别退回去）
 *
 * 我一度把顶栏去掉、改成「大标题 + 说明 + 列表」的普通内容页，理由是「顶栏像对话框
 * header，看起来是个弹出层」。**方向错了**：用户要的不是「少一点 chrome」，
 * 而是**和设置页一致**。真正让那一版显得怪的是另外两件事，且都已修掉：
 * 1. 底色用了纯黑 `#14121b` 而不是应用的渐变底（见 `global.css` 的 `--app-bg`）；
 * 2. 内容只有一条 720px 窄列飘在暗色里，没有分节导航撑着，比例失衡。
 */
export interface OverlaySection {
  id: string;
  label: string;
}

export function OverlayShell({
  title,
  sections,
  section,
  onSection,
  /** 顶栏里 ✕ 左边的页面级主要操作（比如「点一首」）。 */
  actions,
  children,
}: {
  title: string;
  sections: OverlaySection[];
  section: string;
  onSection: (id: string) => void;
  actions?: ReactNode;
  children: ReactNode;
}) {
  // 关：优先回上一页（带动画），`history.length === 1` 时兜底回首页
  const { closing, close } = useOverlayClose('/');

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // ⚠️ 页内有弹窗时**不响应** Esc —— 否则一次 Esc 会同时关掉弹窗和整个页面。
      // 详见 `dialog.tsx` 里 `isDialogOpen` 的注释。
      if (e.key === 'Escape' && !isDialogOpen()) close();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [close]);

  return (
    <div
      className={[
        // `app-bg` 而不是 `bg-[#14121b]`：和主界面同一个底色（渐变），
        // 否则整页比主界面暗一层，看起来就是「盖上来的一块黑板」。
        'app-bg fixed inset-0 z-50 flex flex-col overflow-hidden',
        closing ? 'anim-overlay-out' : 'anim-overlay-in',
      ].join(' ')}
    >
      {/* 顶栏：与 settings.tsx 逐项对齐（58px / px-5 / 分割线 / 圆形 ✕） */}
      <div className="flex h-[58px] shrink-0 items-center gap-2 border-b border-line-weak px-5">
        <h2 className="text-lead font-medium">{title}</h2>
        <span className="flex-1" />
        {actions}
        <button
          type="button"
          onClick={close}
          title="关闭"
          aria-label={`关闭${title}`}
          className="flex size-8 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
        >
          <svg
            className="ico-sm"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth={1.6}
            strokeLinecap="round"
          >
            <path d="M4 4l8 8M12 4l-8 8" />
          </svg>
        </button>
      </div>

      <div className="flex min-h-0 flex-1 flex-col min-[701px]:flex-row">
        {/* 分节导航：窄屏横向 tab，桌面左栏（与设置页同一断点、同一套 class） */}
        <nav className="flex shrink-0 gap-1 overflow-x-auto border-b border-line-weak p-2 min-[701px]:w-[186px] min-[701px]:flex-col min-[701px]:border-r min-[701px]:border-b-0 min-[701px]:p-3">
          {sections.map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => onSection(s.id)}
              className={[
                'shrink-0 rounded-md px-3 py-2 text-left text-nav whitespace-nowrap transition-colors',
                section === s.id
                  ? 'bg-surface text-ink'
                  : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
              ].join(' ')}
            >
              {s.label}
            </button>
          ))}
        </nav>

        <div className="min-h-0 flex-1 overflow-auto p-5">{children}</div>
      </div>
    </div>
  );
}
