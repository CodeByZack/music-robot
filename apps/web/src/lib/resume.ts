/**
 * 断点续播的**宿主侧**：跟后端 settings 表打交道，外加节流。
 * 「键怎么拼、值算不算数」那些判断在 `@music-robot/core` 的 `resume.ts` 里（那边有单测），
 * 这里只负责发请求和别把 NAS 的 sqlite 打爆。
 */

import { readResumeMs, resumeKey, writeResumeMs, type SettingsMap } from '@music-robot/core';
import { api } from '@/lib/client.ts';

/** 同一首歌最多每 15 秒落一次盘。`timeupdate` 是 4Hz 的，不能照着它写。 */
const SAVE_INTERVAL_MS = 15_000;

export interface ResumeStore {
  /** 读续播点（毫秒）。首次调用会 GET 一次 settings，之后走内存缓存。 */
  get(songId: number): Promise<number | null>;
  /** 记位置，**带节流**（播放中每 15 秒一次）。值没变就不写。 */
  save(songId: number, positionMs: number, durationMs: number): void;
  /** 记位置，**不节流**。换歌 / 暂停这种「再也不会更新了」的时刻用。 */
  saveNow(songId: number, positionMs: number, durationMs: number): void;
}

export function createResumeStore(): ResumeStore {
  let loaded: Promise<void> | null = null;
  const cache: SettingsMap = {};

  let sentKey: string | null = null;
  let sentValue: string | null = null;
  let sentAt = 0;

  // 续播是锦上添花：settings 读不到就当没有断点，**绝不能连累播放**。
  const ensureLoaded = () =>
    (loaded ??= api.settings
      .get()
      .then((r) => {
        Object.assign(cache, r.settings);
      })
      .catch(() => {}));

  function write(songId: number, positionMs: number, durationMs: number, force: boolean): void {
    const key = resumeKey(songId);
    const value = writeResumeMs(positionMs, durationMs);
    // 值跟上次写的一模一样（比如暂停后位置没动过）→ 白写
    if (!force && key === sentKey && value === sentValue) return;
    // 同一首、且刚写过 → 节流。keys 不同（换歌）不节流，否则会丢掉上一首的收尾位置。
    if (!force && key === sentKey && Date.now() - sentAt < SAVE_INTERVAL_MS) return;
    sentKey = key;
    sentValue = value;
    sentAt = Date.now();
    // 本地缓存跟着改，否则同一首歌内再次切回会读到旧值
    if (value === null) delete cache[key];
    else cache[key] = value;
    void api.settings.put({ [key]: value }).catch(() => {});
  }

  return {
    async get(songId) {
      await ensureLoaded();
      return readResumeMs(cache, songId);
    },
    save: (songId, positionMs, durationMs) => write(songId, positionMs, durationMs, false),
    saveNow: (songId, positionMs, durationMs) => write(songId, positionMs, durationMs, true),
  };
}
