import type { ReactNode } from 'react';

/**
 * 设置类页面的两个积木：`Panel`（分块）+ `PanelRow`（块里的一行）。
 *
 * 从 `pages/settings.tsx` 里提出来共用 —— 用户 2026-10-02 要求「点歌请求 / 用户管理
 * 的布局大体和设置页一样」，而「一样」的具体载体就是这两个东西：**一块块面板，
 * 面板里一行行「左边标题 + 说明，右边控件」**。抄三份迟早有一处间距 / 字号漂移，
 * 那正是「看起来不一致」的来源。
 *
 * `PanelRow` 的右控件**没有宽度约束** —— 按钮、开关、输入框都往这儿放，
 * 由调用处给宽度（设置页里那些是行内小控件，表单输入框自己带 `w-[…]`）。
 */
/**
 * 面板标题行右侧的控件（筛选、主操作按钮）。
 *
 * 有它才好在**不改面板外观**的前提下把「这一块的主操作」放在该在的地方 ——
 * `actions` 不传时标题行的布局与宽度和以前逐像素相同。
 */
export function Panel({
  title,
  actions,
  children,
}: {
  title: string;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="mb-4 rounded-lg bg-surface p-5">
      <div className="mb-3 flex items-center gap-3">
        {/* 宽字距全大写小标签（`docs/design.md` §7.1）—— 分组标题专用。
            用 `items-center` 而不是 `items-start`：有右侧按钮时标题要在整行里垂直居中，
            否则 11px 的小字会贴着 34px 按钮的顶边。 */}
        <div className="min-w-0 flex-1 text-micro tracking-[.24em] text-ink-4 uppercase">
          {title}
        </div>
        {actions && <div className="flex shrink-0 flex-wrap items-center justify-end gap-2">{actions}</div>}
      </div>
      {children}
    </div>
  );
}

export function PanelRow({
  label,
  hint,
  children,
}: {
  label: ReactNode;
  /** 标题下面那行小字。**说明比标题重要**：写清这一项的后果，别只重复标题。 */
  hint?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="flex items-center gap-3.5 border-b border-line-weak py-3 last:border-b-0">
      <div className="min-w-0 flex-1">
        <b className="block text-nav font-normal">{label}</b>
        {hint && <span className="text-xs text-ink-3">{hint}</span>}
      </div>
      {children}
    </div>
  );
}
