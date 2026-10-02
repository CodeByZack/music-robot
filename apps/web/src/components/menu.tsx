import { useEffect, useRef, useState, type ReactNode } from 'react';
import { Link } from 'react-router';

/**
 * 一个很小的下拉菜单（点一个 trigger → 弹一层），**手写的，没有依赖**。
 *
 * 什么时候该换成 shadcn/Radix 的 `DropdownMenu`：当你需要
 * **碰撞翻转（贴边自动换方向 + 换对齐）、焦点陷阱、子菜单、键盘首字母跳转** 时。
 * 这里只做了三件必需品：点外面关、Esc 关、点了菜单项关；
 * 外加一个「下方放不下就往上翻」的粗略判断（表格最后几行会被底部播放条压住）。
 *
 * 无障碍口径：trigger 是 `<button>`（所以键盘 Tab 到得了、Enter/Space 打得开），
 * 面板 `role="menu"`、菜单项 `role="menuitem"`。**没做**键盘上下键在菜单内移动 ——
 * 那是 Radix 的活，手写容易写出「Tab 进去出不来」的坑。
 */

export interface MenuItem {
  label: string;
  /** 右侧的次要说明（比如快捷键、后果）。 */
  hint?: string;
  /** 给路由跳转用（渲染成 `<Link>`，别用整页刷新）。与 `onClick` 二选一。 */
  to?: string;
  onClick?: () => void;
  /** 危险动作（覆盖文件 / 退出登录这类）—— 用强调色标出来。 */
  danger?: boolean;
  disabled?: boolean;
}

export function MenuButton({
  items,
  title = '更多操作',
  header,
  align = 'right',
  children,
}: {
  items: MenuItem[];
  title?: string;
  /** 面板顶部的一块自定义内容（比如用户信息）。 */
  header?: ReactNode;
  align?: 'left' | 'right';
  /** trigger 里的图标 —— 外面那颗 `<button>` 由本组件提供。 */
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const [up, setUp] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    // 用 capture：表格行自己也有 onClick（点行 = 播放），得先于它拿到事件
    document.addEventListener('mousedown', onDown, true);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown, true);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  function toggle() {
    if (!open) {
      // 打开前粗判一下方向：下面剩下的空间装不下一层菜单就往上报
      const r = wrapRef.current?.getBoundingClientRect();
      if (r) setUp(window.innerHeight - r.bottom < items.length * 34 + 90);
    }
    setOpen((v) => !v);
  }

  return (
    <div ref={wrapRef} className="relative inline-flex">
      <button
        type="button"
        title={title}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={(e) => {
          e.stopPropagation(); // 别让「点行播放」抢走这一下
          toggle();
        }}
        className={[
          'inline-flex size-6 items-center justify-center rounded transition-colors hover:bg-surface-hover',
          open ? 'bg-surface text-ink' : 'text-ink-4',
        ].join(' ')}
      >
        {children}
      </button>

      {open && (
        <div
          role="menu"
          onClick={(e) => e.stopPropagation()}
          className={[
            'absolute z-40 min-w-[188px] rounded-lg border border-line bg-[#1b1922] py-1 shadow-[0_10px_34px_rgba(0,0,0,.55)]',
            align === 'right' ? 'right-0' : 'left-0',
            up ? 'bottom-full mb-1' : 'top-full mt-1',
          ].join(' ')}
        >
          {header && <div className="border-b border-line-weak px-3 pt-2 pb-2.5">{header}</div>}
          {items.map((it) => {
            const cls = [
              'flex w-full items-center justify-between gap-3 px-3 py-[7px] text-left text-nav transition-colors',
              it.disabled ? 'cursor-not-allowed text-ink-4' : 'hover:bg-surface-hover',
              !it.disabled && it.danger ? 'text-accent' : !it.disabled ? 'text-ink-2' : '',
            ].join(' ');
            const inner = (
              <>
                <span>{it.label}</span>
                {it.hint && <span className="shrink-0 text-cap text-ink-4">{it.hint}</span>}
              </>
            );
            return it.to ? (
              <Link
                key={it.label}
                to={it.to}
                role="menuitem"
                onClick={(e) => {
                  e.stopPropagation();
                  setOpen(false);
                }}
                className={cls}
              >
                {inner}
              </Link>
            ) : (
              <button
                key={it.label}
                type="button"
                role="menuitem"
                disabled={it.disabled}
                onClick={(e) => {
                  e.stopPropagation();
                  setOpen(false);
                  it.onClick?.();
                }}
                className={cls}
              >
                {inner}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
