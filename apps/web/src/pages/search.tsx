import { useCallback } from 'react';
import { useSearchParams } from 'react-router';
import type { Page, Song } from '@music-robot/core';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

/**
 * 全局搜索页。入口是**顶栏那个搜索胶囊**（回车跳这里）。
 *
 * 走的是后端 `/api/search`，不是拿当前列表做本地过滤 —— 搜的是整个曲库。
 * （以前搜索框在音乐库页自己的 header 里，只能过滤已加载的那 200 首。）
 */
export default function SearchPage() {
  const [params] = useSearchParams();
  const q = (params.get('q') ?? '').trim();
  const load = useCallback(
    () => (q ? api.search(q, { page_size: 200 }) : Promise.resolve(null)),
    [q],
  );
  const { data, error, loading } = useAsync<Page<Song> | null>(load, [q]);

  return (
    <div className="flex-1 overflow-auto px-[22px] pt-1.5 pb-[130px] max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">搜索</h1>
        <div className="mt-1 text-[13px] text-ink-3">
          {!q
            ? '在顶栏的搜索框里输入关键词'
            : loading
              ? '搜索中…'
              : error
                ? '搜索失败'
                : `“${q}” · ${data?.total ?? 0} 首`}
        </div>
      </div>

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : !q ? null : (data?.items.length ?? 0) === 0 ? (
        <p className="py-6 text-[13px] text-ink-3">没有找到匹配的歌曲。</p>
      ) : (
        <SongTable songs={data?.items ?? []} />
      )}
    </div>
  );
}
