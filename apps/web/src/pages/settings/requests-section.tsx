import { useCallback, useState } from 'react';
import type { RequestStatus, Song, SongRequest } from '@music-robot/core';
import { Panel, PanelRow } from '@/components/panel.tsx';
import { RequestSongDialog } from '@/components/request-dialog.tsx';
import { api } from '@/lib/client.ts';
import { messageOf, useSession } from '@/lib/session.tsx';
import { useAsync } from '@/lib/use-async.tsx';

/**
 * 设置 → 点歌请求（**是设置页的一个分节，不是独立页面**）。
 *
 * 用户 2026-10-02 要求并入设置页。理由也成立：它是「偶尔来一下」的管理动作，
 * 和「音乐库 / 播放 / 账号」同一层级，单独占一个全屏页面反而过重。
 * 需求侧入口仍在**搜索页**（搜不到 → 「请求这首歌」）。
 *
 * 权限（后端 `routes::requests` 强制）：普通用户无论带什么 query 都只看得到
 * **自己提交的**；`?status=` 这种全量筛选是管理端能力，普通用户调会 403 ——
 * 所以状态筛选只对 admin 显示。
 */
const FILTERS: { id: 'all' | RequestStatus; label: string }[] = [
  { id: 'all', label: '全部' },
  // ⚠️ `pending` 是后端四个状态之一（新请求的初始值，`pending → processing` 是状态机的
  // 起点），**不是**可以省掉的一档。标签写「已收到」而不是后端注释里的「待处理」：
  // 「待处理」读起来像积压的任务，它的实际语义是「收到了、还没开始」——
  // 用户 2026-10-02 就是被这个词绊住的（以为没有这个状态）。
  { id: 'pending', label: '已收到' },
  { id: 'processing', label: '处理中' },
  { id: 'done', label: '已添加' },
  { id: 'rejected', label: '已拒绝' },
];

