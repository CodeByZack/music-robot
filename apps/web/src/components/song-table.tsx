import { useEffect, useState } from 'react';
import type { Song } from '@music-robot/core';
import Cover from '@/components/cover.tsx';
import { api } from '@/lib/client.ts';
import { usePlayer } from '@/lib/player.tsx';
import { MenuButton } from '@/components/menu.tsx';
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
 * 行尾的「⋯」菜单。收藏仍然是左边那颗心（一键可达，不该藏进菜单），
 * 菜单里放**其余**对这首歌的操作；目前只有「重新刮削」，以后加操作就往这里塞。
 *
 * 为什么不摆一排图标按钮：一列三颗小图标很吵，而且每加一个操作就再挤一颗；
 * 收进菜单后，操作还能带一句文字说清后果（会不会动原文件、失败原因是什么）。
 *
 * 状态只影响**菜单里的那一项**（文字 + 禁用），trigger 始终是「⋯」——
 * 按钮字形变来变去反而认不出是同一个东西。
 */
function RowMenu({ state, writeFiles, onRescrape }: {
  state: ScrapeState | undefined;
  writeFiles: boolean;
  onRescrape: () => void;
}) {
  const running = state?.phase === 'running';
  return (
    <MenuButton
      title="更多操作"
      header={
        state?.phase === 'err' ? (
          <p className="max-w-[230px] text-cap leading-4 text-accent">{state.message}</p>
        ) : undefined
      }
      items={[
        {
          label: running ? '刮削中…' : state?.phase === 'ok' ? '已重新刮削' : '重新刮削这首歌',
          // 「会不会动原文件」直接写在菜单里 —— 用户没理由记得住设置页那个开关当时开没开
          hint: writeFiles ? '会写文件' : '只入库',
          disabled: running,
          onClick: onRescrape,
        },
      ]}
    >
      <svg className="ico-sm" viewBox="0 0 16 16" fill="currentColor">
        <circle cx="3.4" cy="8" r="1.3" />
        <circle cx="8" cy="8" r="1.3" />
        <circle cx="12.6" cy="8" r="1.3" />
      </svg>
    </MenuButton>
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

  async function rescrape(song: Song) {
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
    return <p className="py-8 text-nav text-ink-3">这里还没有歌。</p>;
  }

  return (
    <table className="w-full border-collapse text-nav max-[900px]:[table-layout:fixed]">
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
          <th className="w-[62px] border-b border-line px-3 pb-[9px] text-right text-xs font-normal text-ink-4">
            时长
          </th>
          <th className="w-[60px] border-b border-line px-3 pb-[9px] text-left text-cap tracking-[.06em] text-ink-4 max-[1024px]:hidden">
            格式
          </th>
          {/* ⋯ 放**最后一列**（以前夹在 ♡ 与 时长 中间，把数据列切断了）。
              窄屏也保留 —— 它是那一行唯一的操作入口。 */}
          <th className="w-[38px] border-b border-line px-3 pb-[9px] text-left text-xs font-normal text-ink-4" />
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
                {/* 真封面（没内嵌封面时 Cover 自己退回 ♪ 占位）。
                    以前这里只是一个 ♪ / ⚠ 方块 —— 列表是音乐，应该有图。
                    「脏标签」徽标留在下面歌手那行，信息一点没丢。 */}
                <Cover
                  id={s.id}
                  className="size-[34px]"
                  rounded="rounded-md"
                  glyphClass="text-nav"
                />
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
                      <span className="ml-1.5 inline-flex h-[15px] items-center rounded-[3px] bg-accent-soft px-[5px] text-micro tracking-[.04em] text-accent">
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
                <svg className="ico-sm" viewBox="0 0 16 16" fill={fav.ids.has(s.id) ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth={1.4}>
                  <path d="M8 13.5S2.5 10.2 2.5 6.4A2.9 2.9 0 0 1 8 5a2.9 2.9 0 0 1 5.5 1.4c0 3.8-5.5 7.1-5.5 7.1z" />
                </svg>
              </button>
            </td>
            <td className="border-b border-line-weak px-3 text-right text-ink-3 tabular-nums">
              {mmss(s.duration_ms)}
            </td>
            <td className="border-b border-line-weak px-3 text-cap tracking-[.06em] text-ink-4 max-[1024px]:hidden">
              {s.format ?? '—'}
            </td>
            <td className="border-b border-line-weak px-3 text-center">
              <RowMenu
                state={scrape.get(s.id)}
                writeFiles={writeFiles}
                onRescrape={() => void rescrape(s)}
              />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
