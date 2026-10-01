import { useEffect, useState } from 'react';
import type { Song } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { usePlayer } from '@/lib/player.tsx';

/** 毫秒 → `3:58`。 */
export function mmss(ms: number | null | undefined): string {
  if (!ms || ms <= 0) return '—';
  const total = Math.round(ms / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`;
}

/** `公众号：阿乐资源库` 这种被上传者塞了广告的 artist —— 界面要能一眼看出来。 */
export function isDirtyArtist(v: string | null | undefined): boolean {
  if (!v) return false;
  return /公众号|音乐下载|yym\d|加微信|QQ群|www\.|\.com/i.test(v);
}

/**
 * 收藏集合。**模块级缓存**：一个会话只拉一次，避免每个页面/每个表格挂载都打一次。
 * 变动时本地先改（乐观），失败再回滚 —— 心形点了要立刻有反应。
 */
let favCache: Set<number> | null = null;
let favInflight: Promise<Set<number>> | null = null;

function useFavorites() {
  const [ids, setIds] = useState<Set<number>>(favCache ?? new Set());
  useEffect(() => {
    if (favCache) return;
    // 每次挂载都重新拉一次（而不是永远用缓存）：收藏页刚点完心形切过来要是旧的就很怪
    favInflight = api.favorites.list().then((r) => new Set(r.items.map((s) => s.id)));
    let alive = true;
    favInflight
      .then((set) => {
        favCache = set;
        if (alive) setIds(new Set(set));
      })
      .catch(() => {
        /* 收藏读不到不影响听歌 */
      });
    return () => {
      alive = false;
    };
  }, []);

  async function toggle(song: Song, e: React.MouseEvent) {
    // 别让点击冒泡到行上 —— 那是「播放」
    e.stopPropagation();
    const next = new Set(favCache ?? ids);
    const on = next.has(song.id);
    if (on) next.delete(song.id);
    else next.add(song.id);
    favCache = next;
    setIds(new Set(next));
    try {
      if (on) await api.favorites.remove(song.id);
      else await api.favorites.add(song.id);
    } catch {
      // 回滚：服务端没成功，界面上就不该显示成功
      const back = new Set(favCache ?? next);
      if (on) back.add(song.id);
      else back.delete(song.id);
      favCache = back;
      setIds(new Set(back));
    }
  }
  return { ids, toggle };
}

/**
 * 曲目表 —— 音乐库 / 收藏 / 歌单详情 / 歌手详情都用它。
 *
 * 抽出来是因为**四处的列、交互、脏标签标记必须完全一致**；
 * 抄四遍迟早漂移。（也顺手把「点行播放」这件事收在一处。）
 */
export default function SongTable({
  songs,
  showIndex = true,
}: {
  songs: Song[];
  showIndex?: boolean;
}) {
  const player = usePlayer();
  const playingId = player.song?.id ?? null;
  const fav = useFavorites();

  if (songs.length === 0) {
    return <p className="py-8 text-[13px] text-ink-3">这里还没有歌。</p>;
  }

  return (
    <table className="w-full border-collapse text-[13.5px] max-[900px]:[table-layout:fixed]">
      <thead>
        <tr>
          {showIndex && (
            <th className="w-[38px] border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4 max-[640px]:hidden">
              #
            </th>
          )}
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
        {songs.map((s, i) => (
          <tr
            key={`${s.id}-${i}`}
            onClick={() => player.playList(songs, i)}
            className={[
              'h-[46px] cursor-pointer transition-colors hover:bg-surface-hover',
              i % 2 === 1 ? 'bg-white/[.017]' : '',
              playingId === s.id ? 'bg-accent-soft' : '',
            ].join(' ')}
          >
            {showIndex && (
              <td className="border-b border-line-weak px-3 text-xs text-ink-4 tabular-nums max-[640px]:hidden">
                {String(i + 1).padStart(2, '0')}
              </td>
            )}
            <td className="overflow-hidden border-b border-line-weak px-3">
              <div className="flex min-w-0 items-center gap-[11px]">
                <span className="flex size-[30px] shrink-0 items-center justify-center rounded-sm bg-surface text-xs text-ink-4">
                  {isDirtyArtist(s.artists) ? '⚠' : '♪'}
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
                    {isDirtyArtist(s.artists) && (
                      <span className="ml-1.5 inline-flex h-[15px] items-center rounded-[3px] bg-accent-soft px-[5px] text-[10px] tracking-[.04em] text-accent">
                        脏标签
                      </span>
                    )}
                  </div>
                </span>
              </div>
            </td>
            <td className="border-b border-line-weak px-3 text-center max-[640px]:hidden">
              <button
                onClick={(e) => void fav.toggle(s, e)}
                title={fav.ids.has(s.id) ? '取消收藏' : '收藏'}
                className={[
                  'inline-flex size-6 items-center justify-center rounded transition-colors hover:bg-surface-hover',
                  fav.ids.has(s.id) ? 'text-accent' : 'text-ink-4',
                ].join(' ')}
              >
                <svg width="14" height="14" viewBox="0 0 16 16" fill={fav.ids.has(s.id) ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth={1.4}>
                  <path d="M8 13.5S2.5 10.2 2.5 6.4A2.9 2.9 0 0 1 8 5a2.9 2.9 0 0 1 5.5 1.4c0 3.8-5.5 7.1-5.5 7.1z" />
                </svg>
              </button>
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
  );
}
