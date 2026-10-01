import { useCallback } from 'react';
import { Link, useParams } from 'react-router';
import type { AlbumSummary, Song } from '@music-robot/core';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

export default function ArtistPage() {
  const { name = '' } = useParams<{ name: string }>();
  const load = useCallback(() => api.artists.get(name), [name]);
  const { data, error, loading } = useAsync<{
    artist: string;
    songs: Song[];
    albums: AlbumSummary[];
  }>(load);

  return (
    <div className="flex-1 overflow-auto px-[22px] pt-2 pb-[130px] max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <Link to="/artists" className="text-[13px] text-ink-3 hover:text-ink">
          ← 歌手
        </Link>
        <h1 className="mt-2 text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">
          {data?.artist ?? (loading ? '读取中…' : name)}
        </h1>
        <div className="mt-1 text-[13px] text-ink-3">
          {data ? `${data.songs.length} 首 · ${data.albums.length} 张专辑` : ''}
        </div>
      </div>

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : (
        <>
          {(data?.albums ?? []).length > 0 && (
            <div className="mb-6 grid gap-4 [grid-template-columns:repeat(auto-fill,minmax(132px,1fr))]">
              {(data?.albums ?? []).map((a) => (
                <Link key={a.id} to={`/albums/${a.id}`} className="rounded-lg p-2.5 transition-colors hover:bg-surface">
                  <div className="flex aspect-square items-center justify-center rounded-md bg-surface text-[22px] text-ink-4">
                    ♪
                  </div>
                  <div className="mt-[9px] overflow-hidden text-[13.5px] font-medium text-ellipsis whitespace-nowrap">
                    {a.name}
                  </div>
                  <div className="text-xs leading-4 text-ink-3">{a.song_count} 首</div>
                </Link>
              ))}
            </div>
          )}
          <SongTable songs={data?.songs ?? []} />
        </>
      )}
    </div>
  );
}
