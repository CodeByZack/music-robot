import { useCallback, useState } from 'react';
import type { Song } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { usePlayer } from '@/lib/player.tsx';
import { useAsync } from '@/lib/use-async.tsx';

type SongWithLyrics = Song & { lyrics?: string | null };

function clock(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '0:00';
  const t = Math.floor(ms / 1000);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, '0')}`;
}

function IconBtn({
  onClick,
  title,
  on,
  children,
}: {
  onClick?: () => void;
  title: string;
  on?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className={[
        'flex size-10 items-center justify-center rounded-full transition-colors hover:bg-surface-hover',
        on ? 'text-accent' : 'text-ink-2 hover:text-ink',
      ].join(' ')}
    >
      {children}
    </button>
  );
}

export default function NowPage() {
  const p = usePlayer();
  const [tab, setTab] = useState<'lyr' | 'queue'>('lyr');
  /**
   * 窄屏（<900px）下封面和歌词**不能同时铺开** —— 竖着堆要滚很久，
   * 而且封面巨大、歌词只剩一条缝。所以窄屏只显示一栏，顶上给个切换。
   * 桌面两栏并排，这个状态不起作用。
   */
  const [pane, setPane] = useState<'cover' | 'lyrics'>('cover');
  const songId = p.song?.id;
  const load = useCallback(
    () => (songId ? api.library.song(songId) : Promise.resolve(null)),
    [songId],
  );
  const { data } = useAsync<{ song: SongWithLyrics } | null>(load, [songId]);

  if (!p.song) {
    return (
      <div className="grid flex-1 place-items-center text-[13px] text-ink-3">
        还没有在放的歌。去音乐库点一首。
      </div>
    );
  }

  const lyrics = (data?.song.lyrics ?? '').split('\n').filter((l) => l.trim());
  const pct = p.durationMs > 0 ? Math.min(100, (p.positionMs / p.durationMs) * 100) : 0;

  return (
    <div className="relative flex-1 overflow-hidden">
      {/* 封面取色的氛围背景。⚠️ 封面是占位，所以这层是手挑的紫 —— 接上真封面后应从图里提色 */}
      <div className="pointer-events-none absolute -inset-[120px] z-0 bg-[radial-gradient(60%_55%_at_26%_34%,#7b4bd0_0%,transparent_62%),radial-gradient(50%_50%_at_62%_18%,#c934e1_0%,transparent_60%),radial-gradient(45%_45%_at_40%_70%,#ef6b3c_0%,transparent_62%)] opacity-50 blur-[110px] saturate-150" />

      <div className="relative z-10 flex h-full flex-col gap-6 overflow-auto p-6 min-[901px]:flex-row min-[901px]:items-center min-[901px]:justify-center min-[901px]:gap-11 min-[901px]:p-[28px_38px_34px]">
        {/* 窄屏的封面 / 歌词 切换。桌面两栏都在，所以整块藏掉。 */}
        <div className="flex shrink-0 gap-1 self-center rounded-full bg-black/25 p-1 min-[901px]:hidden">
          {(['cover', 'lyrics'] as const).map((k) => (
            <button
              key={k}
              onClick={() => setPane(k)}
              className={[
                'rounded-full px-4 py-[6px] text-[13px] transition-colors',
                pane === k ? 'bg-surface text-ink' : 'text-ink-3',
              ].join(' ')}
            >
              {k === 'cover' ? '封面' : '歌词'}
            </button>
          ))}
        </div>

        <div
          className={[
            'flex w-full max-w-[420px] flex-col items-center text-center min-[901px]:flex min-[901px]:w-[330px] min-[901px]:shrink-0',
            pane === 'cover' ? '' : 'max-[900px]:hidden',
          ].join(' ')}
        >
          <div className="flex size-[170px] items-center justify-center rounded-2xl border border-line bg-surface text-[40px] text-ink-4 shadow-[0_8px_32px_rgba(0,0,0,.42)] min-[901px]:size-[330px] min-[901px]:text-[74px]">
            ♪
          </div>
          <div className="mt-[18px] text-xl leading-7 font-semibold min-[901px]:mt-[26px] min-[901px]:text-2xl min-[901px]:leading-8">
            {p.song.title ?? '（无标题）'}
          </div>
          <div className="text-[15px] text-ink-2">{p.song.artists ?? '（未知歌手）'}</div>
          <div className="mt-1 text-[12.5px] text-ink-3">
            {[p.song.year, p.song.format?.toUpperCase()].filter(Boolean).join(' · ')}
          </div>

          <div className="mt-6 w-full">
            <div
              onClick={(e) => {
                const r = e.currentTarget.getBoundingClientRect();
                p.seek(((e.clientX - r.left) / r.width) * p.durationMs);
              }}
              className="relative h-1 cursor-pointer rounded-full bg-white/15"
            >
              <i className="absolute inset-y-0 left-0 rounded-full bg-accent" style={{ width: `${pct}%` }} />
              <b
                className="absolute top-1/2 size-[11px] -translate-x-1/2 -translate-y-1/2 rounded-full bg-white shadow-[0_1px_4px_#0008]"
                style={{ left: `${pct}%` }}
              />
            </div>
            <div className="mt-[9px] flex justify-between text-[11px] text-ink-3 tabular-nums">
              <span>{clock(p.positionMs)}</span>
              <span>{clock(p.durationMs)}</span>
            </div>
          </div>

          <div className="mt-1.5 flex items-center justify-center gap-2">
            <IconBtn onClick={() => {}} title="随机">
              <svg width="17" height="17" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
                <path d="M2 5h2.5l7 6H14" /><path d="M2 11h2.5l7-6H14" />
                <path d="M12 3.2 14 5l-2 1.8" /><path d="M12 9.2 14 11l-2 1.8" />
              </svg>
            </IconBtn>
            <IconBtn onClick={p.prev} title="上一首">
              <svg width="19" height="19" viewBox="0 0 16 16" fill="currentColor">
                <path d="M12.5 4v8l-6-4z" /><path d="M3.5 4h1.2v8H3.5z" />
              </svg>
            </IconBtn>
            <button
              onClick={p.toggle}
              title={p.playing ? '暂停' : '播放'}
              className="mx-2.5 flex size-14 items-center justify-center rounded-full bg-ink text-[#14121b] transition-transform hover:scale-105"
            >
              {p.playing ? (
                <svg width="17" height="17" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M5 3.5h2.4v9H5zM8.6 3.5H11v9H8.6z" />
                </svg>
              ) : (
                <svg width="17" height="17" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M5 3.5v9L13 8z" />
                </svg>
              )}
            </button>
            <IconBtn onClick={p.next} title="下一首">
              <svg width="19" height="19" viewBox="0 0 16 16" fill="currentColor">
                <path d="M3.5 4v8l6-4z" /><path d="M11.3 4h1.2v8h-1.2z" />
              </svg>
            </IconBtn>
            <IconBtn onClick={p.cycleMode} title={`播放模式：${p.queue.mode}`} on={p.queue.mode !== 'order'}>
              <svg width="17" height="17" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
                <path d="M3 8a5 5 0 0 1 5-5h3.5" /><path d="M9.5 1.2 11.7 3 9.5 4.8" />
                <path d="M13 8a5 5 0 0 1-5 5H4.5" /><path d="M6.5 14.8 4.3 13l2.2-1.8" />
              </svg>
            </IconBtn>
          </div>
        </div>

        <div
          className={[
            'flex w-full min-w-0 flex-col self-stretch pt-1.5 min-[901px]:flex min-[901px]:max-w-[560px] min-[901px]:flex-1',
            pane === 'lyrics' ? '' : 'max-[900px]:hidden',
          ].join(' ')}
        >
          <div className="mb-[18px] flex shrink-0 gap-1">
            {(['lyr', 'queue'] as const).map((t) => (
              <button
                key={t}
                onClick={() => setTab(t)}
                className={[
                  'rounded-sm px-[14px] py-[7px] text-[13px] transition-colors',
                  tab === t ? 'bg-surface text-ink' : 'text-ink-3 hover:text-ink',
                ].join(' ')}
              >
                {t === 'lyr' ? '歌词' : '播放队列'}
              </button>
            ))}
          </div>

          {tab === 'lyr' ? (
            <div className="min-h-0 flex-1 overflow-auto pr-2">
              {lyrics.length === 0 ? (
                <p className="text-[13px] text-ink-3">这首歌没有内嵌歌词。</p>
              ) : (
                lyrics.map((line, i) => (
                  <p key={i} className="mb-1 text-[15px] leading-[30px] text-ink-4">
                    {line}
                  </p>
                ))
              )}
            </div>
          ) : (
            <div className="min-h-0 flex-1 overflow-auto">
              {p.queueSongs.map((s, i) => (
                <div
                  key={`${s.id}-${i}`}
                  onClick={() => p.jumpToQueueIndex(i)}
                  className={[
                    'flex h-11 cursor-pointer items-center gap-[11px] rounded-md px-2.5 transition-colors hover:bg-surface',
                    p.song?.id === s.id ? 'bg-accent-soft' : '',
                  ].join(' ')}
                >
                  <span className="flex size-[30px] shrink-0 items-center justify-center rounded-sm bg-surface text-xs text-ink-4">
                    {p.song?.id === s.id ? '▶' : '♪'}
                  </span>
                  <span className="min-w-0 flex-1">
                    <div className={['overflow-hidden text-ellipsis whitespace-nowrap', p.song?.id === s.id ? 'text-accent' : ''].join(' ')}>
                      {s.title ?? '（无标题）'}
                    </div>
                    <div className="overflow-hidden text-xs text-ellipsis whitespace-nowrap text-ink-3">
                      {s.artists ?? '（未知歌手）'}
                    </div>
                  </span>
                  <span className="shrink-0 text-xs text-ink-3 tabular-nums">
                    {clock(s.duration_ms ?? 0)}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
