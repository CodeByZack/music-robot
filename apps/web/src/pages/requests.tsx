import { useCallback, useState } from 'react';
import type { RequestStatus, Song, SongRequest } from '@music-robot/core';
import { OverlayShell } from '@/components/overlay.tsx';
import { RequestSongDialog } from '@/components/request-dialog.tsx';
import { api } from '@/lib/client.ts';
import { messageOf, useSession } from '@/lib/session.tsx';
import { useAsync } from '@/lib/use-async.tsx';

/**
 * 点歌请求页（全屏浮层页面，**和设置一样**）。
 *
 * 入口有两个（都不是本页自己发的）：
 * 1. 顶栏右上角的下拉菜单（面向所有人）；
 * 2. 搜索页搜不到时的「请求这首歌」（面向需求发生的当场）。
 * 两条都通到这里 —— 但真正的「提交」用的是同一个 `RequestSongDialog`。
 *
 * 权限（后端 `routes::requests` 强制）：普通用户无论带什么 query 都只看得到
 * **自己提交的**；`?status=` 那种全量筛选是管理端能力，普通用户调会 403。
 * 所以「只看我的」这个开关只对 admin 出现。
 */
const FILTERS: { id: 'all' | RequestStatus; label: string }[] = [
  { id: 'all', label: '全部' },
  { id: 'pending', label: '待处理' },
  { id: 'processing', label: '处理中' },
  { id: 'done', label: '已添加' },
  { id: 'rejected', label: '已拒绝' },
];

