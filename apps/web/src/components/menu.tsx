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
  /** 异步也行 —— `keepOpen` 的项要等它完成后重新拉二级列表。 */
  onClick?: () => void | Promise<void>;
  /** 危险动作（覆盖文件 / 退出登录这类）—— 用强调色标出来。 */
  danger?: boolean;
  disabled?: boolean;
  /**
   * 点开一个**二级列表**（比如「添加到歌单」）。与 `to` / `onClick` 互斥。
   *
   * 传函数而不是数组：内容要现拉（歌单列表会变），而且不想在菜单打开的瞬间
   * 就把请求打出去 —— 用户可能只是想点「重新刮削」。
   */
  submenu?: () => Promise<MenuItem[]>;
  /**
   * 点完**不关菜单**。
   *
   * 用在「需要当场看到结果」的操作上 —— 比如添加到歌单，关掉菜单用户就不知道
   * 到底加没加成功。这类项自己负责把状态反映到 label / hint 上。
   */
  keepOpen?: boolean;
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
  /** 二级列表的内容；`null` = 在顶层。 */
  const [sub, setSub] = useState<MenuItem[] | null>(null);
  const [subTitle, setSubTitle] = useState('');
  /** 当前二级列表的加载器。点完 `keepOpen` 项要**重新拉一遍** ——
      否则列表是打开那一刻的快照，「已添加」这种状态更新不到。 */
  const subLoaderRef = useRef<(() => Promise<MenuItem[]>) | null>(null);

  /** 展开二级列表 —— 现拉内容，拉之前先显示「载入中」。 */
  async function openSub(title: string, load: () => Promise<MenuItem[]>) {
    subLoaderRef.current = load;
    setSubTitle(title);
    setSub([]);
    try {
      setSub(await load());
    } catch (e) {
      setSub([{ label: e instanceof Error ? e.message : '载入失败', disabled: true }]);
    }
  }

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
      setSub(null); // 每次打开都从顶层开始，别把上次的二级列表留着
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
          'inline-flex size-6 items-center justify-center rounded-xs transition-colors hover:bg-surface-hover',
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
          {/* 一级才显示 header（用户信息那种）；进二级后顶部换成「返回」 */}
          {sub === null && header && (
            <div className="border-b border-line-weak px-3 pt-2 pb-2.5">{header}</div>
          )}

          {sub !== null && (
            <button
              type="button"
              role="menuitem"
              onClick={(e) => {
                e.stopPropagation();
                setSub(null);
              }}
              className="flex w-full items-center gap-2 px-3 py-2 text-left text-nav text-ink-3 transition-colors hover:bg-surface-hover"
            >
              <svg
                className="ico-xs shrink-0"
                viewBox="0 0 16 16"
                fill="none"
                stroke="currentColor"
                strokeWidth={1.5}
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M6 3.5 11 8l-5 4.5" />
              </svg>
              <span className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">
                {subTitle}
              </span>
            </button>
          )}

          {sub !== null && sub.length === 0 && (
            <div className="px-3 py-2 text-nav text-ink-4">载入中…</div>
          )}

          {(sub ?? items).map((it, idx) => {
            const cls = [
              'flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-nav transition-colors',
              it.disabled ? 'cursor-not-allowed text-ink-4' : 'hover:bg-surface-hover',
              !it.disabled && it.danger ? 'text-accent' : !it.disabled ? 'text-ink-2' : '',
            ].join(' ');
            const inner = (
              <>
                <span className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">
                  {it.label}
                </span>
                <span className="flex shrink-0 items-center gap-1.5">
                  {it.hint && <span className="text-cap text-ink-4">{it.hint}</span>}
                  {it.submenu && (
                    <svg
                      className="ico-xs text-ink-4"
                      viewBox="0 0 16 16"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth={1.5}
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    >
                      <path d="M10 3.5 5 8l5 4.5" />
                    </svg>
                  )}
                </span>
              </>
            );
            const key = `${it.label}-${idx}`;
            return it.to ? (
              <Link
                key={key}
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
                key={key}
                type="button"
                role="menuitem"
                disabled={it.disabled}
                onClick={(e) => {
                  e.stopPropagation();
                  // 有二级就先展开，别关菜单 —— 关了就没地方显示列表了
                  if (it.submenu) {
                    void openSub(it.label, it.submenu);
                    return;
                  }
                  if (!it.keepOpen) setOpen(false);
                  void Promise.resolve(it.onClick?.()).finally(() => {
                    // 不关菜单的操作，等它做完重新拉一遍二级列表，状态才更新得上
                    const load = subLoaderRef.current;
                    if (it.keepOpen && load) void openSub(subTitle, load);
                  });
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
