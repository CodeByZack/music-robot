/**
 * 审查栏：**改动预览 + 备份开关 + 两个动作按钮**。
 *
 * ## 为什么这三样必须在一起
 *
 * 上一版把两个按钮钉在最底部的横条上，而预览结果渲染在滚动内容的最末尾（歌词下面）——
 * 用户点完「预览改动」得往下滚才看得见结果，看完再滚回底部点「写入文件」。
 * 操作与结果被隔开了。
 *
 * 现在整个审查栏固定在右侧（窄屏落到下方），点按钮**结果就出现在按钮正上方**。
 *
 * ## 四态提示（两步闸门的可视化）
 *
 * | 状态 | 显示 |
 * |---|---|
 * | 没改动 | 「改点东西，会在这里告诉你将写入什么」 |
 * | 改过但没预览 | 「点『预览改动』看看会写成什么」← 此时「写入文件」是灰的 |
 * | 预览过、字段又改了 | 「字段又改了，之前的预览已过期」← 同样是灰的 |
 * | 预览是最新的 | 逐行 diff + 「写入文件」可用 |
 *
 * 这个状态机是这个页面最重要的一条约束（写文件不可撤销），所以它得**看得见**，
 * 不能只藏在按钮的 disabled 里。
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

export function ReviewPane({
  preview,
  fresh,
  dirty,
  busy,
  backup,
  onBackup,
  onPreview,
  onWrite,
  invalid,
  notice,
  failure,
}: {
  preview: TagPatchResult | null;
  /** 预览是否仍对应当前改动（字段一改就过期）。 */
  fresh: boolean;
  dirty: boolean;
  busy: boolean;
  backup: boolean;
  onBackup: (v: boolean) => void;
  onPreview: () => void;
  onWrite: () => void;
  /** 非负整数校验失败的字段标签。 */
  invalid: string[];
  notice: string | null;
  failure: string | null;
}) {
  const showDiffs = preview !== null && fresh && (preview.diffs.length > 0 || preview.cover_op);
  const count = preview ? preview.diffs.length + (preview.cover_op ? 1 : 0) : 0;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* 面板标题 */}
      <div className="flex shrink-0 items-center gap-2 border-b border-line-weak px-4 py-3">
        <b className="text-note font-medium">改动预览</b>
        {showDiffs && (
          <span className="text-cap text-ink-4">{preview?.applied ? '已写入文件' : `共 ${count} 处`}</span>
        )}
      </div>

      {/* diff 区（可滚动，免得字段多时把按钮挤出去） */}
      <div className="min-h-0 flex-1 overflow-auto px-4 py-3">
        {showDiffs ? (
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
            {!dirty
              ? '改点东西，这里会逐行告诉你将写入什么。'
              : preview !== null
                ? '字段又改了，之前的预览已过期 —— 重新点一次「预览改动」。'
                : '点下面的「预览改动」，先看清会写成什么。'}
          </p>
        )}
      </div>

      {/* 反馈 + 动作 */}
      <div className="shrink-0 border-t border-line-weak px-4 py-3">
        {failure && (
          <p className="mb-2.5 rounded-md bg-accent-soft px-3 py-2 text-cap leading-4 text-accent">{failure}</p>
        )}
        {notice && (
          <p className="mb-2.5 rounded-md bg-black/25 px-3 py-2 text-cap leading-4 text-ink-2">{notice}</p>
        )}

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
    </div>
  );
}