export function RequestsSection() {
  const { user } = useSession();
  const isAdmin = user?.role === 'admin';
  const [filter, setFilter] = useState<'all' | RequestStatus>('all');
  const [mineOnly, setMineOnly] = useState(false);
  const [composing, setComposing] = useState(false);

  const load = useCallback(
    () =>
      api.requests.list({
        ...(filter === 'all' || !isAdmin ? {} : { status: filter }),
        ...(mineOnly ? { mine: 1 as const } : {}),
      }),
    [filter, mineOnly, isAdmin],
  );
  const { data, error, loading, reload } = useAsync(load, [filter, mineOnly]);

  // 当前展开的就地操作（一次只开一条 —— 同时开两个输入框很难看）
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
    <>
      <Panel
        title={isAdmin ? '点歌请求' : '我的点歌'}
        // 筛选与主操作放标题行右侧 —— 与设置页其余面板「块标题 + 右侧控件」同形
        actions={
          <>
            {/* 状态筛选对普通用户无意义（后端只给他自己的、且拒绝全量筛选） */}
            {isAdmin && (
              <div className="flex flex-wrap gap-1">
                {FILTERS.map((f) => (
                  <button
                    key={f.id}
                    type="button"
                    onClick={() => setFilter(f.id)}
                    className={[
                      'rounded-full px-3 py-1.5 text-cap transition-colors',
                      filter === f.id
                        ? 'bg-surface-press text-ink'
                        : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
                    ].join(' ')}
                  >
                    {f.label}
                  </button>
                ))}
              </div>
            )}
            <button
              type="button"
              onClick={() => setComposing(true)}
              className="h-[34px] shrink-0 rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110"
            >
              点一首
            </button>
          </>
        }
      >
        {/* 「只看我的」是**范围**而不是状态，所以不做成筛选按钮，而是这块面板里的
            一行开关 —— 与「音乐库」那块里「写入文件」同一个写法。只对 admin 显示。 */}
        {isAdmin && (
          <PanelRow
            label="只看我的"
            hint={
              mineOnly
                ? '只显示你自己提交的请求'
                : '显示所有人提交的请求（按想要的人数排序）'
            }
          >
            <button
              type="button"
              role="switch"
              aria-checked={mineOnly}
              aria-label="只看我的"
              onClick={() => setMineOnly((v) => !v)}
              className={[
                'relative h-[23px] w-10 shrink-0 rounded-full transition-colors',
                mineOnly ? 'bg-accent' : 'bg-white/16',
              ].join(' ')}
            >
              <span
                className={[
                  'absolute top-[3px] left-[3px] size-[17px] rounded-full bg-ink transition-transform',
                  mineOnly ? 'translate-x-[17px]' : '',
                ].join(' ')}
              />
            </button>
          </PanelRow>
        )}

        {err && (
          <p className="mb-3 rounded-md bg-accent-soft px-4 py-2.5 text-note text-accent">{err}</p>
        )}

        {error ? (
          <p className="py-8 text-center text-nav text-ink-3">{error}</p>
        ) : loading ? (
          <p className="py-8 text-center text-nav text-ink-4">读取中…</p>
        ) : items.length === 0 ? (
          /* 空状态写得具体点：区分「这一档是空的」与「压根没人点过」，
             否则用户会以为筛选坏了。 */
          <p className="py-8 text-center text-nav text-ink-4">
            {filter === 'all' && !mineOnly
              ? isAdmin
                ? '还没有人点歌。'
                : '你还没有提交过点歌请求。'
              : mineOnly
                ? '你没有符合这一档的请求。'
                : '这一档里没有请求。'}
          </p>
        ) : (
          items.map((r) => (
            <RequestRow
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
      </Panel>

      {composing && (
        <RequestSongDialog
          onClose={() => setComposing(false)}
          // 提交后刷新 + 切到「待处理」：新建的请求一定落在那一档，让用户看得见它
          onDone={() => {
            reload();
            if (isAdmin) setFilter('pending');
          }}
        />
      )}
    </>
  );
}

/** 状态徽标。四个状态四句话 —— 只用一个颜色点，用户看不出「已收到」还是「已拒绝」。 */
function StatusChip({ status }: { status: RequestStatus }) {
  const [label, cls] = {
    // 与筛选标签同一套措辞（见上方 `FILTERS`）—— 同一状态在两处叫不同名字最容易被误解
    pending: ['已收到', 'bg-surface-hover text-ink-2'],
    processing: ['处理中', 'bg-accent-soft text-accent'],
    done: ['已添加', 'bg-surface-hover text-ink-4'],
    rejected: ['已拒绝', 'bg-surface-hover text-ink-4'],
  }[status];
  return <span className={['rounded-full px-2.5 py-1 text-micro', cls].join(' ')}>{label}</span>;
}

/**
 * 一条请求。结构与设置页的 `PanelRow` 同形（**左边文字块、右边控件**），
 * 只是左边多几行、右边是一组按钮。没直接用 `PanelRow`，是因为就地展开的操作区
 * （拒绝理由 / 选歌）要占满整行宽度。
 */
function RequestRow({
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
    <div className="border-b border-line-weak py-3.5 last:border-b-0">
      <div className="flex items-start gap-3.5">
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2.5">
            <b className="truncate text-nav font-normal text-ink">{req.title}</b>
            <StatusChip status={req.status} />
          </div>
          <span className="block text-xs text-ink-3">
            {req.artist || '（没填歌手）'}
            {req.album ? ` · ${req.album}` : ''}
          </span>
          {req.note && <span className="block text-xs text-ink-3">备注：{req.note}</span>}
          {req.status === 'rejected' && req.reject_reason && (
            <span className="block text-xs text-ink-4">拒绝理由：{req.reject_reason}</span>
          )}
          {req.song_id != null && req.status === 'done' && (
            <span className="block text-xs text-ink-4">已关联到曲库里的歌曲 #{req.song_id}</span>
          )}
        </div>

        <div className="flex shrink-0 flex-col items-end gap-2">
          {/* 票数 = 有多少人想要。新建时就是 1，所以「1 人」是正常状态，不是异常。 */}
          <span className="text-cap text-ink-4 tabular-nums">{req.vote_count} 人想要</span>
          {isAdmin && !settled && action === null && (
            <div className="flex flex-wrap justify-end gap-1.5">
              {req.status === 'pending' && (
                <Act onClick={onProcess} disabled={busy}>
                  开始处理
                </Act>
              )}
              <Act onClick={() => onAction('link')} disabled={busy}>
                已有，关联
              </Act>
              <Act onClick={() => onAction('reject')} disabled={busy}>
                拒绝
              </Act>
            </div>
          )}
        </div>
      </div>

      {action === 'reject' && (
        <div className="mt-3 flex flex-wrap items-center gap-2">
          <input
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder="拒绝理由（必填，提交人看得到）"
            className="h-8 min-w-[200px] flex-1 rounded-md border-0 bg-surface-hover px-3 text-note text-ink outline-0 placeholder:text-ink-4"
          />
          <Act onClick={() => onReject(reason.trim())} disabled={busy || !reason.trim()} danger>
            确认拒绝
          </Act>
          <Act onClick={onCancel}>取消</Act>
        </div>
      )}

      {action === 'link' && (
        <div className="mt-3">
          <p className="mb-2 text-xs text-ink-3">
            搜一首已经在库里的歌 —— 关联后这条请求会直接标成「已添加」
          </p>
          <SongPicker busy={busy} onPick={onLink} onCancel={onCancel} />
        </div>
      )}
    </div>
  );
}

/** 小动作按钮。放在行右侧，必须轻 —— 胶囊 + 12px 字，别用实心大按钮抢视线。 */
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
        'shrink-0 rounded-full px-3 py-1.5 text-cap transition-colors disabled:opacity-40',
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
    // 防抖放在事件里而不是 useEffect：`useAsync` 是「deps 一变就重取」，
    // 没有等待窗口，不适合「打字触发」的场景。
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
        className="h-9 w-full max-w-[420px] rounded-lg border-0 bg-surface-hover px-3 text-note text-ink outline-0 placeholder:text-ink-4"
      />
      {kw.trim() && (
        <div className="mt-2 max-w-[420px] rounded-lg bg-black/25 p-1">
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
