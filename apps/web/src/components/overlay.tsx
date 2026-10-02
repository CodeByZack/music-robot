import { useEffect, type ReactNode } from 'react';
import { isDialogOpen } from '@/components/dialog.tsx';
import { useOverlayClose } from '@/lib/use-overlay-close.ts';

/**
 * 全屏浮层页面的外壳（**设置、点歌请求、用户管理共用**）。
 *
 * 这几个页面**不占内容区**，而是 `fixed inset-0` 盖住整个应用（侧边栏和顶栏都让位）——
 * 它们是**上下文切换**，不是「主界面里的第 N 页」（`docs/design.md` §12.2）。
 *
 * # 为什么没有横贯的标题栏
 *
 * 第一版做了一条 58px 的深色顶栏（左边标题、右边 ✕、底下一道分割线），用户的反馈是
 * **「看起来就像一个弹出层，怪怪的」** —— 说得准：那条横贯的条就是**对话框 header**
 * 的形状，而下面只有一条 720px 的窄列飘在暗色里，两者合起来就是「从上面盖下来的面板」。
 *
 * 现在改成：
 * - ✕ **浮在右上角**（不画条、不画线），任何滚动位置都够得着；
 * - 页面标题挪进内容区，用**和音乐库 / 歌单那些普通页面一样的 header 结构**
 *   （大标题 + 一行说明 + 右侧主要操作），宽度也放宽到 860px。
 *
 * 于是它读起来是「一个页面，右上角有关闭」，而不是「一个弹出来的层」。
 *
 * ⚠️ `pages/settings.tsx` 与 `pages/tag-edit.tsx` 比这里更早，各自内联了同一套逻辑
 * （tag-edit 的顶栏还要显示文件名与格式，结构不同）。**没有回头改它们** ——
 * 那是独立的一次清理，混在这次改动里只会让 diff 难读。
 */
export function OverlayShell({
  title,
  description,
  /** 页面主要操作，放在标题行右侧（和歌单页的「新建」同位置）。 */
  actions,
  children,
}: {
  title: string;
  /** 标题下面那行说明。写清楚「这一页是干什么的」，别只放个数量。 */
  description?: ReactNode;
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
        // `app-bg` 而不是 `bg-[#14121b]`：和主界面同一个底色，否则整页比主界面暗一层，
        // 看起来就是「盖上来的一块黑板」（见 global.css 里 --app-bg 的注释）。
        'app-bg fixed inset-0 z-50 flex flex-col overflow-hidden',
        closing ? 'anim-overlay-out' : 'anim-overlay-in',
      ].join(' ')}
    >
      {/* 绝对定位在**不滚动**的外层上，所以滚到哪儿它都在。 */}
      <button
        type="button"
        onClick={close}
        title="关闭"
        aria-label={`关闭${title}`}
        className="absolute top-3 right-3 z-10 flex size-9 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
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

      <div className="min-h-0 flex-1 overflow-auto px-5 pt-2 pb-20 max-[1024px]:px-4 max-[640px]:px-3">
        <div className="mx-auto w-full max-w-[860px]">
          {/* 页面 header —— 形状对齐 `playlists.tsx` / `library.tsx`。
              `pr-11` 是给右上角那颗 ✕ 让位（窄屏时内容会铺到它下面）。 */}
          <div className="flex items-end gap-4 pt-3 pb-5 pr-11 max-[640px]:flex-wrap">
            <div className="min-w-0">
              <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">
                {title}
              </h1>
              {description && <div className="mt-1 text-nav text-ink-3">{description}</div>}
            </div>
            <span className="flex-1" />
            {actions}
          </div>
          {children}
        </div>
      </div>
    </div>
  );
}
