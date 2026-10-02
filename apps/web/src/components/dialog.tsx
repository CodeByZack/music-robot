import { useEffect, useRef, useState, type ReactNode } from 'react';

/**
 * 当前打开的模态框数量。
 *
 * ⚠️ **必须有人问它**：全屏浮层页（`OverlayShell`）**也**在 `document` 上监听 Esc，
 * 而弹窗常常就开在那种页面里（点歌请求页的「点一首」、用户管理页的「新建用户」）。
 * 两个监听器都在 document 上，`stopPropagation` 拦不住同一个 target 上的另一个监听器
 * （只有 `stopImmediatePropagation` 能，那依赖注册顺序，很脆）。
 *
 * 后果实测过：按一次 Esc 先关弹窗、**同时也把整个页面关掉**，用户回到上一页，
 * 以为自己的操作丢了。所以由浮层页主动问「现在有弹窗吗」，有就不响应。
 */
let openModals = 0;

/** 有没有模态框开着。Esc 的多个监听器靠它排优先级（弹窗优先）。 */
export function isDialogOpen(): boolean {
  return openModals > 0;
}

/**
 * 居中弹窗（点歌请求表单、新建用户表单共用）。
 *
 * 与 `OverlayShell`（全屏浮层页面）的区别：这是**在某个页面之上**要一小块交互，
 * 做完就关、不改变 URL。表单类的东西用它 —— 用户填完就回原页面，不该被导航走。
 *
 * 三件必需品都做了：点遮罩关、Esc 关、打开时把焦点放进第一个输入框。
 * **没做焦点陷阱**（Tab 循环在弹窗内）—— 那是 Radix 的活，手写容易做出
 * 「Tab 进去出不来」。这里的表单最多两个字段，Tab 跑出去再点回来不算事故。
 *
 * ⚠️ 遮罩的 z-index 必须**高于**全屏浮层页（`z-50`）—— 点歌请求**页面**里也要
 * 能弹出它（页内「点一首」按钮）。用 `z-60`。
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
      className="fixed inset-0 z-60 flex items-start justify-center overflow-auto bg-black/55 p-5 backdrop-blur-sm"
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(e) => e.stopPropagation()}
        className={[
          'mt-[8vh] w-full max-w-[440px] rounded-2xl border border-line bg-[#1b1922] shadow-[0_18px_60px_rgba(0,0,0,.6)]',
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
  );
}
