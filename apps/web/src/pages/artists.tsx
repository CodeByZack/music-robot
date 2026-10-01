import { useCallback } from 'react';
import { Link } from 'react-router';
import type { ArtistRow, Page } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';
import { isDirtyArtist } from '@/components/song-table.tsx';

export default function ArtistsPage() {
  const load = useCallback(() => api.artists.list({ page_size: 200 }), []);
  const { data, error, loading } = useAsync<Page<ArtistRow>>(load);

  return (
    <div className="flex-1 overflow-auto px-[22px] pt-2 pb-[130px] max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">歌手</h1>
        <div className="mt-1 text-[13px] text-ink-3">
          {error ? '读取失败' : loading ? '读取中…' : `${data?.total ?? 0} 位`}
        </div>
      </div>

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : (
        <table className="w-full border-collapse text-[13.5px]">
          <thead>
            <tr>
              <th className="w-[46px] border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4" />
              <th className="border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4">歌手</th>
              <th className="w-[90px] border-b border-line px-3 pb-[9px] text-right text-xs font-normal text-ink-4">
                专辑
              </th>
              <th className="w-[90px] border-b border-line px-3 pb-[9px] text-right text-xs font-normal text-ink-4">
                歌曲
              </th>
            </tr>
          </thead>
          <tbody>
            {(data?.items ?? []).map((a, i) => (
              <tr
                key={a.name}
                className={['h-[46px] transition-colors hover:bg-surface-hover', i % 2 === 1 ? 'bg-white/[.017]' : ''].join(' ')}
              >
                <td className="border-b border-line-weak px-3">
                  <Link
                    to={`/artists/${encodeURIComponent(a.name)}`}
                    className="flex size-[30px] items-center justify-center rounded-full bg-surface text-xs text-ink-4"
                  >
                    {isDirtyArtist(a.name) ? '⚠' : '人'}
                  </Link>
                </td>
                <td className="overflow-hidden border-b border-line-weak px-3">
                  <Link to={`/artists/${encodeURIComponent(a.name)}`} className="block min-w-0">
                    <div className="overflow-hidden font-medium text-ellipsis whitespace-nowrap">{a.name}</div>
                    {isDirtyArtist(a.name) && (
                      <div className="text-xs leading-4 text-ink-3">
                        <span className="inline-flex h-[15px] items-center rounded-[3px] bg-accent-soft px-[5px] text-[10px] tracking-[.04em] text-accent">
                          脏标签
                        </span>
                      </div>
                    )}
                  </Link>
                </td>
                <td className="border-b border-line-weak px-3 text-right text-ink-3 tabular-nums">{a.album_count}</td>
                <td className="border-b border-line-weak px-3 text-right text-ink-3 tabular-nums">{a.song_count}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
