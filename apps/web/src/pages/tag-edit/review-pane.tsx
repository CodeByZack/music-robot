/**
 * 改动预览拆成两个组件：**详情**（`ReviewDetail`）与**动作栏**（`ActionBar`）。
 *
 * ## 为什么要拆
 *
 * 窄屏时这两者的诉求是相反的：
 *
 * * **动作**（预览 / 写入 / 备份）是这个页面的出口，**必须常显**；
 * * **详情**（diff 列表、空状态提示）只在需要时看，平时常开着纯占地方
 *   —— 实测在 450px 宽的手机上，"改动预览 + 一行提示 + 备份 + 两个按钮"
 *   加起来接近 190px，把编辑字段挤掉了半屏。
 *
 * 所以窄屏把详情做成**可折叠**、动作留在下面一条约 80px 的栏里；
 * 宽屏（≥1100px）两者仍是右栏的上下两段，看起来和以前一样。
 *
 * ## 四态
 *
 * | 状态 | 含义 |
 * |---|---|
 * | `clean` | 还没改任何字段 |
 * | `unpreviewed` | 改了但没点过「预览改动」 |
 * | `stale` | 预览过、之后字段又改了（预览作废） |
 * | `ready` | 预览对应当前改动 |
 *
 * 写文件不可撤销，所以这个状态机是页面最重要的一条约束 ——
 * 它必须在**窄屏折叠后也看得见**，这就是 `reviewChip` 存在的理由
 * （折叠时那一行显示「未预览 / 预览已过期 / 已预览 3 处」）。
 */
import type { TagDiff, TagPatchResult } from '@music-robot/core';

/** diff 的 key（后端 `diff_fields` 产出的英文名）→ 界面标签。 */
const FIELD_LABEL: Record<string, string> = {
  title: '标题',
  artist: '歌手',
  album: '专辑',
  albumArtist: '专辑艺术家',
  track: '音轨',
  disc: '碟号',
  discTotal: '碟号总数',
  year: '年份',
  genre: '流派',
  composer: '作曲',
  comment: '注释',
  lyrics: '歌词',
  lyricsTimed: '同步歌词',
  cover: '封面',
};

export type ReviewState = 'clean' | 'unpreviewed' | 'stale' | 'ready';

/** 由三个输入推出状态。**唯一一处**算这个的地方，两个组件都用它。 */
export function reviewStateOf(
  dirty: boolean,
  fresh: boolean,
  preview: TagPatchResult | null,
): ReviewState {
  if (!dirty) return 'clean';
  if (preview !== null && !fresh) return 'stale';
  if (fresh) return 'ready';
  return 'unpreviewed';
}

/** 改动条数（封面单独算一条：引擎的 diff 只比张数，换封面是 1 → 1、没有 diff 行）。 */
export function changeCount(preview: TagPatchResult | null): number {
  if (!preview) return 0;
  return preview.diffs.length + (preview.cover_op ? 1 : 0);
}

/** 空状态那一句（详情区里显示）。 */
const STATE_HINT: Record<Exclude<ReviewState, 'ready'>, string> = {
  clean: '改点东西，这里会逐行告诉你将写入什么。',
  unpreviewed: '点下面的「预览改动」，先看清会写成什么。',
  stale: '字段又改了，之前的预览已过期 —— 重新点一次「预览改动」。',
};

/** 折叠时那一行的短文案。 */
export function reviewChip(state: ReviewState, count: number, applied: boolean): string {
  if (state === 'clean') return '未改动';
  if (state === 'unpreviewed') return '未预览';
  if (state === 'stale') return '预览已过期';
  return applied ? '已写入文件' : `已预览 ${count} 处`;
}

function DiffRow({ label, before, after }: { label: string; before: string; after: string }) {
  return (
    <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 py-1 text-note">
      <span className="w-[68px] shrink-0 text-ink-3">{label}</span>
      <span className="text-ink-4 line-through">{before}</span>
      <span className="text-ink-4">→</span>
      <span className="min-w-0 break-all text-accent">{after}</span>
    </div>
  );
}

