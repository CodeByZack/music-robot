import { useCallback } from 'react';
import type { Page, Song } from '@music-robot/core';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

/**
 * 曲库列表。专辑列由 `SongTable` 提供 —— 后端现在在出口处补了 `album`（专辑名）。
 * 以前 `/api/library` 只给 `album_id`，所以那列是缺的。
 *
 * **搜索已经提到顶栏（全局）**，这一页不再自带搜索框 —— 两个搜索框更容易让人困惑，
 * 而且页面内那个只能过滤已加载的 200 首。
 */
export default function LibraryPage() {
  const load = useCallback(() => api.library.list({ page_size: 200 }), []);
  const { data, error, loading } = useAsync<Page<Song>>(load);

  return (
    <div className="flex-1 overflow-auto px-5 pt-1.5 pb-32 max-[1024px]:px-4 max-[640px]:px-3">
      <div className="flex items-end gap-4 pt-2.5 pb-5 max-[640px]:flex-wrap">
        <div>
          <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">
            音乐库
          </h1>
          <div className="mt-1 text-nav text-ink-3">
            {error ? '读取失败' : loading ? '读取中…' : `共 ${data?.total ?? 0} 首`}
          </div>
        </div>
      </div>

      {error ? (
        <ErrorNote message={error} hint={<>检查后端是否在 <code>127.0.0.1:8080</code>。</>} />
      ) : loading ? (
        <LoadingNote />
      ) : (
        <SongTable songs={data?.items ?? []} />
      )}
    </div>
  );
}
