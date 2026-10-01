/**
 * `<audio>` 适配器 —— core 的播放队列状态机只算「下一首是谁」，
 * **真正出声的那层是宿主能力**，所以它在 adapters/ 里，不在 core 里。
 * 将来 mobile 写一份 `audio.native.ts`（expo-audio），core 一行不用改。
 */

export type AudioEvent = 'time' | 'ended' | 'error' | 'playing' | 'paused';
type Listener = () => void;

export interface AudioAdapter {
  /**
   * `path` 是后端的媒体路径（如 `/api/stream/12`），由 `resolveSrc` 换成可播 URL。
   *
   * **返回 Promise**：因为 blob 绕法要先把文件取回来。调用方必须 `await` 之后再 `play()`，
   * 否则 play() 会在 src 还没设上时被调用 —— 什么都不会发生，且不报错（踩过）。
   */
  load(path: string): Promise<void>;
  play(): Promise<void>;
  pause(): void;
  seekMs(ms: number): void;
  currentMs(): number;
  durationMs(): number;
  on(ev: AudioEvent, fn: Listener): () => void;
  destroy(): void;
}

export interface AudioAdapterOptions {
  /**
   * 把后端媒体路径解析成 `<audio>` 能直接吃的 URL。
   *
   * **为什么需要这一层**：`/api/stream/{id}` 在 `require_auth` 后面，
   * 而 `<audio src>` 发的是裸 GET，**带不了 `Authorization` 头**（实测裸 GET → 401）。
   * Web 端的绕法是先带令牌 fetch 成 blob，再喂给 `<audio>`（见 `lib/client.ts`）。
   * 等后端给媒体端点加上 cookie / 短票据鉴权，这里换成直接返回原路径即可，**其余代码不用动**。
   */
  resolveSrc: (path: string) => Promise<string>;
}

export function createAudioAdapter({ resolveSrc }: AudioAdapterOptions): AudioAdapter {
  const el = new Audio();
  el.preload = 'metadata';

  // 快速连点下一首时，前一次 load 的 fetch 可能后完成。
  // 用代数计数把过期结果丢掉，否则会把已经不该播的那首喂进去。
  let generation = 0;

  const map: Record<AudioEvent, Set<Listener>> = {
    time: new Set(),
    ended: new Set(),
    error: new Set(),
    playing: new Set(),
    paused: new Set(),
  };
  const emit = (ev: AudioEvent) => () => {
    for (const fn of map[ev]) fn();
  };

  el.addEventListener('timeupdate', emit('time'));
  el.addEventListener('ended', emit('ended'));
  el.addEventListener('error', emit('error'));
  el.addEventListener('playing', emit('playing'));
  el.addEventListener('pause', emit('paused'));

  return {
    async load(path) {
      const gen = ++generation;
      try {
        const src = await resolveSrc(path);
        if (gen !== generation) return; // 期间已经换歌，丢弃
        el.src = src;
        el.load();
      } catch (e) {
        if (gen === generation) emit('error')();
        throw e;
      }
    },
    // play() 会因自动播放策略 reject（用户没交互过）。这个项目里播放总由点击触发，
    // 但仍然不能让它变成未处理的 rejection。
    play: () => el.play(),
    pause: () => el.pause(),
    seekMs(ms) {
      if (Number.isFinite(el.duration)) el.currentTime = ms / 1000;
    },
    currentMs: () => (Number.isFinite(el.currentTime) ? el.currentTime * 1000 : 0),
    durationMs: () => (Number.isFinite(el.duration) ? el.duration * 1000 : 0),
    on(ev, fn) {
      map[ev].add(fn);
      return () => map[ev].delete(fn);
    },
    destroy() {
      generation += 1;
      el.pause();
      el.src = '';
      for (const set of Object.values(map)) set.clear();
    },
  };
}
