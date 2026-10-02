import { useCallback, useState } from 'react';
import { Link } from 'react-router';
import type { Playlist } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';
import { messageOf } from '@/lib/session.tsx';

export default function PlaylistsPage() {
  const load = useCallback(() => api.playlists.list(), []);
  const { data, error, loading, reload } = useAsync<{ items: Playlist[]; total: number }>(load);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function create(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim()) return;
    setBusy(true);
    setErr(null);
    try {
      await api.playlists.create(name.trim());
      setName('');
      reload();
    } catch (e2) {
      setErr(messageOf(e2));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex-1 overflow-auto px-5 pt-2 pb-32 max-[1024px]:px-4 max-[640px]:px-3">
      <div className="flex items-end gap-4 pt-2.5 pb-5 max-[640px]:flex-wrap">
        <div>
          <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">歌单</h1>
          <div className="mt-1 text-nav text-ink-3">
            {error ? '读取失败' : loading ? '读取中…' : `${data?.total ?? 0} 个`}
          </div>
        </div>
      </div>

      <form onSubmit={create} className="mb-6 flex gap-2">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="新建歌单，输入名字"
          className="h-9 max-w-[260px] flex-1 rounded-lg border-0 bg-surface px-4 text-nav text-ink outline-0 placeholder:text-ink-4"
        />
        <button
          type="submit"
          disabled={busy || !name.trim()}
          className="h-9 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110 disabled:opacity-40"
        >
          新建
        </button>
      </form>
      {err && <p className="mb-4 text-nav text-accent">{err}</p>}

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : (data?.items ?? []).length === 0 ? (
        <p className="py-6 text-nav text-ink-3">还没有歌单。上面输入名字建一个。</p>
      ) : (
        <div className="grid gap-4 [grid-template-columns:repeat(auto-fill,minmax(132px,1fr))]">
          {(data?.items ?? []).map((p) => (
            <Link
              key={p.id}
              to={`/playlists/${p.id}`}
              className="rounded-lg p-2.5 transition-colors hover:bg-surface"
            >
              <div className="flex aspect-square items-center justify-center rounded-md bg-surface text-display text-ink-4">
                ≡
              </div>
              <div className="mt-2 overflow-hidden text-nav font-medium text-ellipsis whitespace-nowrap">
                {p.name}
              </div>
              <div className="overflow-hidden text-xs leading-4 text-ellipsis whitespace-nowrap text-ink-3">
                {/* 「公开 / 私有」是**歌单自身**的属性，「别人的」是**相对我**的关系。
                    两者的组合决定我能不能改（见后端 playlist_json 的 is_owner），
                    所以两个都要显示 —— 别人的公开歌单点进去是只读的，不标出来会让人
                    以为是页面坏了。 */}
                {p.is_public ? '公开' : '私有'}
                {!p.is_owner && ' · 别人的'}
                {p.description ? ` · ${p.description}` : ''}
              </div>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
