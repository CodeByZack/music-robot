import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import {
  createQueue,
  currentId,
  next,
  prev,
  replaceQueue,
  setMode as setQueueMode,
  type PlayMode,
  type QueueState,
  type Song,
} from '@music-robot/core';
import { createAudioAdapter, type AudioAdapter } from '@/adapters/audio.web.ts';
import { api, resolveMediaUrl } from '@/lib/client.ts';

interface PlayerValue {
  queue: QueueState;
  song: Song | null;
  playing: boolean;
  positionMs: number;
  durationMs: number;
  /** 从一份列表开始播；`index` 是起始位置。 */
  playList: (songs: Song[], index: number) => void;
  toggle: () => void;
  next: () => void;
  prev: () => void;
  seek: (ms: number) => void;
  cycleMode: () => void;
  /** 队列里的曲目（按播放顺序），给「正在播放」页的队列标签用。 */
  queueSongs: Song[];
  /** 跳到队列里的第 i 个（按播放顺序）。 */
  jumpToQueueIndex: (index: number) => void;
}

const PlayerContext = createContext<PlayerValue | null>(null);

const MODE_ORDER: PlayMode[] = ['order', 'shuffle', 'repeat-all', 'repeat-one'];

export function PlayerProvider({ children }: { children: ReactNode }) {
  const audioRef = useRef<AudioAdapter | null>(null);
  // resolveSrc 里那个 blob 绕法的来龙去脉见 lib/client.ts 的注释
  if (!audioRef.current) audioRef.current = createAudioAdapter({ resolveSrc: resolveMediaUrl });
  const audio = audioRef.current;

  const [queue, setQueue] = useState<QueueState>(() => createQueue([]));
  const [songs, setSongs] = useState<Map<number, Song>>(() => new Map());
  const [playing, setPlaying] = useState(false);
  const [positionMs, setPositionMs] = useState(0);
  const [durationMs, setDurationMs] = useState(0);

  // 想在「由 ended 推进队列」时读到最新 queue，但 effect 依赖 queue 又会重复触发。
  // 用一个 ref 拿最新值，避免把 next() 塞进 effect 依赖里造成循环。
  const queueRef = useRef(queue);
  queueRef.current = queue;

  useEffect(() => {
    const offs = [
      audio.on('time', () => {
        setPositionMs(audio.currentMs());
        setDurationMs(audio.durationMs());
      }),
      audio.on('playing', () => setPlaying(true)),
      audio.on('paused', () => setPlaying(false)),
      audio.on('ended', () => setQueue((q) => next(q))),
      audio.on('error', () => setPlaying(false)),
    ];
    return () => {
      offs.forEach((off) => off());
      audio.destroy();
    };
  }, [audio]);

  const currentTrackId = currentId(queue);

  // 队列里的曲目，按**播放顺序**排（随机会打乱 order）
  const queueSongs = queue.order
    .map((idx) => queue.trackIds[idx])
    .map((id) => (id === undefined ? undefined : songs.get(id)))
    .filter((s): s is Song => Boolean(s));

  // 队列光标一变就换源。`playing` 由 audio 的 playing/paused 事件回报，
  // 不在这里猜 —— 猜会导致 UI 与实际出声不一致。
  useEffect(() => {
    if (currentTrackId == null) return;
    let alive = true;
    setPositionMs(0);
    // 必须**先 await load 再 play**：blob 绕法下 src 是异步才设上的，
    // 提前 play() 会静默无效（踩过一次，现象是 readyState=4 但 paused=true）。
    void (async () => {
      try {
        await audio.load(`/api/stream/${currentTrackId}`);
        if (!alive) return;
        await audio.play();
        // 记一条播放历史。失败不打断播放 —— 历史是锦上添花，不该影响听歌。
        void api.history.record(currentTrackId).catch(() => {});
      } catch {
        if (alive) setPlaying(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [audio, currentTrackId]);

  const playList = useCallback((list: Song[], index: number) => {
    const map = new Map(list.map((s) => [s.id, s]));
    setSongs(map);
    setQueue((q) => {
      const ids = list.map((s) => s.id);
      const startAt = list[index]?.id;
      // 保持当前模式；从空队列起步时 createQueue 会把起始曲放在第 0 位
      return q.trackIds.length === 0
        ? createQueue(ids, q.mode, startAt)
        : replaceQueue(q, ids, startAt);
    });
  }, []);

  const toggle = useCallback(() => {
    if (currentId(queueRef.current) == null) return;
    if (audioRef.current === null) return;
    if (playing) audio.pause();
    else void audio.play().catch(() => setPlaying(false));
  }, [audio, playing]);

  const seek = useCallback(
    (ms: number) => {
      audio.seekMs(ms);
      setPositionMs(ms);
    },
    [audio],
  );

  const cycleMode = useCallback(() => {
    setQueue((q) => {
      const at = MODE_ORDER.indexOf(q.mode);
      const nextMode = MODE_ORDER[(at + 1) % MODE_ORDER.length] as PlayMode;
      // 交给 core 处理「切随机时当前这首歌必须留在原地」
      return setQueueMode(q, nextMode);
    });
  }, []);

  const value = useMemo<PlayerValue>(
    () => ({
      queue,
      song: currentTrackId == null ? null : (songs.get(currentTrackId) ?? null),
      playing,
      positionMs,
      durationMs,
      playList,
      toggle,
      next: () => setQueue((q) => next(q)),
      prev: () => setQueue((q) => prev(q)),
      seek,
      cycleMode,
      queueSongs,
      jumpToQueueIndex: (index: number) => setQueue((q) => ({ ...q, cursor: index })),
    }),
    [
      queue,
      currentTrackId,
      songs,
      playing,
      positionMs,
      durationMs,
      playList,
      toggle,
      seek,
      cycleMode,
      queueSongs,
    ],
  );

  return <PlayerContext.Provider value={value}>{children}</PlayerContext.Provider>;
}

export function usePlayer(): PlayerValue {
  const v = useContext(PlayerContext);
  if (!v) throw new Error('usePlayer 必须在 PlayerProvider 内使用');
  return v;
}

export { MODE_ORDER };
