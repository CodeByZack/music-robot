import { useCallback, useState } from 'react';
import type { RequestStatus, Song, SongRequest } from '@music-robot/core';
import { OverlayShell, type OverlaySection } from '@/components/overlay.tsx';
import { Panel, PanelRow } from '@/components/panel.tsx';
import { RequestSongDialog } from '@/components/request-dialog.tsx';
import { api } from '@/lib/client.ts';
import { messageOf, useSession } from '@/lib/session.tsx';
import { useAsync } from '@/lib/use-async.tsx';

/**
 * 点歌请求页（全屏浮层页面，**骨架与设置页一致**：顶栏 + 左侧分节 + 右侧 Panel）。
 *
 * 入口有两个（都不是本页自己发的）：
 * 1. 顶栏右上角的下拉菜单（面向所有人）；
 * 2. 搜索页搜不到时的「请求这首歌」（面向需求发生的当场）。
 * 两条都通到这里 —— 但真正的「提交」用的是同一个 `RequestSongDialog`（弹窗）。
 *
 * **左侧分节 = 状态筛选**。这是本页最自然的分法：用户来这儿想看的是
 * 「哪些还没处理 / 哪些已经被拒了」，那就该是分节，而不是顶上排一排筛选小按钮
 * —— 设置页导航存在的理由正是把「看哪一块」显式化。筛选走后端 `?status=`。
 *
 * 权限（后端 `routes::requests` 强制）：普通用户无论带什么 query 都只看得到
 * **自己提交的**；`?status=` 这种全量筛选是管理端能力，普通用户调会 403 ——
 * 所以分节导航只对 admin 显示，普通用户只有一个「我的请求」分节。
 */
const SECTIONS: OverlaySection[] = [
  { id: 'all', label: '全部请求' },
  { id: 'pending', label: '待处理' },
  { id: 'processing', label: '处理中' },
  { id: 'done', label: '已添加' },
  { id: 'rejected', label: '已拒绝' },
];

export default function RequestsPage() {
  const { user } = useSession();
  const isAdmin = user?.role === 'admin';
  const [section, setSection] = useState<string>('all');
  const [mineOnly, setMineOnly] = useState(false);
  const [composing, setComposing] = useState(false);

  const load = useCallback(
    () =>
      api.requests.list({
        ...(section === 'all' ? {} : { status: section as RequestStatus }),
        ...(mineOnly ? { mine: 1 as const } : {}),
      }),
    [section, mineOnly],
  );
  const { data, error, loading, reload } = useAsync(load, [section, mineOnly]);

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
  // 分节表是常量且一定非空，所以 `?? SECTIONS[0]` 兜底后必是真值；
  // 写 `!` 而不是再深一层判空 —— 这里没有运行时风险，只是 TS 推不出来。
  const current = SECTIONS.find((s) => s.id === section) ?? SECTIONS[0]!;

  return (
    <OverlayShell
      title="点歌请求"
      sections={isAdmin ? SECTIONS : [{ id: 'all', label: '我的请求' }]}
      section={section}
      onSection={(id) => {
        setSection(id);
        setAction(null); // 换一节就把展开的表单收掉，别让它挂在新列表上
      }}
      actions={
        <button
          type="button"
          onClick={() => setComposing(true)}
          className="h-8 shrink-0 rounded-full bg-accent px-3.5 text-cap font-medium text-white transition-colors hover:brightness-110"
        >
          点一首
        </button>
      }
    >
      {err && (
        <p className="mb-4 rounded-md bg-accent-soft px-4 py-3 text-note text-accent">{err}</p>
      )}

      <Panel title={current.label}>
        {/* 「只看我的」是**范围**而不是状态，所以不做成分节，而是这块面板里的一个开关
            —— 与设置页那个「写入文件」开关同一个写法（含 hint 说清当前含义）。
            只对 admin 显示：普通用户拿到的本来就只有自己的。 */}
        {isAdmin && (
          <PanelRow
            label="只看我的"
            hint={mineOnly ? '只显示你自己提交的请求' : '显示所有人提交的请求'}
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

        {error ? (
          <p className="py-8 text-center text-nav text-ink-3">{error}</p>
        ) : loading ? (
          <p className="py-8 text-center text-nav text-ink-4">读取中…</p>
        ) : items.length === 0 ? (
          /* 空状态写得具体点：区分「这一节是空的」与「压根没人点过」，
             否则用户会以为筛选坏了。 */
          <p className="py-8 text-center text-nav text-ink-4">
            {section === 'all' && !mineOnly
              ? isAdmin
                ? '还没有人点歌。'
                : '你还没有提交过点歌请求。'
              : mineOnly
                ? '你没有符合这一节的请求。'
                : '这一节里没有请求。'}
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
          // 提交后刷新：新请求（或新票数）要立刻出现在列表里
          onDone={() => {
            reload();
            // 新建的请求一定是「待处理」，切过去让用户看得见它落在哪儿
            // （普通用户没有这个分节，所以只在 admin 下切）
            if (isAdmin) setSection('pending');
          }}
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

/**
 * 一条请求。结构与设置页的 `PanelRow` 同形（**左边文字块、右边控件**），
 * 只是左边多几行、右边从单个控件变成一组。没直接用 `PanelRow`，是因为
 * 就地展开的操作区（拒绝理由 / 选歌）要占满整行宽度。
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
            <span className="block text-xs text-ink-4">
              已关联到曲库里的歌曲 #{req.song_id}
            </span>
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
