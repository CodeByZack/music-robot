import { useEffect, useState } from 'react';
import type { Song } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { usePlayer } from '@/lib/player.tsx';
import { messageOf } from '@/lib/session.tsx';

/** 毫秒 → `3:58`。 */
function mmss(ms: number | null): string {
  if (!ms || ms <= 0) return '—';
  const total = Math.round(ms / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`;
}

/** `公众号：阿乐资源库` 这种被上传者塞了广告的 artist —— 界面要能一眼看出来。 */
function isDirty(v: string | null): boolean {
  if (!v) return false;
  return /公众号|音乐下载|yym\d|加微信|QQ群|www\.|\.com/i.test(v);
}

/**
 * ⚠️ 这里**没有「专辑」列**，故意的。
 * `/api/library` 的曲目 JSON 只给 `album_id`，**不给专辑名**（见后端 `song_json`）。
 * 要显示得二选一：① 再拉一次 `/api/albums` 在客户端 join；② 后端加上专辑名。
 * 在没决定之前，宁可不显示，也不摆一列 `—` 占位。
 */
export default function LibraryPage() {
  const [songs, setSongs] = useState<Song[] | null>(null);
  const [total, setTotal] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const player = usePlayer();
  const playingId = player.song?.id ?? null;

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const page = await api.library.list({ page_size: 100 });
        if (!alive) return;
        setSongs(page.items);
        setTotal(page.total);
      } catch (e) {
        if (alive) setError(messageOf(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  const shown = (songs ?? []).filter((s) => {
    if (!query) return true;
    const q = query.toLowerCase();
    return `${s.title ?? ''} ${s.artists ?? ''}`.toLowerCase().includes(q);
  });

  return (
    <>
      <header className="flex h-[58px] shrink-0 items-center gap-3 px-[22px] max-[900px]:px-[14px]">
        <div className="flex h-9 max-w-[480px] flex-1 items-center gap-[9px] rounded-full bg-surface px-[14px] text-[13px] text-ink-4">
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round">
            <circle cx="7" cy="7" r="4.6" />
            <path d="m10.6 10.6 3 3" />
          </svg>
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索歌曲 / 歌手 / 专辑"
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
              {error
                ? '读取失败'
                : query
                  ? `筛选出 ${shown.length} 首`
                  : songs
                    ? `共 ${total} 首`
                    : '读取中…'}
            </div>
          </div>
        </div>

        {error ? (
          <div className="rounded-lg bg-surface p-5 text-[13px] text-ink-2">
            {error}
            <div className="mt-2 text-xs text-ink-4">
              检查后端是否在 <code>127.0.0.1:8080</code>，或设 <code>MR_API_TARGET</code> 指到别处。
            </div>
          </div>
        ) : (
          <table className="w-full border-collapse text-[13.5px] [table-layout:fixed] max-[900px]:[table-layout:fixed]">
            <thead>
              <tr>
                <th className="w-[38px] border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4 max-[640px]:hidden">
                  #
                </th>
                <th className="border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4">
                  歌曲 / 歌手
                </th>
                <th className="w-[42px] border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4 max-[640px]:hidden" />
                <th className="w-[62px] border-b border-line px-3 pb-[9px] text-right text-xs font-normal text-ink-4">
                  时长
                </th>
                <th className="w-[60px] border-b border-line px-3 pb-[9px] text-left text-[11px] tracking-[.06em] text-ink-4 max-[1024px]:hidden">
                  格式
                </th>
              </tr>
            </thead>
            <tbody>
              {shown.map((s, i) => (
                <tr
                  key={s.id}
                  onClick={() => player.playList(shown, i)}
                  className={[
                    'h-[46px] cursor-pointer transition-colors hover:bg-surface-hover',
                    i % 2 === 1 ? 'bg-white/[.017]' : '',
                    playingId === s.id ? 'bg-accent-soft' : '',
                  ].join(' ')}
                >
                  <td className="border-b border-line-weak px-3 text-xs text-ink-4 tabular-nums max-[640px]:hidden">
                    {String(i + 1).padStart(2, '0')}
                  </td>
                  <td className="overflow-hidden border-b border-line-weak px-3">
                    <div className="flex min-w-0 items-center gap-[11px]">
                      <span className="flex size-[30px] shrink-0 items-center justify-center rounded-sm bg-surface text-xs text-ink-4">
                        {isDirty(s.artists) ? '⚠' : '♪'}
                      </span>
                      <span className="min-w-0">
                        <div
                          className={[
                            'overflow-hidden font-medium text-ellipsis whitespace-nowrap',
                            playingId === s.id ? 'text-accent' : '',
                          ].join(' ')}
                        >
                          {s.title ?? '（无标题）'}
                        </div>
                        <div className="overflow-hidden text-xs leading-4 text-ellipsis whitespace-nowrap text-ink-3">
                          {s.artists ?? '（未知歌手）'}
                          {isDirty(s.artists) && (
                            <span className="ml-1.5 inline-flex h-[15px] items-center rounded-[3px] bg-accent-soft px-[5px] text-[10px] tracking-[.04em] text-accent">
                              脏标签
                            </span>
                          )}
                        </div>
                      </span>
                    </div>
                  </td>
                  <td className="border-b border-line-weak px-3 text-center text-ink-4 max-[640px]:hidden">
                    ♡
                  </td>
                  <td className="border-b border-line-weak px-3 text-right text-ink-3 tabular-nums">
                    {mmss(s.duration_ms)}
                  </td>
                  <td className="border-b border-line-weak px-3 text-[11px] tracking-[.06em] text-ink-4 max-[1024px]:hidden">
                    {s.format ?? '—'}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </>
  );
}
