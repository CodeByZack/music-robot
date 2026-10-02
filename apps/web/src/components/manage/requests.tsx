import { useCallback, useEffect, useState } from 'react';
import type { RequestStatus, Song, SongRequest } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { messageOf, useSession } from '@/lib/session.tsx';
import { useAsync } from '@/lib/use-async.tsx';

/**
 * 点歌请求的管理面板（**顶栏右上角 popover 里**，不占页面）。
 *
 * 为什么不做成独立页面：这个面板是「顺手处理一下」的东西 —— 有人点歌了就去点两下，
 * 没事的时候它不该占导航里的一格。需求侧的入口在**搜索页**（搜不到 → 请求这首歌），
 * 两边各司其职。
 *
 * 权限：普通用户只能看到**自己提交的**（后端强制，`user_id` 只来自令牌）；
 * 「全部」这个切换只对 admin 显示，且 `?status=` 过滤是管理端能力（普通用户调会 403）。
 */
export function RequestsPanel() {
  const { user } = useSession();
  const isAdmin = user?.role === 'admin';
  /** 普通用户没有「全部」可选 —— 后端也不会给他。 */
  const [scope, setScope] = useState<'all' | 'mine'>('all');

  const load = useCallback(
    () => api.requests.list(scope === 'mine' ? { mine: 1 } : {}),
    [scope],
  );
  const { data, error, loading, reload } = useAsync(load, [scope]);

  // 当前展开的「就地操作」。一次只展开一条 —— 列表本来就窄，同时开两个会很难看。
  const [action, setAction] = useState<{ id: number; kind: 'reject' | 'link' } | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  /** 所有动作都走这里：报错就地显示，成功后重拉列表（状态/票数都会变）。 */
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
    <div className="flex max-h-[min(72vh,560px)] flex-col">
      {/* 工具栏。**标题不在这里** —— 标题（或 tab）由外层 `ManageMenu` 给，
          免得一个 360px 宽的浮层里连着出现两遍「点歌请求」。 */}
      <div className="flex shrink-0 items-center gap-2 border-b border-line-weak px-3.5 py-2">
        {isAdmin && (
          <div className="flex rounded-full bg-surface p-0.5">
            {(['all', 'mine'] as const).map((s) => (
              <button
                key={s}
                type="button"
                onClick={() => setScope(s)}
                className={[
                  'rounded-full px-2.5 py-1 text-cap transition-colors',
                  scope === s ? 'bg-surface-press text-ink' : 'text-ink-3 hover:text-ink',
                ].join(' ')}
              >
                {s === 'all' ? '全部' : '我的'}
              </button>
            ))}
          </div>
        )}
        <span className="flex-1" />
        <span className="text-cap text-ink-4 tabular-nums">{data ? `${data.total} 条` : ''}</span>
      </div>

      {err && (
        <p className="shrink-0 border-b border-line-weak bg-accent-soft px-3.5 py-2 text-cap text-accent">
          {err}
        </p>
      )}

      <div className="min-h-0 flex-1 overflow-auto p-1.5">
        {error ? (
          <p className="px-2 py-6 text-nav text-ink-3">{error}</p>
        ) : loading ? (
          <p className="px-2 py-6 text-nav text-ink-4">读取中…</p>
        ) : items.length === 0 ? (
          <p className="px-2 py-6 text-nav text-ink-4">
            {scope === 'mine' ? '你还没有提交过点歌请求。' : '还没有人点歌。'}
          </p>
        ) : (
          items.map((r) => (
            <Row
              key={r.id}
              req={r}
              isAdmin={isAdmin}
              busy={busy}
              action={action?.id === r.id ? action.kind : null}
              onAction={(kind) => setAction({ id: r.id, kind })}
              onCancel={() => setAction(null)}
              onProcess={() => run(() => api.requests.update(r.id, { status: 'processing' }))}
              onReject={(reason) =>
                run(() => api.requests.update(r.id, { status: 'rejected', reject_reason: reason }))
              }
              onLink={(songId) => run(() => api.requests.link(r.id, songId))}
            />
          ))
        )}
      </div>
    </div>
  );
}

/** 状态徽标。四个状态四句话 —— 别只用一个颜色点，用户看不出「待处理」还是「已拒绝」。 */
function StatusChip({ status }: { status: RequestStatus }) {
  const [label, cls] = {
    pending: ['待处理', 'bg-surface text-ink-2'],
    processing: ['处理中', 'bg-accent-soft text-accent'],
    done: ['已添加', 'text-ink-4'],
    rejected: ['已拒绝', 'text-ink-4'],
  }[status];
  return (
    <span className={['rounded-full px-2 py-0.5 text-micro', cls].join(' ')}>{label}</span>
  );
}

