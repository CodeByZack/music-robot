import { useEffect, useRef, useState, type ReactNode } from 'react';

/**
 * 当前打开的模态框数量。
 *
 * ⚠️ **必须有调用方**（`useEscapeToClose`）：全屏浮层页也监听 Esc，而弹窗就开在
 * 那种页面里（设置页「点歌请求」分节的「点一首」）。两边的监听器都挂在 `document` 上，
 * `stopPropagation` 拦不住同一个 target 上的另一个监听器（只有
 * `stopImmediatePropagation` 能，那依赖注册顺序，很脆）。
 *
 * 实测后果：在设置页里点「点一首」→ 按 Esc → 弹窗和**整个设置页**一起关掉，
 * 用户被弹回上一页，以为自己把设置弄丢了。
 *
 * 这个坑踩过两次：第一次修好之后，我把点歌请求并进设置页时以为「设置页里没有弹窗」
 * 就把计数器删了 —— 但新分节里正好有一个。**别再删。**
 */
let openModals = 0;

/** 有没有模态框开着。Esc 的多个监听器靠它排优先级（弹窗优先）。 */
export function isDialogOpen(): boolean {
  return openModals > 0;
}

/**
 * 居中弹窗（点歌请求表单）。
 *
 * 与全屏浮层页（设置 / 播放 / 标签编辑）的区别：这是**在某个页面之上**要一小块交互，
 * 做完就关、不改变 URL。表单类的东西用它 —— 用户填完就回原页面，不该被导航走。
 *
 * 三件必需品都做了：点遮罩关、Esc 关、打开时把焦点放进第一个输入框。
 * **没做焦点陷阱**（Tab 循环在弹窗内）—— 那是 Radix 的活，手写容易做出
 * 「Tab 进去出不来」。这里的表单最多两个字段，Tab 跑出去再点回来不算事故。
 *
 * 居中写法说明：**不是** `flex items-center` 直接套面板 —— 那样内容比视口高时
 * 会把顶部裁掉且滚不上去（flex 居中 + overflow 的经典坑）。
 * 正确写法是「滚动容器 → `min-h-full` 的居中行 → 面板」：内容矮时居中，
 * 高时从顶部开始且能滚。
 *
 * ⚠️ 遮罩的 z-index 必须**高于**全屏浮层页（`z-50`）—— 设置页那个浮层里也要能弹出它。
 * 用 `z-60`。
 */
export function Dialog({
  title,
  onClose,
  children,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const panelRef = useRef<HTMLDivElement | null>(null);
  /** 关闭过的标记 —— 避免「父组件已卸载，回调还在跑」。 */
  const [open, setOpen] = useState(true);

  // 挂载期间给全局计数 +1 —— 浮层页的 Esc 处理器靠它避让（见文件头的注释）。
  useEffect(() => {
    openModals += 1;
    return () => {
      openModals -= 1;
    };
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose]);

  // 打开时把焦点送到第一个输入框：表单弹窗不这么做，用户得先点一下才能打字
  useEffect(() => {
    const first = panelRef.current?.querySelector('input, textarea');
    if (first instanceof HTMLElement) first.focus();
  }, []);

  return (
    <div
      role="presentation"
      onClick={() => {
        setOpen(false);
        onClose();
      }}
      className="fixed inset-0 z-60 overflow-y-auto bg-black/55 backdrop-blur-sm"
    >
      <div className="flex min-h-full items-center justify-center p-5">
        <div
          ref={panelRef}
          role="dialog"
          aria-modal="true"
          aria-label={title}
          onClick={(e) => e.stopPropagation()}
          className={[
            'w-full max-w-[440px] rounded-2xl border border-line bg-[#1b1922] shadow-[0_18px_60px_rgba(0,0,0,.6)]',
            open ? 'anim-overlay-in' : 'anim-overlay-out',
          ].join(' ')}
        >
          <div className="flex items-center gap-2 border-b border-line-weak px-4 py-3">
            <h3 className="text-nav font-medium">{title}</h3>
            <span className="flex-1" />
            <button
              type="button"
              onClick={onClose}
              title="关闭"
              aria-label={`关闭${title}`}
              className="flex size-7 items-center justify-center rounded-full text-ink-4 transition-colors hover:bg-surface-hover hover:text-ink"
            >
              <svg
                className="ico-xs"
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
          {children}
        </div>
      </div>
    </div>
  );
}