export default function RequestsPage() {
  const { user } = useSession();
  const isAdmin = user?.role === 'admin';
  const [filter, setFilter] = useState<'all' | RequestStatus>('all');
  const [mineOnly, setMineOnly] = useState(false);
  const [composing, setComposing] = useState(false);

  const load = useCallback(
    () =>
      api.requests.list({
        ...(filter === 'all' ? {} : { status: filter }),
        ...(mineOnly ? { mine: 1 as const } : {}),
      }),
    [filter, mineOnly],
  );
  const { data, error, loading, reload } = useAsync(load, [filter, mineOnly]);

  // 当前展开的就地操作（一次只开一条 —— 列表里同时开两个输入框很难看）
  const [action, setAction] = useState<{ id: number; kind: 'reject' | 'link' } | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  /** 所有动作走这里：报错就地显示，成功后重拉（状态与票数都会变）。 */
  async function run(fn: () => Promise<unknown>) {
    setBusy(true);
    setErr(null);
    try {
      await fn();
      setAction(null);
      reload();
    } catch (e) {
      setErr(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  const items = data?.items ?? [];

  return (
    <OverlayShell
      title="点歌请求"
      description={
        isAdmin
          ? '大家想听但库里没有的歌。按想要的人数排序。'
          : '你提交的点歌请求与处理进度。'
      }
      actions={
        <button
          type="button"
          onClick={() => setComposing(true)}
          className="h-9 shrink-0 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110"
        >
          点一首
        </button>
      }
    >
      <div className="mb-4 flex flex-wrap items-center gap-1.5">
        {FILTERS.map((f) => (
          <button
            key={f.id}
            type="button"
            onClick={() => setFilter(f.id)}
            className={[
              'rounded-full px-3 py-1.5 text-cap transition-colors',
              filter === f.id
                ? 'bg-surface text-ink'
                : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
            ].join(' ')}
          >
            {f.label}
          </button>
        ))}
        {/* 总数与「只看我的」跟在**同一个** `ml-auto` 后面：窄屏换行时两者一起
            被推到行尾，而不是只剩一个勾选框孤零零挂在右边。 */}
        <span className="ml-auto text-cap text-ink-4 tabular-nums">
          {data ? `${data.total} 条` : ''}
        </span>
        {/* 「只看我的」只对 admin 有意义 —— 普通用户拿到的本来就只有自己的 */}
        {isAdmin && (
          <label className="flex cursor-pointer items-center gap-2 text-cap text-ink-3">
            <input
              type="checkbox"
              checked={mineOnly}
              onChange={(e) => setMineOnly(e.target.checked)}
              className="size-3.5 accent-[#ef6b3c]"
            />
            只看我的
          </label>
        )}
      </div>

      {err && (
        <p className="mb-3 rounded-lg bg-accent-soft px-4 py-2.5 text-cap text-accent">{err}</p>
      )}

      {error ? (
        <p className="rounded-xl bg-surface py-10 text-center text-nav text-ink-3">{error}</p>
      ) : loading ? (
        <p className="py-10 text-center text-nav text-ink-4">读取中…</p>
      ) : items.length === 0 ? (
        /* 空状态写得具体点：区分「筛出来是空的」与「压根没人点过」，
           否则用户会以为筛选坏了。 */
        <div className="rounded-xl bg-surface px-6 py-12 text-center">
          <p className="text-nav text-ink-3">
            {filter === 'all' && !mineOnly
              ? '还没有人点歌。'
              : mineOnly
                ? '你没有符合这个筛选的请求。'
                : '这个状态下没有请求。'}
          </p>
          {filter === 'all' && !mineOnly && (
            <button
              type="button"
              onClick={() => setComposing(true)}
              className="mt-4 rounded-full bg-accent px-4 py-2 text-nav font-medium text-white transition-colors hover:brightness-110"
            >
              点一首
            </button>
          )}
        </div>
      ) : (
        /* 一个容器 + 内部行，行间用淡内嵌阴影分隔 —— 与用户管理页同一套写法。
           独立卡片一排浮在暗色上更像「面板」，行式列表才像「页面里的列表」。 */
        <div className="overflow-hidden rounded-xl bg-surface">
          {items.map((r, i) => (
            <RequestRow
              key={r.id}
              req={r}
              isAdmin={isAdmin}
              busy={busy}
              first={i === 0}
              action={action?.id === r.id ? action.kind : null}
              onAction={(kind) => setAction({ id: r.id, kind })}
              onCancel={() => setAction(null)}
              onProcess={() => run(() => api.requests.update(r.id, { status: 'processing' }))}
              onReject={(reason) =>
                run(() => api.requests.update(r.id, { status: 'rejected', reject_reason: reason }))
              }
              onLink={(songId) => run(() => api.requests.link(r.id, songId))}
            />
          ))}
        </div>
      )}

      {composing && (
        <RequestSongDialog
          onClose={() => setComposing(false)}
          // 提交后刷新：新请求（或新票数）要立刻出现在列表里
          onDone={() => reload()}
        />
      )}
    </OverlayShell>
  );
}

/** 状态徽标。四个状态四句话 —— 只用一个颜色点，用户看不出「待处理」还是「已拒绝」。 */
function StatusChip({ status }: { status: RequestStatus }) {
  const [label, cls] = {
    pending: ['待处理', 'bg-surface text-ink-2'],
    processing: ['处理中', 'bg-accent-soft text-accent'],
    done: ['已添加', 'bg-surface text-ink-4'],
    rejected: ['已拒绝', 'bg-surface text-ink-4'],
  }[status];
  return <span className={['rounded-full px-2.5 py-1 text-micro', cls].join(' ')}>{label}</span>;
}

function RequestRow({
  req,
  isAdmin,
  busy,
  first,
  action,
  onAction,
  onCancel,
  onProcess,
  onReject,
  onLink,
}: {
  req: SongRequest;
  isAdmin: boolean;
  busy: boolean;
  /** 第一行不画分隔线。 */
  first: boolean;
  action: 'reject' | 'link' | null;
  onAction: (kind: 'reject' | 'link') => void;
  onCancel: () => void;
  onProcess: () => void;
  onReject: (reason: string) => void;
  onLink: (songId: number) => void;
}) {
  const [reason, setReason] = useState('');
  const settled = req.status === 'done' || req.status === 'rejected';

  return (
    <div
      className={[
        'px-4 py-3.5 transition-colors hover:bg-surface-hover',
        // 行分隔用**内嵌阴影**而不是 border：深色底上 border 比行背景更亮，
        // 会看成一条亮线（项目里表格那套写法，见 docs/design.md §6.2）
        first ? '' : 'shadow-[inset_0_1px_0_var(--color-line-weak)]',
      ].join(' ')}
    >
      <div className="flex items-baseline gap-3">
        <span className="min-w-0 flex-1 truncate text-lead text-ink">{req.title}</span>
        {/* 票数 = 有多少人想要。新建时就是 1，所以「1 人」是正常状态，不是异常。 */}
        <span className="shrink-0 text-cap text-ink-3 tabular-nums">
          {req.vote_count} 人想要
        </span>
      </div>
      <div className="mt-0.5 text-note text-ink-3">
        {req.artist || '（没填歌手）'}
        {req.album ? ` · ${req.album}` : ''}
      </div>
      {req.note && <div className="mt-1.5 text-note text-ink-3">备注：{req.note}</div>}
      {req.status === 'rejected' && req.reject_reason && (
        <div className="mt-1.5 text-note text-ink-4">拒绝理由：{req.reject_reason}</div>
      )}
      {req.song_id != null && req.status === 'done' && (
        <div className="mt-1.5 text-cap text-ink-4">已关联到曲库里的歌曲 #{req.song_id}</div>
      )}

      <div className="mt-2.5 flex flex-wrap items-center gap-2">
        <StatusChip status={req.status} />
        {isAdmin && !settled && action === null && (
          <>
            {req.status === 'pending' && (
              <Act onClick={onProcess} disabled={busy}>
                开始处理
              </Act>
            )}
            <Act onClick={() => onAction('link')} disabled={busy}>
              已有这首歌，关联
            </Act>
            <Act onClick={() => onAction('reject')} disabled={busy}>
              拒绝
            </Act>
          </>
        )}
      </div>

      {action === 'reject' && (
        <div className="mt-2.5 flex flex-col gap-2">
          <input
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder="拒绝理由（必填，提交人看得到）"
            className="h-9 w-full rounded-lg border-0 bg-surface-hover px-3 text-note text-ink outline-0 placeholder:text-ink-4"
          />
          <div className="flex gap-2">
            <Act onClick={() => onReject(reason.trim())} disabled={busy || !reason.trim()} danger>
              确认拒绝
            </Act>
            <Act onClick={onCancel}>取消</Act>
          </div>
        </div>
      )}

      {action === 'link' && (
        <div className="mt-2.5">
          <p className="mb-2 text-cap text-ink-4">
            搜一首已经在库里的歌 —— 关联后这条请求会直接标成「已添加」
          </p>
          <SongPicker busy={busy} onPick={onLink} onCancel={onCancel} />
        </div>
      )}
    </div>
  );
}

/** 小动作按钮。列表里要轻 —— 圆角胶囊 + 12px 字，别用实心大按钮抢视线。 */
function Act({
  children,
  onClick,
  disabled,
  danger,
}: {
  children: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={[
        'rounded-full px-3 py-1.5 text-cap transition-colors disabled:opacity-40',
        danger
          ? 'bg-accent-soft text-accent hover:brightness-125'
          : 'bg-surface-hover text-ink-2 hover:brightness-125',
      ].join(' ')}
    >
      {children}
    </button>
  );
}

/**
 * 关联歌曲用的即时搜索。
 *
 * 从**曲库**搜（`/api/search`），不是让管理员手输 song_id —— id 是内部编号，
 * 没有人记得住。250ms 防抖：不然打「七里香」会发三次请求。
 */
function SongPicker({
  busy,
  onPick,
  onCancel,
}: {
  busy: boolean;
  onPick: (songId: number) => void;
  onCancel: () => void;
}) {
  const [kw, setKw] = useState('');
  const [hits, setHits] = useState<Song[]>([]);
  const [searching, setSearching] = useState(false);

  const onKw = useCallback((v: string) => {
    setKw(v);
    const q = v.trim();
    if (!q) {
      setHits([]);
      return;
    }
    setSearching(true);
    // 防抖放在这里而不是 useEffect：`useAsync` 不适合「打字触发」的场景
    // （deps 一变就重取，没有等待窗口）。
    window.setTimeout(() => {
      api
        .search(q, { page_size: 6 })
        .then((r) => setHits(r.items))
        // 搜失败就当没结果 —— 这是辅助选择器，不值得为它弹错误条
        .catch(() => setHits([]))
        .finally(() => setSearching(false));
    }, 250);
  }, []);

  return (
    <>
      <input
        autoFocus
        value={kw}
        onChange={(e) => onKw(e.target.value)}
        placeholder="搜歌名 / 歌手 / 专辑"
        className="h-9 w-full rounded-lg border-0 bg-surface-hover px-3 text-note text-ink outline-0 placeholder:text-ink-4"
      />
      {kw.trim() && (
        <div className="mt-2 rounded-lg bg-black/25 p-1">
          {hits.length === 0 ? (
            <p className="px-3 py-2 text-cap text-ink-4">
              {searching ? '搜索中…' : '没有匹配的歌曲'}
            </p>
          ) : (
            hits.map((s) => (
              <button
                key={s.id}
                type="button"
                disabled={busy}
                onClick={() => onPick(s.id)}
                className="flex w-full items-baseline gap-3 rounded-md px-3 py-2 text-left transition-colors hover:bg-surface-hover disabled:opacity-40"
              >
                <span className="min-w-0 flex-1 truncate text-note text-ink-2">
                  {s.title ?? '（无标题）'}
                </span>
                <span className="min-w-0 shrink-0 truncate text-cap text-ink-4">
                  {s.artists ?? ''}
                </span>
              </button>
            ))
          )}
        </div>
      )}
      <button
        type="button"
        onClick={onCancel}
        className="mt-2 text-cap text-ink-4 transition-colors hover:text-ink-2"
      >
        取消
      </button>
    </>
  );
}