function Row({
  req,
  isAdmin,
  busy,
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
    <div className="rounded-lg px-2 py-2 transition-colors hover:bg-surface">
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 flex-1 truncate text-nav text-ink">{req.title}</span>
        {/* 票数 = 有多少人想要。新建时就是 1，所以「1 人」是正常状态，不是异常。 */}
        <span className="shrink-0 text-cap text-ink-4 tabular-nums">{req.vote_count} 人</span>
      </div>
      <div className="mt-0.5 truncate text-cap text-ink-4">
        {req.artist || '（没填歌手）'}
        {req.album ? ` · ${req.album}` : ''}
      </div>
      {req.note && <div className="mt-1 text-cap text-ink-3">备注：{req.note}</div>}
      {req.status === 'rejected' && req.reject_reason && (
        <div className="mt-1 text-cap text-ink-4">理由：{req.reject_reason}</div>
      )}

      <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
        <StatusChip status={req.status} />

        {isAdmin && !settled && action === null && (
          <>
            {req.status === 'pending' && (
              <Act onClick={onProcess} disabled={busy}>
                开始处理
              </Act>
            )}
            <Act onClick={() => onAction('link')} disabled={busy}>
              关联歌曲
            </Act>
            <Act onClick={() => onAction('reject')} disabled={busy}>
              拒绝
            </Act>
          </>
        )}
      </div>

      {action === 'reject' && (
        <div className="mt-2 flex flex-col gap-1.5">
          <input
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder="拒绝理由（必填，会显示给提交人）"
            className="h-8 rounded-md border-0 bg-surface px-2.5 text-cap text-ink outline-0 placeholder:text-ink-4"
          />
          <div className="flex gap-2">
            <Act
              onClick={() => onReject(reason.trim())}
              disabled={busy || !reason.trim()}
              danger
            >
              确认拒绝
            </Act>
            <Act onClick={onCancel}>取消</Act>
          </div>
        </div>
      )}

      {action === 'link' && (
        <div className="mt-2">
          <p className="mb-1.5 text-micro text-ink-4">
            搜一首已在库里的歌 —— 关联后这条请求会直接标成「已添加」
          </p>
          <LinkPicker busy={busy} onPick={onLink} onCancel={onCancel} />
        </div>
      )}
    </div>
  );
}

/** 小动作按钮。面板很窄，按钮必须轻 —— 圆角胶囊 + 12px 字，别用实心大按钮。 */
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
        'rounded-full px-2.5 py-1 text-cap transition-colors disabled:opacity-40',
        danger
          ? 'bg-accent-soft text-accent hover:brightness-125'
          : 'bg-surface text-ink-2 hover:bg-surface-hover',
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
function LinkPicker({
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

  useEffect(() => {
    const q = kw.trim();
    if (!q) {
      setHits([]);
      return;
    }
    const t = setTimeout(() => {
      setSearching(true);
      api
        .search(q, { page_size: 6 })
        .then((r) => setHits(r.items))
        // 搜失败就当没结果 —— 这是个辅助选择器，不值得为它弹一个错误条
        .catch(() => setHits([]))
        .finally(() => setSearching(false));
    }, 250);
    return () => clearTimeout(t);
  }, [kw]);

  return (
    <>
      <input
        autoFocus
        value={kw}
        onChange={(e) => setKw(e.target.value)}
        placeholder="搜歌名 / 歌手 / 专辑"
        className="h-8 w-full rounded-md border-0 bg-surface px-2.5 text-cap text-ink outline-0 placeholder:text-ink-4"
      />
      {kw.trim() && (
        <div className="mt-1.5 rounded-md bg-black/25 p-1">
          {searching && hits.length === 0 ? (
            <p className="px-2 py-1.5 text-cap text-ink-4">搜索中…</p>
          ) : hits.length === 0 ? (
            <p className="px-2 py-1.5 text-cap text-ink-4">没有匹配的歌曲</p>
          ) : (
            hits.map((s) => (
              <button
                key={s.id}
                type="button"
                disabled={busy}
                onClick={() => onPick(s.id)}
                className="flex w-full items-baseline gap-2 rounded-sm px-2 py-1.5 text-left transition-colors hover:bg-surface-hover disabled:opacity-40"
              >
                <span className="min-w-0 flex-1 truncate text-cap text-ink-2">
                  {s.title ?? '（无标题）'}
                </span>
                <span className="min-w-0 shrink-0 truncate text-micro text-ink-4">
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
        className="mt-1.5 text-cap text-ink-4 transition-colors hover:text-ink-2"
      >
        取消
      </button>
    </>
  );
}
