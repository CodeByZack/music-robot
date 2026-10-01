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
import { createResumeStore, type ResumeStore } from '@/lib/resume.ts';

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

  const resumeRef = useRef<ResumeStore | null>(null);
  if (!resumeRef.current) resumeRef.current = createResumeStore();
  const resume = resumeRef.current;

  const [queue, setQueue] = useState<QueueState>(() => createQueue([]));
  const [songs, setSongs] = useState<Map<number, Song>>(() => new Map());
  const [playing, setPlaying] = useState(false);
  const [positionMs, setPositionMs] = useState(0);
  const [durationMs, setDurationMs] = useState(0);

  // 想在「由 ended 推进队列」时读到最新 queue，但 effect 依赖 queue 又会重复触发。
  // 用一个 ref 拿最新值，避免把 next() 塞进 effect 依赖里造成循环。
  const queueRef = useRef(queue);
  queueRef.current = queue;

  /**
   * 最后一次 `timeupdate` 记下的位置。收尾落盘用它，**不去读 audio 元素**。
   *
   * 原因是卸载顺序：Provider 卸载时上面那个事件 effect 的 cleanup 先跑，
   * 里面 `audio.destroy()` 会把 `src` 清掉、`currentTime` 归零；
   * 等换歌 effect 的 cleanup 再去读元素，读到的就是 0 ——
   * 于是「断点」被当成「刚开始」而**删掉**，用户下次发现进度没了。
   */
  const lastPosRef = useRef<{ id: number; ms: number; dur: number } | null>(null);

  useEffect(() => {
    const offs = [
      audio.on('time', () => {
        const ms = audio.currentMs();
        const dur = audio.durationMs();
        setPositionMs(ms);
        setDurationMs(dur);
        const id = currentId(queueRef.current);
        if (id == null) return;
        lastPosRef.current = { id, ms, dur };
        // 边播边记断点（save 内部节流成 15 秒一次）。
        // 不用给 pagehide 挂钩子 —— 最坏也就丢这 15 秒。
        resume.save(id, ms, dur);
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
  }, [audio, resume]);

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
    // 必须**先 await load 再 play**：src 是异步才设上的，
    // 提前 play() 会静默无效（踩过一次，现象是 readyState=4 但 paused=true）。
    void (async () => {
      try {
        // 取断点跟拉流并行，别让 settings 那一次请求压在播放启动的关键路径上。
        // load() resolve 时 metadata 已就绪，所以这里 seek 一定生效。
        const [at] = await Promise.all([resume.get(currentTrackId), audio.load(`/api/stream/${currentTrackId}`)]);
        if (!alive) return;
        if (at != null) {
          audio.seekMs(at);
          setPositionMs(at);
        }
        await audio.play();
        // 记一条播放历史。失败不打断播放 —— 历史是锦上添花，不该影响听歌。
        void api.history.record(currentTrackId).catch(() => {});
      } catch {
        if (alive) setPlaying(false);
      }
    })();
    return () => {
      alive = false;
      // 换歌 / 卸载时把这一首的收尾位置写掉。**用最后一次 timeupdate 记下的值**（见 lastPosRef）：
      // 播完自动下一首时那里是 position ≈ duration，core 会判定「听完了」从而删键，下次从头播。
      // id 对不上说明这一首还没出过声（刚点就换），没什么可记的。
      const last = lastPosRef.current;
      if (last !== null && last.id === currentTrackId) resume.saveNow(last.id, last.ms, last.dur);
    };
  }, [audio, resume, currentTrackId]);

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
    const id = currentId(queueRef.current);
    if (id == null) return;
    if (audioRef.current === null) return;
    if (playing) {
      audio.pause();
      // 暂停基本等于「我这就走开」，立刻落盘，不等那 15 秒的节流窗口。
      resume.saveNow(id, audio.currentMs(), audio.durationMs());
    } else {
      void audio.play().catch(() => setPlaying(false));
    }
  }, [audio, playing, resume]);

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
