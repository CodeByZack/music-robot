import { useEffect, useRef, useState } from 'react';
import type { Song } from '@music-robot/core';
import Cover from '@/components/cover.tsx';
import { api } from '@/lib/client.ts';
import { usePlayer } from '@/lib/player.tsx';
import { MenuButton, type MenuItem } from '@/components/menu.tsx';
import { useWriteFiles } from '@/lib/scrape-prefs.ts';
import { useSession } from '@/lib/session.tsx';

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
function RowMenu({ state, writeFiles, song, onRescrape, onRemove, onMoveUp, onMoveDown }: {
  state: ScrapeState | undefined;
  writeFiles: boolean;
  song: Song;
  onRescrape: () => void;
  /** 传了才显示「从歌单移除」——只有歌单详情页会传（见 SongTable 的 onRemove）。 */
  onRemove?: () => void;
  /** 传了才显示「上移」。第一行不传（已经是最上面）。 */
  onMoveUp?: () => void;
  /** 传了才显示「下移」。最后一行不传。 */
  onMoveDown?: () => void;
}) {
  const running = state?.phase === 'running';
  const { user } = useSession();
  /**
   * 写标签仅管理员可用（后端 `PATCH /api/songs/{id}/tags` 挂的是 `AdminUser`），
   * 所以非管理员**干脆不给这个入口** —— 点进去再收 403 是更差的体验。
   */
  const canEditTags = user?.role === 'admin';
  /**
   * 刚加进去的歌单 id —— 在二级列表里打「已添加」，不然点完没反馈。
   *
   * 用 **ref** 不用 state：二级列表是点开时拉的一次快照，重新拉的时候执行的还是
   * 当初那个闭包 —— 用 state 会永远读到旧值（踩过一次）。
   */
  const addedRef = useRef<number | null>(null);

  /**
   * 「添加到歌单」的二级列表。**每次点开拉一遍**（加完后也会重拉）——
   * 歌单会变，而且不该在菜单刚打开的瞬间就把这个请求打出去。
   */
  async function loadPlaylists(): Promise<MenuItem[]> {
    const res = await api.playlists.list();
    if (res.items.length === 0) {
      return [{ label: '还没有歌单', to: '/playlists', hint: '去建一个' }];
    }
    return res.items.map((pl) => {
      const already = addedRef.current === pl.id;
      return {
        label: pl.name,
        hint: already ? '已添加' : undefined,
        disabled: already,
        // 加完不关菜单，在这一项上打「已添加」—— 关了用户就不知道成没成
        keepOpen: true,
        onClick: () =>
          api.playlists
            .addSong(pl.id, song.id)
            .then(() => {
              // 后端对重复添加返回 200 {added:false}，不是错误，所以不用分支
              addedRef.current = pl.id;
            })
            .catch(() => {
              /* 失败就什么都不变，保持可再点一次 */
            }),
      };
    });
  }

  return (
    <MenuButton
      title="更多操作"
      header={
        state?.phase === 'err' ? (
          <p className="max-w-[230px] text-cap leading-4 text-accent">{state.message}</p>
        ) : undefined
      }
      items={[
        ...(canEditTags
          ? [{ label: '编辑标签', to: `/songs/${song.id}/tags` } satisfies MenuItem]
          : []),
        { label: '添加到歌单', submenu: loadPlaylists },
        {
          label: running ? '刮削中…' : state?.phase === 'ok' ? '已重新刮削' : '重新刮削这首歌',
          // 「会不会动原文件」直接写在菜单里 —— 用户没理由记得住设置页那个开关当时开没开
          hint: writeFiles ? '会写文件' : '只入库',
          disabled: running,
          onClick: onRescrape,
        },
        // 本行自己能动的位置（拖动排序的兜底入口）。顺序 = 表格里的上下，别写反。
        ...(onMoveUp ? ([{ label: '上移', onClick: onMoveUp }] satisfies MenuItem[]) : []),
        ...(onMoveDown ? ([{ label: '下移', onClick: onMoveDown }] satisfies MenuItem[]) : []),
        // 放在最后：它和上面几项不是一个类别（上面动的是**歌**，这一项动的是**这个歌单**）
        ...(onRemove
          ? ([{ label: '从歌单移除', danger: true, onClick: onRemove }] satisfies MenuItem[])
          : []),
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
  onRemove,
  onReorder,
}: {
  songs: Song[];
  showIndex?: boolean;
  /**
   * 「从某个集合里移除」的回调（歌单详情页传：把这首移出歌单）。
   *
   * 做成可选属性而不是另写一个表：曲目表的列、播放交互、脏标签标记**必须处处一致**
   * （见上面那段注释），为歌单页再抄一份是迟早漂移的那种重复。
   * 不传时这一项**根本不出现**，其余六个调用点的行为一字未变。
   */
  onRemove?: (song: Song) => void;
  /**
   * 拖动排序：把第 `from` 首挪到第 `to` 位（歌单详情页传）。
   *
   * 同样做成可选属性 —— 只有「顺序本身就是数据」的集合才该能拖。曲库 / 收藏 / 搜索
   * 这些页面的顺序是查询结果，拖它没有意义，所以它们**不传**，也就不会变成可拖的。
   *
   * 行的点击是「播放」：HTML5 拖拽在指针移动后不会触发 click，
   * 所以这两件事不冲突（但**触屏上 HTML5 拖拽根本不工作**，
   * 所以 ⋯ 菜单里另有「上移 / 下移」——见 RowMenu）。
   */
  onReorder?: (from: number, to: number) => void;
}) {
  const player = usePlayer();
  const playingId = player.song?.id ?? null;
  const fav = useFavorites();
  const writeFiles = useWriteFiles();

  const [scrape, setScrape] = useState<Map<number, ScrapeState>>(() => new Map());
  // 重刮后的新数据。页面传进来的 songs 是旧的，这里按 id 覆盖。
  const [patched, setPatched] = useState<Map<number, Song>>(() => new Map());
  /**
   * 拖动排序的进行态：正在拖第几行、当前悬停在第几行。
   *
   * 用 state 而不是纯 CSS：需要给「落点」画一条线，否则用户不知道会插到哪儿。
   */
  const [drag, setDrag] = useState<{ from: number; over: number } | null>(null);

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
            <th className="w-[38px] border-b border-line px-3 pb-3 text-left text-xs font-normal text-ink-4 max-[640px]:hidden">
              #
            </th>
          )}
          <th className="border-b border-line px-3 pb-3 text-left text-xs font-normal text-ink-4">
            歌曲 / 歌手
          </th>
          <th className="border-b border-line px-3 pb-3 text-left text-xs font-normal text-ink-4 max-[900px]:hidden">
            专辑
          </th>
          <th className="w-[42px] border-b border-line px-3 pb-3 text-left text-xs font-normal text-ink-4 max-[640px]:hidden" />
          <th className="w-[62px] border-b border-line px-3 pb-3 text-right text-xs font-normal text-ink-4">
            时长
          </th>
          <th className="w-[60px] border-b border-line px-3 pb-3 text-left text-cap tracking-[.06em] text-ink-4 max-[1024px]:hidden">
            格式
          </th>
          {/* ⋯ 放**最后一列**（以前夹在 ♡ 与 时长 中间，把数据列切断了）。
              窄屏也保留 —— 它是那一行唯一的操作入口。 */}
          <th className="w-[38px] border-b border-line px-3 pb-3 text-left text-xs font-normal text-ink-4" />
        </tr>
      </thead>
      <tbody>
        {rows.map((s, i) => (
          <tr
            key={`${s.id}-${i}`}
            onClick={() => player.playList(songs, i)}
            // 只有传了 onReorder 的行才可拖。加了 draggable 之后浏览器会给整行
            // 一个「可拖」的指针提示，所以不能无条件打开。
            draggable={onReorder ? true : undefined}
            onDragStart={
              onReorder
                ? (e) => {
                    // 拖整行；不设 dataTransfer 的话 Firefox 不认这次拖动。
                    e.dataTransfer.effectAllowed = 'move';
                    e.dataTransfer.setData('text/plain', String(i));
                    setDrag({ from: i, over: i });
                  }
                : undefined
            }
            onDragOver={
              onReorder
                ? (e) => {
                    // 必须 preventDefault，否则 drop 事件根本不会触发。
                    e.preventDefault();
                    e.dataTransfer.dropEffect = 'move';
                    if (drag && drag.over !== i) setDrag({ from: drag.from, over: i });
                  }
                : undefined
            }
            onDragEnd={onReorder ? () => setDrag(null) : undefined}
            onDrop={
              onReorder
                ? (e) => {
                    e.preventDefault();
                    const from = drag?.from;
                    setDrag(null);
                    if (from !== undefined && from !== i) onReorder(from, i);
                  }
                : undefined
            }
            className={[
              'h-14 cursor-pointer transition-colors hover:bg-surface-hover',
              i % 2 === 1 ? 'bg-white/[.017]' : '',
              playingId === s.id ? 'bg-accent-soft' : '',
              // 落点提示：拖到哪儿就在哪一行画上边线（往下拖时画在下边，符合直觉）。
              drag && drag.over === i && drag.from !== i
                ? drag.from > i
                  ? 'shadow-[inset_0_2px_0_var(--color-accent)]'
                  : 'shadow-[inset_0_-2px_0_var(--color-accent)]'
                : '',
              drag && drag.from === i ? 'opacity-40' : '',
            ].join(' ')}
          >
            {showIndex && (
              <td className="divider-row px-3 text-xs text-ink-4 tabular-nums max-[640px]:hidden">
                {String(i + 1).padStart(2, '0')}
              </td>
            )}
            <td className="overflow-hidden divider-row px-3">
              <div className="flex min-w-0 items-center gap-3">
                {/* 真封面（没内嵌封面时 Cover 自己退回 ♪ 占位）。
                    以前这里只是一个 ♪ / ⚠ 方块 —— 列表是音乐，应该有图。
                    「脏标签」徽标留在下面歌手那行，信息一点没丢。 */}
                <Cover
                  id={s.id}
                  className="size-10"
                  rounded="rounded-md"
                  glyphClass="text-lead"
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
                  <div className="overflow-hidden text-xs leading-5 text-ellipsis whitespace-nowrap text-ink-3">
                    {s.artists ?? '（未知歌手）'}
                    {isDirtyArtist(s.artists) && (
                      <span className="ml-1.5 inline-flex h-4 items-center rounded-xs bg-accent-soft px-1.5 text-micro tracking-[.04em] text-accent">
                        脏标签
                      </span>
                    )}
                  </div>
                </span>
              </div>
            </td>
            <td className="overflow-hidden divider-row px-3 text-ellipsis whitespace-nowrap text-ink-3 max-[900px]:hidden">
              {s.album ?? '—'}
            </td>
            <td className="divider-row px-3 text-center max-[640px]:hidden">
              <button
                onClick={(e) => void fav.toggle(s, e)}
                title={fav.ids.has(s.id) ? '取消收藏' : '收藏'}
                className={[
                  'inline-flex size-6 items-center justify-center rounded-xs transition-colors hover:bg-surface-hover',
                  fav.ids.has(s.id) ? 'text-accent' : 'text-ink-4',
                ].join(' ')}
              >
                <svg className="ico-sm" viewBox="0 0 16 16" fill={fav.ids.has(s.id) ? 'currentColor' : 'none'} stroke="currentColor" strokeWidth={1.4}>
                  <path d="M8 13.5S2.5 10.2 2.5 6.4A2.9 2.9 0 0 1 8 5a2.9 2.9 0 0 1 5.5 1.4c0 3.8-5.5 7.1-5.5 7.1z" />
                </svg>
              </button>
            </td>
            <td className="divider-row px-3 text-right text-ink-3 tabular-nums">
              {mmss(s.duration_ms)}
            </td>
            <td className="divider-row px-3 text-cap tracking-[.06em] text-ink-4 max-[1024px]:hidden">
              {s.format ?? '—'}
            </td>
            <td className="divider-row px-3 text-center">
              <RowMenu
                state={scrape.get(s.id)}
                writeFiles={writeFiles}
                song={s}
                onRescrape={() => void rescrape(s)}
                onRemove={onRemove ? () => onRemove(s) : undefined}
                /* 上移 / 下移是拖动排序的**无障碍与触屏兜底**：
                   HTML5 拖拽在触屏上完全不工作，键盘用户也拖不了。
                   两处调的是同一个 onReorder，不存在两套逻辑。 */
                onMoveUp={onReorder && i > 0 ? () => onReorder(i, i - 1) : undefined}
                onMoveDown={onReorder && i < rows.length - 1 ? () => onReorder(i, i + 1) : undefined}
              />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
