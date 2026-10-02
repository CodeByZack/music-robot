import { useCallback, useState } from 'react';
import { Link, useSearchParams } from 'react-router';
import type { Page, Song } from '@music-robot/core';
import { RequestSongDialog } from '@/components/request-dialog.tsx';
import SongTable from '@/components/song-table.tsx';
import { api } from '@/lib/client.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

/**
 * 全局搜索页。入口是**顶栏那个搜索胶囊**（回车跳这里）。
 *
 * 走的是后端 `/api/search`，不是拿当前列表做本地过滤 —— 搜的是整个曲库。
 * （以前搜索框在音乐库页自己的 header 里，只能过滤已加载的那 200 首。）
 *
 * 搜不到时给「请求这首歌」—— 这是画布脑图那条点歌链路的自然起点：
 * 用户的需求就是在「搜了、没有」这一刻产生的。填表走弹窗，不离开本页。
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
        <NoMatch keyword={q} />
      ) : (
        <SongTable songs={data?.items ?? []} />
      )}
    </div>
  );
}

/** 搜不到时的去向：请求这首歌（弹窗，预填搜索词），或去点歌请求页看进度。 */
function NoMatch({ keyword }: { keyword: string }) {
  const [composing, setComposing] = useState(false);

  return (
    <div className="py-6">
      <p className="text-nav text-ink-3">没有找到匹配的歌曲。</p>
      <div className="mt-3.5 flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={() => setComposing(true)}
          className="rounded-full bg-accent px-4 py-2 text-nav font-medium text-white transition-colors hover:brightness-110"
        >
          请求这首歌
        </button>
        {/* 已经提过的人会去看进度；也给个入口，省得他只知道菜单里有 */}
        <Link
          to="/requests"
          className="rounded-full px-3.5 py-2 text-nav text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
        >
          查看我的点歌
        </Link>
      </div>
      {composing && (
        <RequestSongDialog initialTitle={keyword} onClose={() => setComposing(false)} />
      )}
    </div>
  );
}
