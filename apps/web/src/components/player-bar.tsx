import type { PlayMode } from '@music-robot/core';
import { usePlayer } from '@/lib/player.tsx';

/** 毫秒 → `1:12`。 */
function clock(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '0:00';
  const total = Math.floor(ms / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`;
}

const MODE_LABEL: Record<PlayMode, string> = {
  order: '顺序播放',
  shuffle: '随机播放',
  'repeat-all': '列表循环',
  'repeat-one': '单曲循环',
};

function ModeIcon({ mode }: { mode: PlayMode }) {
  if (mode === 'order') {
    return (
      <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round">
        <path d="M2 4h12M2 8h12M2 12h7" />
      </svg>
    );
  }
  if (mode === 'shuffle') {
    return (
      <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
        <path d="M2 5h2.5l7 6H14" />
        <path d="M2 11h2.5l7-6H14" />
        <path d="M12 3.2 14 5l-2 1.8" />
        <path d="M12 9.2 14 11l-2 1.8" />
      </svg>
    );
  }
  return (
    <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 8a5 5 0 0 1 5-5h3.5" />
      <path d="M9.5 1.2 11.7 3 9.5 4.8" />
      <path d="M13 8a5 5 0 0 1-5 5H4.5" />
      <path d="M6.5 14.8 4.3 13l2.2-1.8" />
      {mode === 'repeat-one' && <path d="M8 6.6v3" />}
    </svg>
  );
}

/**
 * 底部**悬浮**播放条（不是贴底固定）—— 见 docs/design.md §7.4。
 * 代价是会盖住内容最后两行，所以各页面底部都要留 ~130px。
 */
export default function PlayerBar() {
  const { song, playing, positionMs, durationMs, toggle, next, prev, seek, cycleMode, queue } =
    usePlayer();

  if (!song) return null;

  const pct = durationMs > 0 ? Math.min(100, (positionMs / durationMs) * 100) : 0;

  return (
    <div
      className={[
        'absolute bottom-[18px] left-1/2 flex h-[62px] -translate-x-1/2 items-center gap-[14px]',
        'w-[min(880px,calc(100%-56px))] rounded-2xl border border-line px-4',
        'bg-glass shadow-[0_8px_32px_rgba(0,0,0,.42)] backdrop-blur-[18px] backdrop-saturate-150',
        'max-[900px]:bottom-3 max-[900px]:h-14 max-[900px]:w-[calc(100%-24px)] max-[900px]:gap-2.5 max-[900px]:px-3',
      ].join(' ')}
    >
      <span className="flex size-10 shrink-0 items-center justify-center rounded-md bg-surface text-ink-3 max-[900px]:size-9">
        ♪
      </span>
      <div className="w-[150px] min-w-0 shrink-0 max-[900px]:w-auto max-[900px]:flex-1">
        <div className="overflow-hidden font-medium text-ellipsis whitespace-nowrap">
          {song.title ?? '（无标题）'}
        </div>
        <div className="overflow-hidden text-xs leading-4 text-ellipsis whitespace-nowrap text-ink-3">
          {song.artists ?? '（未知歌手）'}
        </div>
      </div>

      <button
        onClick={prev}
        title="上一首"
        className="flex size-[30px] shrink-0 items-center justify-center rounded-full text-ink-2 transition-colors hover:bg-surface-hover hover:text-ink"
      >
        <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
          <path d="M12.5 4v8l-6-4z" />
          <path d="M3.5 4h1.2v8H3.5z" />
        </svg>
      </button>

      <button
        onClick={toggle}
        title={playing ? '暂停' : '播放'}
        className="flex size-[34px] shrink-0 items-center justify-center rounded-full bg-ink text-[#14121b] transition-transform hover:scale-105"
      >
        {playing ? (
          <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
            <path d="M5 3.5h2.4v9H5zM8.6 3.5H11v9H8.6z" />
          </svg>
        ) : (
          <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
            <path d="M5 3.5v9L13 8z" />
          </svg>
        )}
      </button>

      <button
        onClick={next}
        title="下一首"
        className="flex size-[30px] shrink-0 items-center justify-center rounded-full text-ink-2 transition-colors hover:bg-surface-hover hover:text-ink"
      >
        <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
          <path d="M3.5 4v8l6-4z" />
          <path d="M11.3 4h1.2v8h-1.2z" />
        </svg>
      </button>

      <div className="flex min-w-0 flex-1 items-center gap-2.5">
        <span className="shrink-0 text-[11px] text-ink-3 tabular-nums max-[900px]:hidden">
          {clock(positionMs)}
        </span>
        <div
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect();
            seek(((e.clientX - r.left) / r.width) * durationMs);
          }}
          className="relative h-[3px] min-w-0 flex-1 cursor-pointer rounded-full bg-white/15"
        >
          <i
            className="absolute inset-y-0 left-0 rounded-full bg-accent"
            style={{ width: `${pct}%` }}
          />
        </div>
        <span className="shrink-0 text-[11px] text-ink-3 tabular-nums max-[900px]:hidden">
          {clock(durationMs)}
        </span>
      </div>

      <button
        onClick={cycleMode}
        title={MODE_LABEL[queue.mode]}
        className={[
          'hidden size-[30px] shrink-0 items-center justify-center rounded-full transition-colors hover:bg-surface-hover min-[901px]:flex',
          queue.mode === 'order' ? 'text-ink-2' : 'text-accent',
        ].join(' ')}
      >
        <ModeIcon mode={queue.mode} />
      </button>
    </div>
  );
}
