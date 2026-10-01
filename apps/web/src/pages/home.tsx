import { useCallback } from 'react';
import { Link } from 'react-router';
import type { HistoryPage, Page, Song } from '@music-robot/core';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';
import { useSession } from '@/lib/session.tsx';

const TILES = [
  { to: '/library', label: '音乐库', hero: true, d: 'M2.5 7 8 2.5 13.5 7v6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z' },
  { to: '/favorites', label: '收藏', d: 'M8 13.5S2.5 10.2 2.5 6.4A2.9 2.9 0 0 1 8 5a2.9 2.9 0 0 1 5.5 1.4c0 3.8-5.5 7.1-5.5 7.1z' },
  { to: '/playlists', label: '歌单', d: 'M2 4h12M2 8h12M2 12h7' },
  { to: '/settings', label: '设置', d: 'M8 2.5a5.5 5.5 0 1 0 0 11 5.5 5.5 0 0 0 0-11zM8 5v3.2l2.2 1.4' },
];

function hoursAgo(ms: number): string {
  const h = Math.round((Date.now() - ms) / 3_600_000);
  if (h < 1) return '刚刚';
  if (h < 24) return `${h} 小时前`;
  return `${Math.round(h / 24)} 天前`;
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
        <div className="mt-1 text-[13px] text-ink-3">
          {rec.data ? `共 ${rec.data.total} 首歌` : '读取中…'}
          {hist.data ? ` · 最近播放 ${hist.data.total} 次` : ''}
        </div>
      </div>

      <div className="mb-2 grid gap-4 [grid-template-columns:repeat(auto-fit,minmax(150px,1fr))]">
        {TILES.map((t) => (
          <Link
            key={t.to}
            to={t.to}
            className={[
              'flex h-[104px] flex-col items-start justify-between rounded-lg p-[14px] transition-colors',
              t.hero
                ? 'bg-[linear-gradient(135deg,#f0763a,#c934e1_55%,#5b4bd6)]'
                : 'bg-surface hover:bg-surface-hover',
            ].join(' ')}
          >
            <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinecap="round" strokeLinejoin="round">
              <path d={t.d} />
            </svg>
            <b className="text-[15px] font-medium">{t.label}</b>
          </Link>
        ))}
      </div>

      {rec.error && <ErrorNote message={rec.error} />}

      <h2 className="mt-7 mb-3 text-base leading-6 font-medium">最近添加</h2>
      {rec.loading ? <LoadingNote /> : <SongTable songs={(rec.data?.items ?? []).slice(0, 6)} />}

      <h2 className="mt-7 mb-3 text-base leading-6 font-medium">最近播放</h2>
      {hist.loading ? (
        <LoadingNote />
      ) : recentPlayed.length === 0 ? (
        <p className="py-6 text-[13px] text-ink-3">
          还没有播放记录。{hist.data?.total ? '（有记录但曲目已不在库里）' : '去音乐库点一首试试。'}
        </p>
      ) : (
        <>
          <SongTable songs={recentPlayed} />
          <div className="mt-3 space-y-1 text-xs text-ink-4">
            {(hist.data?.items ?? []).slice(0, 3).map((h) => (
              <div key={h.id}>
                {h.song?.title ?? `曲目 ${h.song_id}`} · {hoursAgo(h.played_at)}
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
