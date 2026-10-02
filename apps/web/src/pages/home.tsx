import { useCallback } from 'react';
import { Link } from 'react-router';
import type { HistoryPage, Page, Song } from '@music-robot/core';
import Cover from '@/components/cover.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';
import { usePlayer } from '@/lib/player.tsx';
import { useSession } from '@/lib/session.tsx';

/**
 * 首页四张 hero 卡。**每张都有渐变**（以前只有「音乐库」一张是彩色的，
 * 其余三张是暗块 —— 节奏很怪）。渐变直接取飞牛的 `--ds-special-gradient-*`。
 */
const TILES = [
  {
    to: '/library',
    label: '音乐库',
    grad: 'linear-gradient(135deg,#ef7030,#f28d23)',
    // 层叠图标。以前是两根竖条，跟暂停键长得一模一样（见 shell.tsx 的 NAV）。
    d: 'M8 2.4 14.2 5.8 8 9.2 1.8 5.8z M2.6 9.6 8 12.5l5.4-2.9',
  },
  {
    to: '/albums',
    label: '专辑',
    grad: 'linear-gradient(135deg,#1a4d2e,#4f9d69)',
    d: 'M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM8 6.4a1.6 1.6 0 1 0 0 3.2 1.6 1.6 0 0 0 0-3.2z',
  },
  {
    to: '/artists',
    label: '歌手',
    grad: 'linear-gradient(135deg,#26356e,#4f6fd0)',
    d: 'M8 2.8a2.6 2.6 0 1 0 0 5.2 2.6 2.6 0 0 0 0-5.2zM3.2 13.4a4.9 4.9 0 0 1 9.6 0',
  },
  {
    to: '/favorites',
    label: '收藏',
    grad: 'linear-gradient(135deg,#8f2f5a,#d9557f)',
    d: 'M8 13.5S2.5 10.2 2.5 6.4A2.9 2.9 0 0 1 8 5a2.9 2.9 0 0 1 5.5 1.4c0 3.8-5.5 7.1-5.5 7.1z',
  },
];

/**
 * 歌曲网格（封面 + 曲名 + 歌手）。首页用网格而不是表格 —— 飞牛首页就是这么做的，
 * 表格留给「音乐库」那种要密集浏览的地方。
 */
function SongTiles({ songs }: { songs: Song[] }) {
  const player = usePlayer();
  return (
    <div className="grid grid-cols-2 gap-x-4 gap-y-5 min-[701px]:grid-cols-3 min-[901px]:grid-cols-4">
      {songs.map((s, i) => (
        <button
          key={`${s.id}-${i}`}
          type="button"
          onClick={() => player.playList(songs, i)}
          className="group min-w-0 text-left"
          title={`${s.title ?? '（无标题）'} — ${s.artists ?? '（未知歌手）'}`}
        >
          <div className="relative">
            <Cover
              id={s.id}
              className="aspect-square w-full transition-shadow group-hover:shadow-[0_8px_32px_rgba(0,0,0,.42)]"
              rounded="rounded-lg"
              glyphClass="text-3xl"
            />
            {/* 播放按钮：**居中** + 悬停才浮现（Spotify / Apple Music 的通行做法）。
                实心整块卡片本身就是播放按钮，所以这个圆钮只是「看得见的提示」，
                不是唯一入口 —— 触屏上直接点卡片就行，不依赖它。 */}
            <span className="pointer-events-none absolute inset-0 flex items-center justify-center">
              <span className="flex size-12 scale-90 items-center justify-center rounded-full bg-accent text-white opacity-0 shadow-[0_6px_20px_rgba(0,0,0,.55)] transition-all duration-200 group-hover:scale-100 group-hover:opacity-100">
                <svg className="ico-md" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M5 3.5v9L13 8z" />
                </svg>
              </span>
            </span>
          </div>
          <div className={['mt-2 overflow-hidden text-nav font-medium text-ellipsis whitespace-nowrap', player.song?.id === s.id ? 'text-accent' : 'group-hover:text-ink'].join(' ')}>
            {s.title ?? '（无标题）'}
          </div>
          <div className="overflow-hidden text-xs text-ellipsis whitespace-nowrap text-ink-3">
            {s.artists ?? '（未知歌手）'}
          </div>
        </button>
      ))}
    </div>
  );
}

export default function HomePage() {
  const { user } = useSession();
  // 最近添加：added_at 是默认排序且降序，直接就是它
  const recent = useCallback(() => api.library.list({ page_size: 12 }), []);
  // 最近播放
  const history = useCallback(() => api.history.list({ limit: 20 }), []);
  const rec = useAsync<Page<Song>>(recent);
  const hist = useAsync<HistoryPage>(history);
  // 后端在历史里已经带了曲目摘要（`song` 字段），不用再拉一遍库做 join。
  // 软删的曲目后端给 null，这里过滤掉。
  const recentPlayed = (hist.data?.items ?? [])
    .map((h) => h.song)
    .filter((s): s is Song => Boolean(s));

  return (
    <div className="flex-1 overflow-auto px-[22px] pt-2 pb-[130px] max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">
          {user ? `${user.username}，晚上好` : '晚上好'}
        </h1>
        <div className="mt-1 text-nav text-ink-3">
          {rec.data ? `共 ${rec.data.total} 首歌` : '读取中…'}
          {hist.data ? ` · 最近播放 ${hist.data.total} 次` : ''}
        </div>
      </div>

      {/* 四张渐变卡（飞牛同形）：窄屏 2 列，≥901px 4 列 */}
      <div className="mb-3 grid grid-cols-2 gap-4 min-[901px]:grid-cols-4">
        {TILES.map((t) => (
          <Link
            key={t.to}
            to={t.to}
            style={{ backgroundImage: t.grad }}
            className="flex h-[104px] flex-col items-start justify-between rounded-xl p-[14px] text-white transition-[filter] hover:brightness-110 min-[901px]:h-[138px]
                       max-[640px]:h-[92px] max-[640px]:p-3"
          >
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinecap="round" strokeLinejoin="round" className="ico-lg opacity-90">
              <path d={t.d} />
            </svg>
            <b className="text-lead font-medium tracking-[.02em]">{t.label}</b>
          </Link>
        ))}
      </div>

      {rec.error && <ErrorNote message={rec.error} />}

      <h2 className="mt-7 mb-3.5 text-base leading-6 font-medium">最近播放</h2>
      {hist.loading ? (
        <LoadingNote />
      ) : recentPlayed.length === 0 ? (
        <p className="py-6 text-nav text-ink-3">
          还没有播放记录。{hist.data?.total ? '（有记录但曲目已不在库里）' : '去音乐库点一首试试。'}
        </p>
      ) : (
        <SongTiles songs={recentPlayed.slice(0, 12)} />
      )}

      <h2 className="mt-7 mb-3.5 text-base leading-6 font-medium">最近添加</h2>
      {rec.loading ? <LoadingNote /> : <SongTiles songs={(rec.data?.items ?? []).slice(0, 12)} />}
    </div>
  );
}
