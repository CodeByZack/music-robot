import { useEffect, useRef, useState, type ReactNode } from 'react';

/**
 * 一个很小的浮层（点 trigger → 弹一层，**内容任意**），手写的，没有依赖。
 *
 * 与 `menu.tsx` 的分工（别把两者合并）：
 *
 * | | `MenuButton` | `Popover` |
 * |---|---|---|
 * | 内容 | 一行行**菜单项**（`MenuItem[]`） | 任意内容：列表、表单、按钮组 |
 * | 点内部 | 点了就关（`keepOpen` 除外）、自己接管事件 | **不接管**，关不关由内容决定 |
 *
 * 抽共用 hook 把两边合起来试过，收益是负的：菜单的展开方向判断依赖菜单项数量、
 * 还带二级列表状态，而面板只需要「固定朝下 + 内部自己滚」。真正的重复只有
 * 「点外面关 / Esc 关」这十来行，重复就重复了。
 *
 * 无障碍口径与 `menu.tsx` 一致：trigger 是 `<button>`（Tab 到得了、Enter 打得开），
 * 面板 `role="dialog"` + `aria-label`。**没做焦点陷阱** —— 那是 Radix 的活，
 * 手写容易做出「Tab 进去出不来」。
 */
export function Popover({
  label,
  title,
  width = 340,
  trigger,
  children,
}: {
  /** trigger 的 tooltip 与无障碍名（顶栏图标没有文字，必须给）。 */
  label: string;
  /** 面板的无障碍名。 */
  title: string;
  width?: number;
  /** trigger 里的图标。外面那颗 `<button>` 由本组件提供。 */
  trigger: ReactNode;
  /**
   * 面板内容。
   *
   * ⚠️ **只在打开时挂载** —— 这是刻意的：内容里的取数逻辑因此每次打开都重跑一遍，
   * 不必额外写「打开时刷新」。反过来说，重内容（大列表）挂载一次的成本要自己掂量。
   */
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    // capture 阶段：顶栏之外的地方也可能有 onClick，先于它们拿到这一下
    const onDown = (e: MouseEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDown, true);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown, true);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  return (
    <div ref={wrapRef} className="relative inline-flex">
      <button
        type="button"
        title={label}
        aria-label={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className={[
          'flex size-8 shrink-0 items-center justify-center rounded-full transition-colors',
          open ? 'bg-surface text-ink' : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
        ].join(' ')}
      >
        {trigger}
      </button>

      {open && (
        <div
          role="dialog"
          aria-label={title}
          className="absolute top-full right-0 z-40 mt-2 overflow-hidden rounded-xl border border-line bg-[#1b1922] shadow-[0_10px_34px_rgba(0,0,0,.55)]"
          style={{ width }}
        >
          {children}
        </div>
      )}
    </div>
  );
}
