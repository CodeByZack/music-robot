import { useCallback, useEffect, useState } from 'react';
import { useSearchParams } from 'react-router';
import type { Page, Song, SubmitRequestResult } from '@music-robot/core';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { messageOf } from '@/lib/session.tsx';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

/**
 * 全局搜索页。入口是**顶栏那个搜索胶囊**（回车跳这里）。
 *
 * 走的是后端 `/api/search`，不是拿当前列表做本地过滤 —— 搜的是整个曲库。
 * （以前搜索框在音乐库页自己的 header 里，只能过滤已加载的那 200 首。）
 */
export default function SearchPage() {
  const [params] = useSearchParams();
  const q = (params.get('q') ?? '').trim();
  const load = useCallback(
    () => (q ? api.search(q, { page_size: 200 }) : Promise.resolve(null)),
    [q],
  );
  const { data, error, loading } = useAsync<Page<Song> | null>(load, [q]);

  return (
    <div className="flex-1 overflow-auto px-5 pt-1.5 pb-32 max-[1024px]:px-4 max-[640px]:px-3">
      <div className="pt-2.5 pb-5">
        <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">搜索</h1>
        <div className="mt-1 text-nav text-ink-3">
          {!q
            ? '在顶栏的搜索框里输入关键词'
            : loading
              ? '搜索中…'
              : error
                ? '搜索失败'
                : `“${q}” · ${data?.total ?? 0} 首`}
        </div>
      </div>

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : !q ? null : (data?.items.length ?? 0) === 0 ? (
        <RequestSong keyword={q} />
      ) : (
        <SongTable songs={data?.items ?? []} />
      )}
    </div>
  );
}

const INPUT =
  'h-9 rounded-lg border-0 bg-surface px-3 text-nav text-ink outline-0 placeholder:text-ink-4';

/**
 * 搜不到时的「请求这首歌」—— 画布脑图那条链路的起点：
 * ① 搜歌没找到 → ② 点「请求这首歌」→ ③ 填 歌名/艺术家/专辑 → ④ POST /api/requests
 *
 * 放在**搜索页**（用户已经在这里了），而不是另开一个「点歌」页面 ——
 * 需求的触发点就是「我搜了，没有」，离开这一页去别处填表是白走路。
 *
 * 预填当前搜索词：用户刚在顶栏打过一遍，不该让他再打第二遍。
 * 提交是**去重合并**的：同一首歌（归一化后同键）只会有一条请求，重复提交只是给你
 * 补一票，所以结果反馈要分三种情况说清楚（见下）。
 */
function RequestSong({ keyword }: { keyword: string }) {
  const [open, setOpen] = useState(false);
  const [title, setTitle] = useState(keyword);
  const [artist, setArtist] = useState('');
  const [album, setAlbum] = useState('');
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [result, setResult] = useState<SubmitRequestResult | null>(null);

  // 搜索词一变（用户在顶栏搜了别的），整块重置：预填跟着新词走，
  // 上一条的结果也别挂在新词下面 —— 那会让人以为是这一首的结果。
  useEffect(() => {
    setTitle(keyword);
    setResult(null);
    setErr(null);
    setOpen(false);
  }, [keyword]);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const t = title.trim();
    if (!t || busy) return;
    setBusy(true);
    setErr(null);
    try {
      // 空的选填项**不传**（后端把缺席与空串一视同仁，但不传少一次归一化）。
      setResult(
        await api.requests.submit({
          title: t,
          ...(artist.trim() ? { artist: artist.trim() } : {}),
          ...(album.trim() ? { album: album.trim() } : {}),
          ...(note.trim() ? { note: note.trim() } : {}),
        }),
      );
      setOpen(false);
    } catch (e2) {
      setErr(messageOf(e2));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="py-6">
      <p className="text-nav text-ink-3">没有找到匹配的歌曲。</p>

      {result ? (
        /* 结果分三种，别合并成一句「提交成功」——
             «合并» 与 «你已投过» 是完全不同的两件事，用户要能分清。 */
        <div className="mt-3.5 rounded-lg bg-surface px-4 py-3.5">
          <p className="text-nav text-ink-2">
            {result.created
              ? '已提交点歌请求。'
              : result.voted
                ? '库里已经有这条请求了，也给你记了一票。'
                : '你之前已经请求过这首了（不会重复计票）。'}
          </p>
          <p className="mt-1 text-cap text-ink-4">
            「{result.request.title}」
            {result.request.artist ? ` · ${result.request.artist}` : ''} · 现在有{' '}
            <b className="font-medium text-ink-3">{result.request.vote_count}</b> 人想要
          </p>
          <button
            type="button"
            onClick={() => {
              setResult(null);
              setOpen(true);
            }}
            className="mt-2 text-cap text-ink-3 underline underline-offset-2 transition-colors hover:text-ink"
          >
            再提一首
          </button>
        </div>
      ) : !open ? (
        <button
          type="button"
          onClick={() => setOpen(true)}
          className="mt-2.5 rounded-full bg-accent px-4 py-2 text-nav font-medium text-white transition-colors hover:brightness-110"
        >
          请求这首歌
        </button>
      ) : (
        <form onSubmit={submit} className="mt-3.5 max-w-[420px] rounded-lg bg-surface p-4">
          <div className="mb-2 text-micro tracking-[.24em] text-ink-4 uppercase">请求这首歌</div>
          <div className="flex flex-col gap-2">
            <input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="歌名（必填）"
              className={INPUT}
            />
            <input
              value={artist}
              onChange={(e) => setArtist(e.target.value)}
              placeholder="歌手"
              className={INPUT}
            />
            <input
              value={album}
              onChange={(e) => setAlbum(e.target.value)}
              placeholder="专辑"
              className={INPUT}
            />
            <input
              value={note}
              onChange={(e) => setNote(e.target.value)}
              placeholder="备注（比如「要现场版」）"
              className={INPUT}
            />
          </div>
          {err && <p className="mt-2 text-cap text-accent">{err}</p>}
          <div className="mt-3 flex gap-2">
            <button
              type="submit"
              disabled={busy || !title.trim()}
              className="h-9 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110 disabled:opacity-40"
            >
              {busy ? '提交中…' : '提交'}
            </button>
            <button
              type="button"
              onClick={() => setOpen(false)}
              className="h-9 rounded-full px-4 text-nav text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
            >
              取消
            </button>
          </div>
        </form>
      )}
    </div>
  );
}
