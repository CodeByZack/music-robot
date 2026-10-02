import { useCallback } from 'react';
import { Link, useParams } from 'react-router';
import type { PlaylistDetail } from '@music-robot/core';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

export default function PlaylistPage() {
  const { id } = useParams<{ id: string }>();
  const pid = Number(id);
  const load = useCallback(() => api.playlists.get(pid), [pid]);
  const { data, error, loading } = useAsync<PlaylistDetail>(load);

  // ⚠️ 后端返回的是 { playlist, songs } —— 歌单与曲目**并列**，不是一个扁平对象
  const playlist = data?.playlist;
  const songs = data?.songs ?? [];

  return (
    <div className="flex-1 overflow-auto px-5 pt-2 pb-32 max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <Link to="/playlists" className="text-nav text-ink-3 hover:text-ink">
          ← 歌单
        </Link>
        <h1 className="mt-2 text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">
          {playlist?.name ?? (loading ? '读取中…' : '歌单')}
        </h1>
        <div className="mt-1 text-nav text-ink-3">
          {playlist?.description || (playlist ? (playlist.is_public ? '公开' : '私有') : '')}
          {songs.length > 0 && ` · ${songs.length} 首`}
        </div>
      </div>

      {error ? <ErrorNote message={error} /> : loading ? <LoadingNote /> : <SongTable songs={songs} />}
    </div>
  );
}
