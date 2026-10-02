import { useCallback } from 'react';
import { Link } from 'react-router';
import type { Song } from '@music-robot/core';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

export default function FavoritesPage() {
  const load = useCallback(() => api.favorites.list(), []);
  const { data, error, loading } = useAsync<{ items: Song[]; total: number }>(load);

  return (
    <div className="flex-1 overflow-auto px-5 pt-2 pb-32 max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">收藏</h1>
        <div className="mt-1 text-nav text-ink-3">
          {error ? '读取失败' : loading ? '读取中…' : `${data?.total ?? 0} 首`}
        </div>
      </div>

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : (data?.items ?? []).length === 0 ? (
        <p className="py-6 text-nav text-ink-3">
          还没有收藏。去<Link to="/library" className="text-accent">音乐库</Link>点心形标记。
        </p>
      ) : (
        <SongTable songs={data?.items ?? []} />
      )}
    </div>
  );
}
