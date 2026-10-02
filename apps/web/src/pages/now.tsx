import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { PlayMode, Song } from '@music-robot/core';
import Cover from '@/components/cover.tsx';
import { api } from '@/lib/client.ts';
import { useOverlayClose } from '@/lib/use-overlay-close.ts';
import { usePlayer } from '@/lib/player.tsx';
import { useAsync } from '@/lib/use-async.tsx';

type SongWithLyrics = Song & { lyrics?: string | null; timed_lyrics?: string | null };

/** 一行歌词：`ms` 为 -1 表示这行没有时间轴。 */
interface LyricLine {
  ms: number;
  text: string;
}

/** `[mm:ss]` / `[mm:ss.xx]` / `[m:ss.xxx]` 行解析成带时间轴的歌词。没有时间轴的行丢掉。 */
function parseLrc(raw: string): LyricLine[] {
  const out: LyricLine[] = [];
  for (const line of raw.split('\n')) {
    const m = /^\s*\[(\d+):(\d+(?:\.\d+)?)\]\s*(.*)$/.exec(line);
    const text = m?.[3]?.trim();
    if (m && text) out.push({ ms: Number(m[1]) * 60_000 + Math.round(Number(m[2]) * 1000), text });
  }
  return out;
}

function clock(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '0:00';
  const t = Math.floor(ms / 1000);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, '0')}`;
}

const MODE_LABEL: Record<PlayMode, string> = {
  order: '顺序播放',
  shuffle: '随机播放',
  'repeat-all': '列表循环',
  'repeat-one': '单曲循环',
};

function ModeIcon({ mode }: { mode: PlayMode }) {
  if (mode === 'shuffle') {
    return (
      <svg className="ico-md" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
        <path d="M2 5h2.5l7 6H14" /><path d="M2 11h2.5l7-6H14" />
        <path d="M12 3.2 14 5l-2 1.8" /><path d="M12 9.2 14 11l-2 1.8" />
      </svg>
    );
  }
  if (mode === 'order') {
    return (
      <svg className="ico-md" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round">
        <path d="M2 4h12M2 8h12M2 12h7" />
      </svg>
    );
  }
  return (
    <svg className="ico-md" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 8a5 5 0 0 1 5-5h3.5" /><path d="M9.5 1.2 11.7 3 9.5 4.8" />
      <path d="M13 8a5 5 0 0 1-5 5H4.5" /><path d="M6.5 14.8 4.3 13l2.2-1.8" />
      {mode === 'repeat-one' && <path d="M8 6.6v3" />}
    </svg>
  );
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
      type="button"
      onClick={onClick}
      title={title}
      aria-label={title}
      className={[
        'flex size-10 items-center justify-center rounded-full transition-colors hover:bg-surface-hover',
        on ? 'text-accent' : 'text-ink-2 hover:text-ink',
      ].join(' ')}
    >
      {children}
    </button>
  );
}

/** 音量。点开一个小竖条 —— 后端 settings 里没有 volume 键，这是纯本地偏好。 */
function VolumeBtn({ volume, setVolume }: { volume: number; setVolume: (v: number) => void }) {
  const [open, setOpen] = useState(false);
  const wrap = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!wrap.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [open]);

  return (
    <div ref={wrap} className="relative">
      <IconBtn title="音量" on={volume === 0} onClick={() => setOpen((v) => !v)}>
        <svg className="ico-md" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
          <path d="M3 6h2l3-2.5v9L5 10H3z" />
          {volume > 0.05 && <path d="M10.5 6.2a2.6 2.6 0 0 1 0 3.6" />}
          {volume > 0.6 && <path d="M12.2 4.6a5 5 0 0 1 0 6.8" />}
          {volume <= 0.05 && <path d="M11 6.5 14 10M14 6.5 11 10" />}
        </svg>
      </IconBtn>

      {open && (
        <div className="absolute bottom-[46px] left-1/2 z-20 flex h-[122px] w-11 -translate-x-1/2 flex-col items-center rounded-lg border border-line bg-[#0a0a0eb8] py-2 backdrop-blur-xl">
          <input
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={volume}
            onChange={(e) => setVolume(Number(e.target.value))}
            style={{ writingMode: 'vertical-lr', direction: 'rtl' }}
            className="h-[82px] accent-accent"
          />
          <span className="mt-1 text-micro text-ink-3 tabular-nums">{Math.round(volume * 100)}</span>
        </div>
      )}
    </div>
  );
}

export default function NowPage() {
  const p = usePlayer();
  /**
   * 窄屏（<900px）下封面和歌词**不能同时铺开** —— 竖着堆要滚很久，
   * 而且封面巨大、歌词只剩一条缝。所以窄屏只显示一栏，顶上给个切换。
   * 桌面两栏并排，这个状态不起作用。
   */
  const [pane, setPane] = useState<'cover' | 'lyrics'>('cover');
  const [queueOpen, setQueueOpen] = useState(false);
  const songId = p.song?.id;
  const load = useCallback(
    () => (songId ? api.library.song(songId) : Promise.resolve(null)),
    [songId],
  );
  const { data } = useAsync<{ song: SongWithLyrics } | null>(load, [songId]);

  // 收起飞出：优先回上一页，带动画（见 lib/use-overlay-close.ts）
  const { closing, close: back } = useOverlayClose('/');

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') back();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [back]);

  /**
   * 歌词行。优先用**同步歌词**（SYLT）；没有就看看普通歌词里是不是本来就带着
   * LRC 时间轴 —— 下载器（QQ / 酷我）就是这种：把 LRC 当纯文本写进 USLT，实测
   * 本项目的库全是这样。两样都没有，才退回「按进度等比猜行」。
   *
   * 后一步看着像在「猜」，但它是在**解读已有的内容**，不是凭空造数据 ——
   * 引擎层已经不再把两个歌词字段互相派生了。
   */
  const lines = useMemo<LyricLine[]>(() => {
    const song = data?.song;
    const timed = parseLrc(song?.timed_lyrics ?? '');
    if (timed.length > 0) return timed;
    const plain = parseLrc(song?.lyrics ?? '');
    if (plain.length > 0) return plain;
    // 确实没有时间轴：按行铺开，后面用播放进度等比推当前行
    return (song?.lyrics ?? '')
      .split('\n')
      .map((t) => t.trim())
      .filter(Boolean)
      .map((text) => ({ ms: -1, text }));
  }, [data]);

  /**
   * 当前行。
   *
   * 有时间轴就用**真同步**（取最后一个 `ms <= 当前进度` 的行）；
   * 没有时间轴才退回「按播放进度等比推」—— 那是近似，不是同步。
   * （旧注释说「库里的歌词全是裸文本」，那是因为读取层把时间轴剥掉了。）
   */
  const activeLine = (() => {
    if (lines.length === 0) return -1;
    if ((lines[0]?.ms ?? -1) >= 0) {
      let idx = 0;
      for (let i = 0; i < lines.length; i += 1) {
        const line = lines[i];
        if (!line || line.ms > p.positionMs) break;
        idx = i;
      }
      return p.positionMs > 0 ? idx : -1;
    }
    if (p.durationMs <= 0) return -1;
    return Math.min(lines.length - 1, Math.floor((p.positionMs / p.durationMs) * lines.length));
  })();
  const activeRef = useRef<HTMLParagraphElement | null>(null);
  // 歌词自己的滚动容器。**只滚它**，见下面的 effect。
  const lyricsRef = useRef<HTMLDivElement | null>(null);

  /**
   * 把当前行滚到视口中间。
   *
   * ⚠️ 这里**不能用 `scrollIntoView`** —— 它会连同**所有可滚动祖先**一起滚，
   * 包括本页那个 `overflow-hidden` 的外层容器。后果是整页被顶上去一截
   * （实测 106px），连背景的压暗蒙层都被推走、底部漏出一条亮带。
   * 所以改成自己算 `scrollTop`，只动歌词框。
   */
  useEffect(() => {
    const box = lyricsRef.current;
    const line = activeRef.current;
    if (!box || !line) return;
    const target = line.offsetTop - box.clientHeight / 2 + line.clientHeight / 2;
    const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
    box.scrollTo({ top: Math.max(0, target), behavior: reduce ? 'auto' : 'smooth' });
  }, [activeLine]);

  const pct = p.durationMs > 0 ? Math.min(100, (p.positionMs / p.durationMs) * 100) : 0;

  return (
    /* 全屏浮层（对齐飞牛）：盖住侧边栏与顶栏，不换路由。
       进场 / 退场动画见 global.css 的 .anim-overlay-*。 */
    <div
      className={[
        'fixed inset-0 z-50 flex flex-col overflow-hidden bg-[#14121b]',
        closing ? 'anim-overlay-out' : 'anim-overlay-in',
      ].join(' ')}
    >
      {/* 氛围背景：真封面的模糊取色。没封面时退回手挑的紫兜底。 */}
      {songId ? (
        <img
          src={api.library.coverUrl(songId)}
          alt=""
          aria-hidden
          className="pointer-events-none absolute inset-0 size-full scale-125 object-cover opacity-25 blur-[70px] brightness-[.85] saturate-150"
        />
      ) : (
        <div className="pointer-events-none absolute -inset-[120px] bg-[radial-gradient(60%_55%_at_26%_34%,#7b4bd0_0%,transparent_62%),radial-gradient(50%_50%_at_62%_18%,#c934e1_0%,transparent_60%),radial-gradient(45%_45%_at_40%_70%,#ef6b3c_0%,transparent_62%)] opacity-50 blur-[110px] saturate-150" />
      )}
      {/* 压暗蒙层。**必须够重** —— 浅色封面（这张就是白底插画）不压暗的话整页会泛米白，
          跟 design.md 的「近黑底 + 彩色只用在 logo/主操作/进度条」直接冲突，
          白字也会读不清。 */}
      <div className="pointer-events-none absolute inset-0 bg-[linear-gradient(to_bottom,rgba(20,18,27,.72)_0%,#14121b_70%)]" />

      {/* 顶栏：左上 ⌄ 收起、右上 ✕ 关闭 —— 两个都只是「离开播放页」 */}
      <header className="relative z-10 flex h-[58px] shrink-0 items-center px-4">
        <IconBtn title="收起" onClick={back}>
          <svg className="ico-lg" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
            <path d="M3.5 6 8 10.5 12.5 6" />
          </svg>
        </IconBtn>
        <span className="flex-1" />
        <IconBtn title="关闭" onClick={back}>
          <svg className="ico-md" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round">
            <path d="M4 4l8 8M12 4l-8 8" />
          </svg>
        </IconBtn>
      </header>

      {!p.song ? (
        <div className="relative z-10 grid flex-1 place-items-center text-nav text-ink-3">
          还没有在放的歌。去音乐库点一首。
        </div>
      ) : (
        /* 滚动容器自己不居中：交给内层 m-auto。直接给滚动容器 items-center 时，
           内容一旦比容器高，顶部会被裁掉且滚不上去。 */
        <div className="relative z-10 flex min-h-0 flex-1 flex-col overflow-auto">
          {/* 窄屏的封面 / 歌词 切换。桌面两栏都在，所以整块藏掉。 */}
          <div className="flex shrink-0 gap-1 self-center rounded-full bg-black/25 p-1 min-[901px]:hidden">
            {(['cover', 'lyrics'] as const).map((k) => (
              <button
                key={k}
                type="button"
                onClick={() => setPane(k)}
                className={[
                  'rounded-full px-4 py-2 text-nav transition-colors',
                  pane === k ? 'bg-surface text-ink' : 'text-ink-3',
                ].join(' ')}
              >
                {k === 'cover' ? '封面' : '歌词'}
              </button>
            ))}
          </div>

          {/* m-auto 居中：内容装得下就居中，装不下就从顶部开始并可滚动
              （给滚动容器加 items-center 时溢出会裁掉顶部且滚不上去）。 */}
          <div className="m-auto flex w-full flex-col gap-5 px-6 pb-6 min-[901px]:flex-row min-[901px]:items-center min-[901px]:justify-center min-[901px]:gap-11 min-[901px]:px-10 min-[901px]:py-6">
            <div
              className={[
                'flex w-full max-w-[440px] flex-col items-center text-center min-[901px]:w-[380px] min-[901px]:shrink-0',
                pane === 'cover' ? '' : 'max-[900px]:hidden',
              ].join(' ')}
            >
            <Cover
              id={p.song.id}
              className="size-[190px] border border-line shadow-[0_8px_32px_rgba(0,0,0,.42)] min-[901px]:size-[330px]"
              rounded="rounded-2xl"
              glyphClass="text-glyph"
            />

            <div className="mt-5 text-xl leading-7 font-semibold min-[901px]:mt-6 min-[901px]:text-2xl min-[901px]:leading-8">
              {p.song.title ?? '（无标题）'}
            </div>
            <div className="mt-1 text-nav text-ink-3">
              {[p.song.artists ?? '（未知歌手）', p.song.album].filter(Boolean).join(' · ')}
            </div>

            {/* 进度条：格式徽标嵌在轨道正中（飞牛同款细节） */}
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
                {p.song.format && (
                  <span className="pointer-events-none absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 rounded-xs bg-white/10 px-2 py-px text-micro tracking-[.1em] text-ink-3 uppercase backdrop-blur-sm">
                    {p.song.format}
                  </span>
                )}
              </div>
              <div className="mt-2 flex justify-between text-cap text-ink-3 tabular-nums">
                <span>{clock(p.positionMs)}</span>
                <span>{clock(p.durationMs)}</span>
              </div>
            </div>

            {/* 控制排：模式 · 上一首 · 播放 · 下一首 · 音量（对齐飞牛） */}
            <div className="mt-1.5 flex items-center justify-center gap-2">
              <IconBtn onClick={p.cycleMode} title={MODE_LABEL[p.queue.mode]} on={p.queue.mode !== 'order'}>
                <ModeIcon mode={p.queue.mode} />
              </IconBtn>
              <IconBtn onClick={p.prev} title="上一首">
                <svg className="ico-lg" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M12.5 4v8l-6-4z" /><path d="M3.5 4h1.2v8H3.5z" />
                </svg>
              </IconBtn>
              <button
                type="button"
                onClick={p.toggle}
                title={p.playing ? '暂停' : '播放'}
                aria-label={p.playing ? '暂停' : '播放'}
                className="mx-2.5 flex size-14 items-center justify-center rounded-full bg-ink text-[#14121b] transition-transform hover:scale-105"
              >
                {p.playing ? (
                  <svg className="ico-md" viewBox="0 0 16 16" fill="currentColor">
                    <path d="M5 3.5h2.4v9H5zM8.6 3.5H11v9H8.6z" />
                  </svg>
                ) : (
                  <svg className="ico-md" viewBox="0 0 16 16" fill="currentColor">
                    <path d="M5 3.5v9L13 8z" />
                  </svg>
                )}
              </button>
              <IconBtn onClick={p.next} title="下一首">
                <svg className="ico-lg" viewBox="0 0 16 16" fill="currentColor">
                  <path d="M3.5 4v8l6-4z" /><path d="M11.3 4h1.2v8h-1.2z" />
                </svg>
              </IconBtn>
              <VolumeBtn volume={p.volume} setVolume={p.setVolume} />
            </div>
          </div>

          {/* 右栏：纯歌词（队列挪到右下角浮标了，见文件末尾） */}
          <div
            className={[
              'flex w-full min-w-0 flex-col self-stretch pt-1.5 min-[901px]:max-w-[560px] min-[901px]:flex-1 min-[901px]:justify-center',
              pane === 'lyrics' ? '' : 'max-[900px]:hidden',
            ].join(' ')}
          >
            <div ref={lyricsRef} className="min-h-0 flex-1 overflow-auto pr-2 min-[901px]:max-h-[70vh]">
              {lines.length === 0 ? (
                <p className="text-nav text-ink-3">这首歌没有内嵌歌词。</p>
              ) : (
                lines.map((line, i) => (
                  <p
                    key={i}
                    ref={i === activeLine ? activeRef : undefined}
                    className={[
                      'mb-1 text-lead leading-[30px] transition-colors',
                      i === activeLine ? 'text-ink' : 'text-ink-4',
                    ].join(' ')}
                  >
                    {line.text}
                  </p>
                ))
              )}
            </div>
            </div>
          </div>
        </div>
      )}

      {/* 队列浮标（右下角）+ 面板 —— 对齐飞牛，不占右栏 */}
      {p.queueSongs.length > 0 && (
        <div className="absolute right-5 bottom-5 z-20 flex flex-col items-end gap-2">
          {queueOpen && (
            <div className="max-h-[min(60vh,420px)] w-[min(360px,calc(100vw-40px))] overflow-auto rounded-2xl border border-line bg-[#0a0a0ee6] p-2 shadow-[0_8px_32px_#000a] backdrop-blur-xl">
              <div className="flex items-center gap-2 px-2.5 py-1.5">
                <b className="text-note font-normal text-ink-2">播放队列</b>
                <span className="text-cap text-ink-4">{p.queueSongs.length}</span>
                <span className="flex-1" />
                <button
                  type="button"
                  onClick={() => setQueueOpen(false)}
                  title="关闭队列"
                  aria-label="关闭队列"
                  className="flex size-6 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
                >
                  <svg className="ico-xs" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round">
                    <path d="M4 4l8 8M12 4l-8 8" />
                  </svg>
                </button>
              </div>
              {p.queueSongs.map((s, i) => (
                <button
                  type="button"
                  key={`${s.id}-${i}`}
                  onClick={() => p.jumpToQueueIndex(i)}
                  className={[
                    'flex w-full items-center gap-3 rounded-md px-2.5 py-2 text-left transition-colors hover:bg-surface',
                    p.song?.id === s.id ? 'bg-accent-soft' : '',
                  ].join(' ')}
                >
                  <span className="w-4 shrink-0 text-center text-cap text-ink-4 tabular-nums">
                    {p.song?.id === s.id ? '▶' : i + 1}
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className={['block overflow-hidden text-nav text-ellipsis whitespace-nowrap', p.song?.id === s.id ? 'text-accent' : ''].join(' ')}>
                      {s.title ?? '（无标题）'}
                    </span>
                    <span className="block overflow-hidden text-cap text-ellipsis whitespace-nowrap text-ink-3">
                      {s.artists ?? '（未知歌手）'}
                    </span>
                  </span>
                  <span className="shrink-0 text-cap text-ink-3 tabular-nums">
                    {clock(s.duration_ms ?? 0)}
                  </span>
                </button>
              ))}
            </div>
          )}

          <button
            type="button"
            onClick={() => setQueueOpen((v) => !v)}
            title="播放队列"
            aria-label="播放队列"
            className={[
              'flex h-10 items-center gap-2 rounded-full border border-line px-3.5 text-note backdrop-blur-xl transition-colors',
              queueOpen ? 'bg-surface-press text-ink' : 'bg-[#ffffff12] text-ink-2 hover:bg-surface-hover hover:text-ink',
            ].join(' ')}
          >
            <svg className="ico-sm" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round">
              <path d="M2 4.5h12M2 8h12M2 11.5h7" />
              <circle cx="12.4" cy="11.5" r="1.7" fill="currentColor" stroke="none" />
            </svg>
            队列
            <span className="text-ink-4 tabular-nums">{p.queueSongs.length}</span>
          </button>
        </div>
      )}
    </div>
  );
}
