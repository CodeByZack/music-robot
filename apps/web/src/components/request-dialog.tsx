import { useState } from 'react';
import type { SubmitRequestResult } from '@music-robot/core';
import { Dialog } from '@/components/dialog.tsx';
import { api } from '@/lib/client.ts';
import { messageOf } from '@/lib/session.tsx';

const INPUT =
  'h-9 w-full rounded-lg border-0 bg-surface px-3 text-nav text-ink outline-0 placeholder:text-ink-4';

/**
 * 「请求这首歌」的弹窗表单。
 *
 * 画布脑图那条链路的起点：① 搜歌没找到 → ② 点「请求这首歌」→ ③ 填歌名/歌手/专辑
 * → ④ `POST /api/requests`。
 *
 * 两个入口共用同一份：**搜索页**（搜不到时）与**点歌请求页**（页内「点一首」）。
 * 做成弹窗而不是页内展开：它是个「填完就走」的小表单，用户填的时候不该丢掉
 * 底下那个列表的上下文。
 *
 * `initialTitle`：从搜索页进来时预填搜索词 —— 用户刚在顶栏打过一遍，
 * 不该让他再打第二遍。
 */
export function RequestSongDialog({
  initialTitle = '',
  onClose,
  onDone,
}: {
  initialTitle?: string;
  onClose: () => void;
  /** 提交成功后的回调（页面用它刷新列表）。 */
  onDone?: (result: SubmitRequestResult) => void;
}) {
  const [title, setTitle] = useState(initialTitle);
  const [artist, setArtist] = useState('');
  const [album, setAlbum] = useState('');
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [result, setResult] = useState<SubmitRequestResult | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const t = title.trim();
    if (!t || busy) return;
    setBusy(true);
    setErr(null);
    try {
      // 空的选填项**不传**：后端把缺席与空串一视同仁，少传一个少一次归一化。
      const r = await api.requests.submit({
        title: t,
        ...(artist.trim() ? { artist: artist.trim() } : {}),
        ...(album.trim() ? { album: album.trim() } : {}),
        ...(note.trim() ? { note: note.trim() } : {}),
      });
      setResult(r);
      onDone?.(r);
    } catch (e2) {
      setErr(messageOf(e2));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog title="请求这首歌" onClose={onClose}>
      {result ? (
        /* 结果分三种，别合并成一句「提交成功」——
           «合并到已有» 与 «你已投过票» 是完全不同的两件事，用户要能分清。
           另外**不自动关闭**：用户可能想看看这条请求现在什么状态。 */
        <div className="p-4">
          <p className="text-nav text-ink-2">
            {result.created
              ? '已提交点歌请求。'
              : result.voted
                ? '库里已经有这条请求了，也给你记了一票。'
                : '你之前已经请求过这首了（不会重复计票）。'}
          </p>
          <p className="mt-1.5 text-cap text-ink-4">
            「{result.request.title}」
            {result.request.artist ? ` · ${result.request.artist}` : ''} · 现在有{' '}
            <b className="font-medium text-ink-3">{result.request.vote_count}</b> 人想要
          </p>
          <div className="mt-4 flex gap-2">
            <button
              type="button"
              onClick={onClose}
              className="h-9 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110"
            >
              知道了
            </button>
            <button
              type="button"
              onClick={() => {
                // 再提一首：清空表单（标题也清掉 —— 用户多半是换一首歌，
                // 留着上一首的标题反而要删一遍）
                setResult(null);
                setTitle('');
                setArtist('');
                setAlbum('');
                setNote('');
              }}
              className="h-9 rounded-full px-4 text-nav text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
            >
              再提一首
            </button>
          </div>
        </div>
      ) : (
        <form onSubmit={submit} className="p-4">
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
          {/* 去重规则写出来 —— 不然用户会怀疑「我提过了怎么还能再提」 */}
          <p className="mt-2.5 text-micro leading-4 text-ink-4">
            同一首歌（名称 + 歌手归一化后相同）只会有一条请求，重复提交是给它加一票。
          </p>
          {err && <p className="mt-2.5 text-cap text-accent">{err}</p>}
          <div className="mt-4 flex gap-2">
            <button
              type="submit"
              disabled={busy || !title.trim()}
              className="h-9 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110 disabled:opacity-40"
            >
              {busy ? '提交中…' : '提交'}
            </button>
            <button
              type="button"
              onClick={onClose}
              className="h-9 rounded-full px-4 text-nav text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
            >
              取消
            </button>
          </div>
        </form>
      )}
    </Dialog>
  );
}
