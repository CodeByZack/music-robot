import { useEffect, type ReactNode } from 'react';
import { isDialogOpen } from '@/components/dialog.tsx';
import { useOverlayClose } from '@/lib/use-overlay-close.ts';

/**
 * 全屏浮层页面的外壳（**设置、点歌请求、用户管理共用**）。
 *
 * 这几个页面**不占内容区**，而是 `fixed inset-0` 盖住整个应用（侧边栏和顶栏都让位）——
 * 它们是**上下文切换**，不是「主界面里的第 N 页」（`docs/design.md` §12.2）。
 *
 * 抽这一个外壳是因为「顶栏 + ✕ + Esc + 进出场动画 + `history.length` 兜底」这套
 * 到处一样，抄三遍必有一处忘记兜底（那个坑很隐蔽：直接敲地址打开时 `navigate(-1)`
 * 什么都不做，页面就卡在浮层里出不去）。
 *
 * ⚠️ `pages/settings.tsx` 与 `pages/tag-edit.tsx` 比这里更早，各自内联了同一套逻辑
 * （tag-edit 的顶栏还要显示文件名与格式，结构不同）。**没有回头改它们** ——
 * 那是独立的一次清理，混在这次改动里只会让 diff 难读。
 */
export function OverlayShell({
  title,
  /** 顶栏右侧、✕ 左边的自定义内容（数量、操作按钮）。 */
  actions,
  /** 顶栏标题右边紧跟的次要信息。 */
  subtitle,
  children,
}: {
  title: string;
  actions?: ReactNode;
  subtitle?: ReactNode;
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
        'fixed inset-0 z-50 flex flex-col overflow-hidden bg-[#14121b]',
        closing ? 'anim-overlay-out' : 'anim-overlay-in',
      ].join(' ')}
    >
      <div className="flex h-[58px] shrink-0 items-center gap-2.5 border-b border-line-weak px-5">
        <h2 className="text-lead font-medium">{title}</h2>
        {subtitle}
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

      <div className="min-h-0 flex-1 overflow-auto">
        {/* 页面内容统一收在 720px 里：这两页都是「列表 + 少量操作」，
            不限宽的话在 1500px 视口下每行会被拉得很长、很难读。 */}
        <div className="mx-auto w-full max-w-[720px] px-5 py-5 pb-20">{children}</div>
      </div>
    </div>
  );
}
