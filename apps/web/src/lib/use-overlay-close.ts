import { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router';
import { isDialogOpen } from '@/components/dialog.tsx';

/**
 * 全屏浮层（设置页 / 播放页）的「带动画关闭」。
 *
 * 两个浮层都盖在整个应用上，直接 `navigate(-1)` 是硬切 —— 很生硬。
 * 这里先挂上退场动画的 class，等 ~180ms 动画播完再真正导航。
 *
 * **兜底**：直接敲地址栏打开 `/settings` 或 `/now` 时 `history.length === 1`，
 * 这时 `navigate(-1)` 什么都不会发生，页面就卡在浮层里出不去 —— 回 `fallback`。
 *
 * 尊重 `prefers-reduced-motion`：开了就跳过动画直接关。
 */
export function useOverlayClose(fallback = '/', ms = 180) {
  const navigate = useNavigate();
  const [closing, setClosing] = useState(false);
  // 用 ref 而不是 state 做「已经点了」的判断：state 更新是异步的，
  // 连按两下 Esc 会排两个定时器。
  const busy = useRef(false);

  const close = useCallback(() => {
    if (busy.current) return;
    busy.current = true;

    const finish = () => {
      if (window.history.length > 1) navigate(-1);
      else navigate(fallback);
    };

    if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) {
      finish();
      return;
    }
    setClosing(true);
    window.setTimeout(finish, ms);
  }, [navigate, fallback, ms]);

  return { closing, close };
}

/**
 * 让 Esc 关掉全屏浮层页（设置 / 播放 / 标签编辑）。
 *
 * ⚠️ **必须用这个，别自己写 `document.addEventListener('keydown')`**：
 * 浮层页里可能开着弹窗（比如设置页「点歌请求」分节的「点一首」），
 * 而弹窗**也**监听 Esc。两个监听器都挂在 `document` 上，
 * `stopPropagation` 拦不住同一个 target 上的另一个（只有 `stopImmediatePropagation`
 * 能，那依赖注册顺序，很脆）。于是按一次 Esc 会同时关掉弹窗和整个页面 ——
 * 用户被弹回上一页，以为自己把设置弄丢了。实测踩过两次。
 *
 * 这里统一先问 `isDialogOpen()`：有弹窗就让给弹窗，浮层页不动。
 *
 * （设置 / 播放 / 标签编辑原来各自内联了这段监听，三份都没有这个判断。）
 */
export function useEscapeToClose(close: () => void): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !isDialogOpen()) close();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [close]);
}