/** 详情：标题 + 逐行 diff（没得看时显示一句状态说明）。 */
export function ReviewDetail({
  preview,
  state,
}: {
  preview: TagPatchResult | null;
  state: ReviewState;
}) {
  const ready = state === 'ready';
  const count = changeCount(preview);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-line-weak px-4 py-3">
        <b className="text-note font-medium">改动预览</b>
        {ready && (
          <span className="text-cap text-ink-4">
            {preview?.applied ? '已写入文件' : `共 ${count} 处`}
          </span>
        )}
      </div>

      {/* diff 区（可滚动，免得字段多时把动作栏挤出可视区） */}
      <div className="min-h-0 flex-1 overflow-auto px-4 py-3">
        {ready && preview ? (
          <>
            {preview.diffs.map((d: TagDiff) => (
              <DiffRow key={d.key} label={FIELD_LABEL[d.key] ?? d.key} before={d.before} after={d.after} />
            ))}
            {/*
              封面单独一行的原因：引擎的 diff 只比**张数**，换一张封面是新旧都是 1 张、
              diffs 里根本没有这一行。不单独显示的话，只换封面的场景预览会是一片空白，
              用户看不出会发生什么。
            */}
            {preview.cover_op && (
              <DiffRow
                label="封面"
                before={preview.cover_op === 'remove' ? '原有封面' : '原封面'}
                after={preview.cover_op === 'remove' ? '移除' : '换成新选的图'}
              />
            )}
          </>
        ) : (
          <p className="text-cap leading-5 text-ink-4">
            {state === 'clean' ? STATE_HINT.clean : state === 'stale' ? STATE_HINT.stale : STATE_HINT.unpreviewed}
          </p>
        )}
      </div>
    </div>
  );
}

/**
 * 动作栏：反馈 + 备份 + 两个按钮（+ 窄屏的折叠开关）。
 *
 * `collapsible` 为真时（窄屏），额外渲染一行「状态短文案 + 详情开关」；
 * 宽屏那一行不渲染（详情本来就常显，没有可折叠的东西）。
 */
export function ActionBar({
  state,
  preview,
  busy,
  dirty,
  fresh,
  backup,
  onBackup,
  onPreview,
  onWrite,
  invalid,
  notice,
  failure,
  expanded,
  onToggleExpanded,
}: {
  state: ReviewState;
  preview: TagPatchResult | null;
  busy: boolean;
  dirty: boolean;
  fresh: boolean;
  backup: boolean;
  onBackup: (v: boolean) => void;
  onPreview: () => void;
  onWrite: () => void;
  /** 非负整数校验失败的字段标签。 */
  invalid: string[];
  notice: string | null;
  failure: string | null;
  expanded: boolean;
  onToggleExpanded: () => void;
}) {
  return (
    <div className="shrink-0 border-t border-line-weak px-4 py-3">
      {failure && (
        <p className="mb-2.5 rounded-md bg-accent-soft px-3 py-2 text-cap leading-4 text-accent">{failure}</p>
      )}
      {notice && (
        <p className="mb-2.5 rounded-md bg-black/25 px-3 py-2 text-cap leading-4 text-ink-2">{notice}</p>
      )}

      {/* 这一行**只在窄屏渲染**（`min-[1100px]:hidden`）—— 宽屏详情常显，没有可折叠的东西。
          用纯 CSS 控制而不读 JS 媒体查询：少一个会重渲染的状态，也少一处不一致。 */}
      <button
        type="button"
        onClick={onToggleExpanded}
        aria-expanded={expanded}
        className="mb-2 flex w-full items-center gap-2 rounded-md bg-black/20 px-2.5 py-1.5 text-left text-cap text-ink-3 transition-colors hover:bg-black/30 min-[1100px]:hidden"
      >
        {/* 折叠时这一行是**四态唯一的落点** —— 写文件不可撤销，状态必须看得见，
            不能只藏在按钮的 disabled 里。 */}
        <span className={state === 'ready' || state === 'stale' ? 'text-ink-2' : ''}>
          {reviewChip(state, changeCount(preview), preview?.applied ?? false)}
        </span>
        <span className="flex-1" />
        <span>{expanded ? '收起详情' : '查看详情'}</span>
        <svg
          className={`ico-xs transition-transform ${expanded ? 'rotate-180' : ''}`}
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth={1.8}
          strokeLinecap="round"
        >
          <path d="M3.5 6 8 10.5 12.5 6" />
        </svg>
      </button>

      <label className="mb-2.5 flex cursor-pointer items-center gap-2 text-cap text-ink-2">
        <input
          type="checkbox"
          checked={backup}
          onChange={(e) => onBackup(e.target.checked)}
          className="accent-accent"
        />
        写入前备份 .bak
      </label>

      {invalid.length > 0 && (
        <p className="mb-2 text-cap leading-4 text-accent">{invalid.join('、')} 需要非负整数</p>
      )}

      <div className="flex gap-2">
        <button
          type="button"
          disabled={busy || !dirty}
          onClick={onPreview}
          className="h-9 flex-1 rounded-full bg-surface px-3 text-note transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-40"
        >
          {busy ? '处理中…' : '预览改动'}
        </button>
        <button
          type="button"
          // 必须先预览过、且预览结果对应当前改动，才能写 —— 这是这个页面最重要的一条约束
          disabled={busy || !fresh || !preview?.changed}
          onClick={onWrite}
          className="h-9 flex-1 rounded-full bg-accent px-3 text-note font-medium text-white transition-colors hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
        >
          写入文件
        </button>
      </div>
    </div>
  );
}
