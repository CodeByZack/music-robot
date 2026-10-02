import { useCallback } from 'react';
import { Link, useParams } from 'react-router';
import type { Album, Song } from '@music-robot/core';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

export default function AlbumPage() {
  const { id } = useParams<{ id: string }>();
  const aid = Number(id);
  const load = useCallback(() => api.albums.get(aid), [aid]);
  const { data, error, loading } = useAsync<{ album: Album; songs: Song[] }>(load);

  return (
    <div className="flex-1 overflow-auto px-[22px] pt-2 pb-[130px] max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <Link to="/albums" className="text-nav text-ink-3 hover:text-ink">
          ← 专辑
        </Link>
        <h1 className="mt-2 text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">
          {data?.album.name ?? (loading ? '读取中…' : '专辑')}
        </h1>
        <div className="mt-1 text-nav text-ink-3">
          {data?.album.album_artist ?? ''}
          {data?.album.year ? ` · ${data.album.year}` : ''}
          {data?.songs?.length ? ` · ${data.songs.length} 首` : ''}
        </div>
      </div>

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : (
        <SongTable songs={data?.songs ?? []} />
      )}
    </div>
  );
}
