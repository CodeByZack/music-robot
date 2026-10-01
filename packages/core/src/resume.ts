/**
 * 断点续播的**纯逻辑**：键怎么拼、读出来的值算不算数、该写什么回去。
 *
 * 存哪儿由宿主决定 —— 现在落在后端通用的 settings 表里（键 = `resume:{song_id}`，
 * 值是毫秒的十进制字符串），这就是后端的**既有能力，不是新接口**。
 * 见 `src/server/routes/playback.rs` 的 `RESUME_KEY_PREFIX` 与模块头注释。
 * 这边不碰网络、不碰 `localStorage`，所以能直接在 Node 里测。
 */

import type { SettingsMap } from './types.ts';

/**
 * 键前缀。**必须和后端 `RESUME_KEY_PREFIX` 一致** —— 后端只有这一份约定，
 * 前端拼错了不会报错，只是永远读不到断点（静默失效）。
 */
export const RESUME_KEY_PREFIX = 'resume:';

/** 小于这个位置就当没听过 —— 记住它只会让人下次一进来就跳一下，很烦。 */
const MIN_RESUME_MS = 5_000;

/** 距离结尾这么近就算是听完了：删键，下次从头。 */
const TAIL_MS = 10_000;

export function resumeKey(songId: number): string {
  return `${RESUME_KEY_PREFIX}${songId}`;
}

/**
 * 从 settings 里读续播点（毫秒）。
 *
 * 返回 `null` 的三种情况都按「从头播」处理：键不存在、值不是合法数字、位置太靠前。
 */
export function readResumeMs(settings: SettingsMap, songId: number): number | null {
  const raw = settings[resumeKey(songId)];
  if (raw === undefined) return null;
  const ms = Number(raw);
  // `Number('')` 是 0，`Number(' 1 ')` 是 1 —— 但 settings 里的值只可能是后端写回来的，
  // 这里仍然当作**外部输入**校验，宁可当没有，也不要 seek 到一个荒唐的位置。
  if (!Number.isFinite(ms) || ms < MIN_RESUME_MS) return null;
  return ms;
}

/**
 * 算出该往 settings 里写什么：返回毫秒字符串；**返回 `null` 表示「删掉这个键」**。
 *
 * 什么时候删：刚开始（< [MIN_RESUME_MS]）或者已经播到尾（距结尾 < [TAIL_MS]）。
 * 后半条同时兜住了「播完自动下一首」—— 播完时 `positionMs ≈ durationMs`，
 * 于是上一首的键被顺手清掉，不需要给 `ended` 事件单开一条分支。
 *
 * `durationMs` 未知（0 / NaN）时不做尾部判断：**不确定就不删**，宁可留着。
 */
export function writeResumeMs(positionMs: number, durationMs: number): string | null {
  if (!Number.isFinite(positionMs) || positionMs < MIN_RESUME_MS) return null;
  if (Number.isFinite(durationMs) && durationMs > 0 && durationMs - positionMs < TAIL_MS) return null;
  return String(Math.floor(positionMs));
}
