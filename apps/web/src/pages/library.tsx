import { useCallback, useState } from 'react';
import type { Page, Song } from '@music-robot/core';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

/**
 * 专辑列由 `SongTable` 提供 —— 后端现在在出口处补了 `album`（专辑名）。
 * 以前 `/api/library` 只给 `album_id`，所以那列是缺的。
 */
export default function LibraryPage() {
  const [query, setQuery] = useState('');
  const load = useCallback(() => api.library.list({ page_size: 200 }), []);
  const { data, error, loading } = useAsync<Page<Song>>(load);

  const songs = data?.items ?? [];
  const shown = query
    ? songs.filter((s) =>
        `${s.title ?? ''} ${s.artists ?? ''}`.toLowerCase().includes(query.toLowerCase()),
      )
    : songs;

  return (
    <>
      {/* 右边留出 54px 给外壳那颗设置齿轮（它绝对定位在右上角）——
          窄屏时搜索框是 flex-1，不留就会被齿轮压住右端。 */}
      <header className="flex h-[58px] shrink-0 items-center gap-3 px-[22px] pr-[54px] max-[900px]:px-[14px] max-[900px]:pr-[54px]">
        <div className="flex h-9 max-w-[480px] flex-1 items-center gap-[9px] rounded-full bg-surface px-[14px] text-[13px] text-ink-4">
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round">
            <circle cx="7" cy="7" r="4.6" />
            <path d="m10.6 10.6 3 3" />
          </svg>
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索歌曲 / 歌手"
            className="flex-1 border-0 bg-transparent text-ink outline-0 placeholder:text-ink-4"
          />
        </div>
      </header>

      <div className="flex-1 overflow-auto px-[22px] pt-1.5 pb-[130px] max-[1024px]:px-4 max-[640px]:px-3">
        <div className="flex items-end gap-[14px] pt-2.5 pb-5 max-[640px]:flex-wrap">
          <div>
            <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">
              音乐库
            </h1>
            <div className="mt-1 text-[13px] text-ink-3">
              {error ? '读取失败' : loading ? '读取中…' : query ? `筛选出 ${shown.length} 首` : `共 ${data?.total ?? 0} 首`}
            </div>
          </div>
        </div>

        {error ? (
          <ErrorNote
            message={error}
            hint={<>检查后端是否在 <code>127.0.0.1:8080</code>。</>}
          />
        ) : loading ? (
          <LoadingNote />
        ) : (
          <SongTable songs={shown} />
        )}
      </div>
    </>
  );
}
