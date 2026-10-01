import { useEffect, useState } from 'react';
import type { Song } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { usePlayer } from '@/lib/player.tsx';
import { useWriteFiles } from '@/lib/scrape-prefs.ts';

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
 * 单曲重新刮削的状态机。刮削是**后台任务**（POST 起 job → 轮询 batch），
 * 所以每行自己记自己的状态，别用一个全局 spinner —— 同时点三行时那个 spinner 会骗人。
 */
type ScrapeState = { phase: 'running' } | { phase: 'ok' } | { phase: 'err'; message: string };

/** 轮询到任务离开 running 为止。单曲刮削通常 1~3 秒，超时给 60 秒兜底。 */
async function awaitScrape(batchId: string): Promise<{ failed: number; message: string | null }> {
  for (let i = 0; i < 80; i++) {
    const job = await api.jobs.scrape(batchId);
    if (job.status !== 'running') return { failed: job.failed, message: job.message };
    await new Promise((r) => setTimeout(r, 750));
  }
  return { failed: 1, message: '刮削超时（等了 60 秒还没结束）' };
}

/**
 * 单曲重刮那颗按钮。四态：待点 / 刮削中 / 成功 / 失败。
 *
 * 提示语必须写清楚**这一下会不会动原文件** —— 它跟着设置页那个开关走，
 * 用户没理由记得住开关当时是开是关。
 */
function ScrapeButton({
  state,
  writeFiles,
  onClick,
}: {
  state: ScrapeState | undefined;
  writeFiles: boolean;
  onClick: (e: React.MouseEvent) => void;
}) {
  const effect = writeFiles ? '会写回原文件（不可撤销）' : '只更新数据库，不碰文件';
  const title =
    state?.phase === 'running'
      ? '刮削中…'
      : state?.phase === 'ok'
        ? `刮削完成（${effect}）`
        : state?.phase === 'err'
          ? `刮削失败：${state.message}`
          : `重新刮削这首歌 · ${effect}`;

  return (
    <button
      onClick={onClick}
      disabled={state?.phase === 'running'}
      title={title}
      className={[
        'inline-flex size-6 items-center justify-center rounded transition-colors hover:bg-surface-hover',
        state?.phase === 'err' ? 'text-accent' : state?.phase === 'ok' ? 'text-ink-2' : 'text-ink-4',
        state?.phase === 'running' ? 'cursor-wait' : '',
      ].join(' ')}
    >
      {state?.phase === 'ok' ? (
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.7} strokeLinecap="round" strokeLinejoin="round">
          <path d="m3.5 8.4 3 3 6-6.4" />
        </svg>
      ) : state?.phase === 'err' ? (
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round">
          <path d="M8 2.6 14.2 13H1.8z" />
          <path d="M8 6.6v3M8 11.4v.1" />
        </svg>
      ) : (
        // 待点 / 刮削中共用同一个回转箭头，转起来就是「在跑」
        <svg
          width="14"
          height="14"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth={1.5}
          strokeLinecap="round"
          strokeLinejoin="round"
          className={state?.phase === 'running' ? 'animate-spin' : ''}
        >
          <path d="M13 8a5 5 0 1 1-1.6-3.7" />
          <path d="M13.4 2.6v3h-3" />
        </svg>
      )}
    </button>
  );
}

/**
 * 曲目表 —— 音乐库 / 收藏 / 歌单详情 / 歌手详情 / 首页都用它。
 *
 * 抽出来是因为**这些地方的列、交互、脏标签标记必须完全一致**；
 * 抄几遍迟早漂移。（也顺手把「点行播放」和「单曲重刮」这两件事收在一处。）
 *
 * 重刮成功后**只重取这一行**（见 patched）—— 不要求页面提供 reload 回调，
 * 六个调用点一个都不用改；而且「脏标签」徽标会当场消失，用户看得见效果。
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
  const writeFiles = useWriteFiles();

  const [scrape, setScrape] = useState<Map<number, ScrapeState>>(() => new Map());
  // 重刮后的新数据。页面传进来的 songs 是旧的，这里按 id 覆盖。
  const [patched, setPatched] = useState<Map<number, Song>>(() => new Map());

  async function rescrape(song: Song, e: React.MouseEvent) {
    e.stopPropagation(); // 别冒泡到行上 —— 那是「播放」
    setScrape((m) => new Map(m).set(song.id, { phase: 'running' }));
    try {
      // write_files 跟着设置页那个开关走（见 lib/scrape-prefs.ts），默认只入库。
      const acc = await api.jobs.startScrape({ song_ids: [song.id], write_files: writeFiles });
      const res = await awaitScrape(acc.batch_id);
      if (res.failed > 0) throw new Error(res.message ?? '刮削失败');
      setScrape((m) => new Map(m).set(song.id, { phase: 'ok' }));
      const fresh = await api.library.song(song.id);
      setPatched((m) => new Map(m).set(song.id, fresh.song));
    } catch (err) {
      setScrape((m) =>
        new Map(m).set(song.id, { phase: 'err', message: err instanceof Error ? err.message : String(err) }),
      );
    }
  }

  const rows = songs.map((s) => patched.get(s.id) ?? s);

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
          <th className="border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4 max-[900px]:hidden">
            专辑
          </th>
          <th className="w-[42px] border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4 max-[640px]:hidden" />
          <th className="w-[38px] border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4 max-[640px]:hidden" />
          <th className="w-[62px] border-b border-line px-3 pb-[9px] text-right text-xs font-normal text-ink-4">
            时长
          </th>
          <th className="w-[60px] border-b border-line px-3 pb-[9px] text-left text-[11px] tracking-[.06em] text-ink-4 max-[1024px]:hidden">
            格式
          </th>
        </tr>
      </thead>
      <tbody>
        {rows.map((s, i) => (
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
            <td className="overflow-hidden border-b border-line-weak px-3 text-ellipsis whitespace-nowrap text-ink-3 max-[900px]:hidden">
              {s.album ?? '—'}
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
            <td className="border-b border-line-weak px-3 text-center max-[640px]:hidden">
              <ScrapeButton
                state={scrape.get(s.id)}
                writeFiles={writeFiles}
                onClick={(e) => void rescrape(s, e)}
              />
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
