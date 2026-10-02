import { useCallback } from 'react';
import { Link } from 'react-router';
import type { AlbumSummary, Page } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

export default function AlbumsPage() {
  const load = useCallback(() => api.albums.list({ page_size: 200 }), []);
  const { data, error, loading } = useAsync<Page<AlbumSummary>>(load);

  return (
    <div className="flex-1 overflow-auto px-[22px] pt-2 pb-[130px] max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">专辑</h1>
        <div className="mt-1 text-nav text-ink-3">
          {error ? '读取失败' : loading ? '读取中…' : `${data?.total ?? 0} 张`}
        </div>
      </div>

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : (data?.items ?? []).length === 0 ? (
        <p className="py-6 text-nav text-ink-3">还没有专辑。先扫描一次曲库。</p>
      ) : (
        <div className="grid gap-4 [grid-template-columns:repeat(auto-fill,minmax(132px,1fr))]">
          {(data?.items ?? []).map((a) => (
            <Link key={a.id} to={`/albums/${a.id}`} className="rounded-lg p-2.5 transition-colors hover:bg-surface">
              <div className="flex aspect-square items-center justify-center rounded-md bg-surface text-display text-ink-4">
                ♪
              </div>
              <div className="mt-[9px] overflow-hidden text-nav font-medium text-ellipsis whitespace-nowrap">
                {a.name}
              </div>
              <div className="overflow-hidden text-xs leading-4 text-ellipsis whitespace-nowrap text-ink-3">
                {a.album_artist || '未知艺术家'}
                {a.year ? ` · ${a.year}` : ''}
                {` · ${a.song_count} 首`}
              </div>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
