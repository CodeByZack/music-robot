/**
 * 「刮削要不要写回原文件」这一档，全局只有一份：**设置页那个开关和曲目表里每行
 * 「重新刮削」读的是同一个值**。
 *
 * 为什么不让两处各存各的：同一个动作在两处语义不同的话，用户在设置页打开了写入、
 * 回到列表点重刮却发现没写（或者反过来，以为只入库结果覆盖了原文件）—— 这种不一致
 * 没法跟用户解释。而且刮削不可撤销，**默认必须是安全的那一档**。
 *
 * 内存级，刷新回默认 false —— 跟设置页原来的行为一致（那个 useState 本来也不持久化）。
 */

import { useSyncExternalStore } from 'react';

let writeFiles = false;
const subscribers = new Set<() => void>();

export function getWriteFiles(): boolean {
  return writeFiles;
}

/** 设置页那个开关调它。真正的写文件由后端 `POST /api/scrape` 的 `write_files` 决定。 */
export function setWriteFiles(next: boolean): void {
  if (next === writeFiles) return;
  writeFiles = next;
  for (const fn of subscribers) fn();
}

function subscribe(fn: () => void): () => void {
  subscribers.add(fn);
  return () => {
    subscribers.delete(fn);
  };
}

/** 任何要跟着这一档变的地方都用它读。 */
export function useWriteFiles(): boolean {
  return useSyncExternalStore(subscribe, getWriteFiles, getWriteFiles);
}
