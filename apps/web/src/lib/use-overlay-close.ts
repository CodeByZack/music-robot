import { useCallback, useRef, useState } from 'react';
import { useNavigate } from 'react-router';

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
