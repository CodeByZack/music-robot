/**
 * `@music-robot/core` —— 两端共用的业务逻辑。
 *
 * **唯一的铁律：这里不出现宿主 API。** 不 import `react`、不碰 `window` / `document` /
 * `localStorage`、不用 `<audio>`、不读 `import.meta.env`。
 * 判据一句话：**这段代码在 Node 里能跑吗？** 能跑就放这儿。
 *
 * 碰宿主的部分走 `apps/<app>/src/adapters/`（令牌存储、音频播放），core 只吃接口。
 */
export { createHttp, ApiError } from './http.ts';
export type { Http, HttpOptions } from './http.ts';
export { createMemoryTokenStore } from './token-store.ts';
export type { TokenStore } from './token-store.ts';

export {
  createQueue,
  currentId,
  isFinished,
  jumpTo,
  next,
  prev,
  replaceQueue,
  setMode,
  PLAY_MODES,
} from './player-queue.ts';
export type { PlayMode, QueueState } from './player-queue.ts';

export { createApi } from './api/index.ts';
export type { Api } from './api/index.ts';

export type * from './types.ts';
