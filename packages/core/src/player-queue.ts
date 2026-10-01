/**
 * 播放队列状态机 —— **纯函数、无副作用、不碰任何宿主 API**。
 *
 * 画布区块 ⑨ 定的规则：**播放队列由前端维护，后端不持久化**。
 * 只有「断点续播的位置」会写回 `user_settings`，那是另一条路径。
 *
 * 四种模式：`order`（顺序）/ `shuffle`（随机）/ `repeat-one`（单曲循环）/ `repeat-all`（列表循环）。
 *
 * 两个容易写错、这里刻意做对的地方：
 * 1. **切到随机时，当前这首歌必须还在原地。** 天真的做法是「重新洗整个队列」，
 *    结果用户一按随机，正在放的歌立刻换了 —— 体感是 bug。
 * 2. **切回顺序时，光标要落在当前这首歌的顺序位置上**，而不是回到 0。
 *
 * 随机源可注入，测试才能确定性地断言。
 */

export type PlayMode = 'order' | 'shuffle' | 'repeat-one' | 'repeat-all';

export const PLAY_MODES: readonly PlayMode[] = ['order', 'shuffle', 'repeat-one', 'repeat-all'];

export interface QueueState {
  /** 队列里的曲目 id，**保持加入顺序**，不随模式变化。 */
  readonly trackIds: readonly number[];
  /** 播放顺序：`order[i]` 是第 i 个播的曲目在 `trackIds` 里的下标。 */
  readonly order: readonly number[];
  /** 当前在 `order` 里的位置。等于 `order.length` 表示「放完了」。 */
  readonly cursor: number;
  readonly mode: PlayMode;
}

/** Fisher-Yates，原地洗一个下标数组。 */
function shuffleInPlace(xs: number[], rand: () => number): number[] {
  for (let i = xs.length - 1; i > 0; i--) {
    const j = Math.floor(rand() * (i + 1));
    const a = xs[i] as number;
    const b = xs[j] as number;
    xs[i] = b;
    xs[j] = a;
  }
  return xs;
}

/**
 * 建队列。`startAt` 可以指定从哪首歌开始（默认第一首）。
 * 随模式下，**起始那首固定在第 0 位**，其余打乱。
 */
export function createQueue(
  trackIds: readonly number[],
  mode: PlayMode = 'order',
  startAt?: number,
  rand: () => number = Math.random,
): QueueState {
  const all = trackIds.map((_, i) => i);
  if (trackIds.length === 0) {
    return { trackIds: [...trackIds], order: [], cursor: 0, mode };
  }

  const startIdx = startAt === undefined ? 0 : Math.max(0, trackIds.indexOf(startAt));
  const rest = all.filter((i) => i !== startIdx);
  const order = mode === 'shuffle' ? [startIdx, ...shuffleInPlace(rest, rand)] : all;

  return {
    trackIds: [...trackIds],
    order,
    cursor: mode === 'shuffle' ? 0 : startIdx,
    mode,
  };
}

/** 当前该播哪首；放完了返回 null。 */
export function currentId(state: QueueState): number | null {
  const at = state.order[state.cursor];
  if (at === undefined) return null;
  return state.trackIds[at] ?? null;
}

/** 是否已放完（顺序/随机模式下走到末尾；循环模式永远不会 finished）。 */
export function isFinished(state: QueueState): boolean {
  if (state.mode === 'repeat-all' || state.mode === 'repeat-one') return false;
  return state.cursor >= state.order.length;
}

/**
 * 下一首。
 * - `repeat-one`：光标不动（由播放器重播当前曲）
 * - `repeat-all`：到底回绕到 0
 * - `order` / `shuffle`：到底则 `cursor = order.length`（表示放完）
 */
export function next(state: QueueState): QueueState {
  if (state.order.length === 0) return state;
  if (state.mode === 'repeat-one') return state;

  const at = state.cursor + 1;
  if (at < state.order.length) return { ...state, cursor: at };
  if (state.mode === 'repeat-all') return { ...state, cursor: 0 };
  return { ...state, cursor: state.order.length };
}

/** 上一首。已在开头就停在开头（不回绕 —— 回绕是「下一首到底」的行为，不是上一首的）。 */
export function prev(state: QueueState): QueueState {
  if (state.order.length === 0) return state;
  return { ...state, cursor: Math.max(0, state.cursor - 1) };
}

/**
 * 切模式。见文件头两条：当前曲目必须留在原地。
 */
export function setMode(state: QueueState, mode: PlayMode, rand: () => number = Math.random): QueueState {
  if (mode === state.mode) return state;

  const at = state.order[state.cursor];
  if (at === undefined) {
    // 已经放完 / 空队列：只改标记，顺序照原样重建
    return { ...state, mode, order: state.trackIds.map((_, i) => i), cursor: state.trackIds.length };
  }

  if (mode === 'shuffle') {
    const rest = state.trackIds.map((_, i) => i).filter((i) => i !== at);
    return { ...state, mode, order: [at, ...shuffleInPlace(rest, rand)], cursor: 0 };
  }

  // 切回顺序 / 循环：顺序即自然序，光标落在当前曲目那一格
  const natural = state.trackIds.map((_, i) => i);
  return { ...state, mode, order: natural, cursor: natural.indexOf(at) };
}

/** 直接跳到某首歌（点列表、点队列都走它）。不在队列里则原样返回。 */
export function jumpTo(state: QueueState, trackId: number): QueueState {
  const at = state.cursor < state.order.length ? state.order[state.cursor] : undefined;
  const found = state.trackIds.indexOf(trackId);
  if (found < 0) return state;

  // 当前曲目可能被随机打乱到任意位置，按 id 找它在 order 里的位置
  const pos = state.order.indexOf(found);
  if (pos < 0) return state;

  // 已经在这一首上就什么都不做（避免无意义的状态变化）
  if (at === found) return state;
  return { ...state, cursor: pos };
}

/**
 * 从某个位置开始重建队列（比如「从此处播放」）。
 * 保持传入的 trackIds 顺序，光标指向 startAt。
 */
export function replaceQueue(
  state: QueueState,
  trackIds: readonly number[],
  startAt?: number,
  rand: () => number = Math.random,
): QueueState {
  return createQueue(trackIds, state.mode, startAt, rand);
}
