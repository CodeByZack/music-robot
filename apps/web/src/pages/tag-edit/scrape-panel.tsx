/**
 * 刮削面板：**只查不写**的插件结果，按插件分组展示。
 *
 * ## 为什么是独立浮层，而不是塞进编辑页
 *
 * 这个动作有三个特点：低频（偶尔用一次）、内容多（N 个插件 × M 条候选）、
 * 用完即弃（结果只是草稿）。塞进主滚动流会把「编辑字段」这件事挤走 ——
 * 上一版就是这样，刮削卡片和改动预览各占一块，正文只剩中间一截。
 *
 * ## 为什么按插件分组
 *
 * 多插件时用户真正要判断的是**信哪个源**：同一个歌名，A 插件给出的是原版专辑、
 * B 插件给的是合辑 —— 这是「源之间的差异」。把它们混成一个扁平列表就看不出来了。
 * 分组之后还能按插件筛选、折叠。
 *
 * ## 筛选
 *
 * 候选可能十几条（一个插件的多条 + 多个插件的），所以给一个文本框按
 * 标题 / 专辑 / 歌手 / 插件名做子串匹配，外加一个插件下拉。
 * **纯前端过滤**，不再去问插件（那要改协议，且每次都要等网络）。
 */
import { useMemo, useState } from 'react';
import type { PluginAttempt, ScrapeProposal, ScrapeQueryResult } from '@music-robot/core';
import { SCRAPE_FIELDS, propText } from './parts.tsx';

/** 一条候选在界面上的标题：优先「标题 · 专辑」，都没有就退回插件名。 */
function headline(p: ScrapeProposal, plugin: string): string {
  const title = propText(p.tags.title).trim();
  const album = propText(p.tags.album).trim();
  if (title && album) return `${title} · ${album}`;
  return title || album || `${plugin} 的候选`;
}

/** 一条候选可被搜索的全部文本。 */
function haystack(p: ScrapeProposal, plugin: string, label: string): string {
  return [plugin, label, ...Object.values(p.tags).map((v) => propText(v))].join(' ').toLowerCase();
}

function CandidateCard({
  proposal,
  plugin,
  currentOf,
  expanded,
  onToggle,
  onApply,
  onUseCover,
}: {
  proposal: ScrapeProposal;
  plugin: string;
  currentOf: (formKey: (typeof SCRAPE_FIELDS)[number]['formKey']) => string;
  expanded: boolean;
  onToggle: () => void;
  onApply: () => void;
  onUseCover: (dataUrl: string) => void;
}) {
  const label = headline(proposal, plugin);
  const pct = Math.round(proposal.confidence * 100);
  const fields = SCRAPE_FIELDS.filter(({ key }) => key in proposal.tags);
  // 只在展开时才逐行比 —— 列表里十几条候选时不必都算。
  const rows = fields.map(({ key, formKey, label: fieldLabel }) => {
    const before = currentOf(formKey).trim();
    const after = propText(proposal.tags[key as keyof typeof proposal.tags]).trim();
    return { fieldLabel, before, after, same: before === after };
  });
  const changed = rows.filter((r) => !r.same).length;

  return (
    <div
      className={[
        'rounded-lg border transition-colors',
        expanded ? 'border-accent/40 bg-black/25' : 'border-line-weak bg-black/15 hover:bg-black/25',
      ].join(' ')}
    >
      <div className="flex items-center gap-2 px-3 py-2.5">
        <button
          type="button"
          onClick={onToggle}
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
          title={expanded ? '收起' : '展开看字段差异'}
        >
          <svg
            className={`ico-xs shrink-0 text-ink-4 transition-transform ${expanded ? 'rotate-90' : ''}`}
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth={1.8}
            strokeLinecap="round"
          >
            <path d="M6 3.5 10.5 8 6 12.5" />
          </svg>
          <span className="min-w-0 truncate text-note text-ink">{label}</span>
        </button>
        <span
          className={[
            'shrink-0 text-cap tabular-nums',
            proposal.meets_threshold ? 'text-ink-3' : 'text-ink-4',
          ].join(' ')}
          title={proposal.meets_threshold ? '达到自动采用阈值' : '低于自动采用阈值，仅供参考'}
        >
          {pct}%
        </span>
        {expanded && (
          <button
            type="button"
            onClick={onApply}
            className="h-7 shrink-0 rounded-full bg-accent px-3 text-cap font-medium text-white transition-colors hover:brightness-110"
          >
            填进表单
          </button>
        )}
      </div>

      {expanded && (
        <div className="px-3 pb-3">
          {rows.length === 0 ? (
            <p className="text-cap text-ink-4">这条候选没给任何字段。</p>
          ) : (
            <div className="space-y-0.5">
              {rows.map((r) => (
                <div key={r.fieldLabel} className="flex flex-wrap items-baseline gap-x-2 text-cap leading-5">
                  <span className="w-[46px] shrink-0 text-ink-4">{r.fieldLabel}</span>
                  {r.same ? (
                    <>
                      <span className="min-w-0 break-all text-ink-3">{r.before || '(无)'}</span>
                      <span className="text-micro text-ink-4">未变化</span>
                    </>
                  ) : (
                    <>
                      <span className="min-w-0 break-all text-ink-4 line-through">{r.before || '(无)'}</span>
                      <span className="text-ink-4">→</span>
                      <span className="min-w-0 break-all text-accent">{r.after || '(清空)'}</span>
                    </>
                  )}
                </div>
              ))}
            </div>
          )}

          {proposal.lyrics && (
            <p className="mt-1 text-cap text-ink-4">另带歌词 {proposal.lyrics.length} 字（会写进库）</p>
          )}

          {(proposal.cover || proposal.cover_skipped) && (
            <div className="mt-2 flex flex-wrap items-center gap-2">
              {proposal.cover ? (
                <>
                  <img src={proposal.cover.data} alt="刮削到的封面" className="size-10 rounded object-cover" />
                  <span className="text-cap text-ink-4">{(proposal.cover.size / 1024).toFixed(0)} KB</span>
                  <button
                    type="button"
                    onClick={() => onUseCover(proposal.cover!.data)}
                    className="h-7 rounded-full bg-black/25 px-2.5 text-cap text-ink-2 transition-colors hover:bg-surface-hover"
                  >
                    用这张封面
                  </button>
                </>
              ) : (
                <span className="text-cap text-ink-4">插件给了封面但太大，没有内联（回传也会被拒）</span>
              )}
            </div>
          )}

          {changed === 0 && rows.length > 0 && (
            <p className="mt-1.5 text-micro text-ink-4">这条候选的所有字段都和当前值一样。</p>
          )}
        </div>
      )}
    </div>
  );
}

export function ScrapePanel({
  data,
  busy,
  error,
  songName,
  currentOf,
  onApply,
  onUseCover,
  onResearch,
  onClose,
}: {
  data: ScrapeQueryResult | null;
  busy: boolean;
  error: string | null;
  songName: string;
  currentOf: (formKey: (typeof SCRAPE_FIELDS)[number]['formKey']) => string;
  onApply: (p: ScrapeProposal) => void;
  onUseCover: (dataUrl: string) => void;
  onResearch: () => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState('');
  const [pluginFilter, setPluginFilter] = useState('');
  // 展开的是哪一条（`插件#序号`）。默认展开第一条可信的。
  const [open, setOpen] = useState<string | null>(null);

  const attempts = data?.plugins ?? [];
  const totalCandidates = attempts.reduce((n, a) => n + a.candidates.length, 0);

  /** 过滤后的插件分组。只有插件全空、且没有筛选条件时才为空。 */
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return attempts
      .filter((a) => !pluginFilter || a.plugin === pluginFilter)
      .map((a: PluginAttempt) => ({
        ...a,
        // 第一条命中的候选在过滤后也要能自动展开，所以先算好可见列表
        candidates: a.candidates.filter((c) => !q || haystack(c, a.plugin, headline(c, a.plugin)).includes(q)),
      }))
      .filter((a) => a.candidates.length > 0 || (!q && a.candidates.length === 0));
  }, [attempts, query, pluginFilter]);

  // 默认展开：第一条候选。用户没展开过、或者展开的那条被筛掉了，就回到第一条。
  const firstKey = (() => {
    for (const a of filtered) {
      if (a.candidates.length > 0) return `${a.plugin}#0`;
    }
    return null;
  })();
  const effectiveOpen = open ?? firstKey;
  const anyShown = filtered.some((a) => a.candidates.length > 0);

  return (
    <div className="fixed inset-0 z-[60] flex flex-col bg-[#14121b]">
      {/* 顶栏 */}
      <div className="flex h-[58px] shrink-0 items-center gap-3 border-b border-line-weak px-5">
        <h2 className="text-lead font-medium">刮削建议</h2>
        <span className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-cap text-ink-4">
          {songName}
        </span>
        <span className="flex-1" />
        <button
          type="button"
          onClick={onResearch}
          disabled={busy}
          className="h-8 shrink-0 rounded-full bg-surface px-3.5 text-note text-ink-2 transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-40"
        >
          {busy ? '查询中…' : '重新刮削'}
        </button>
        <button
          type="button"
          onClick={onClose}
          title="关闭"
          aria-label="关闭刮削建议"
          className="flex size-8 shrink-0 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
        >
          <svg className="ico-sm" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round">
            <path d="M4 4l8 8M12 4l-8 8" />
          </svg>
        </button>
      </div>

      {/* 工具栏：筛选。候选多时（多插件 × 多条）就靠它找。 */}
      {(totalCandidates > 0 || attempts.length > 1) && (
        <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-line-weak px-5 py-2.5">
          <div className="relative min-w-[180px] flex-1">
            <svg
              className="ico-xs pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-ink-4"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth={1.6}
              strokeLinecap="round"
            >
              <circle cx="7" cy="7" r="4.2" />
              <path d="M10.2 10.2 14 14" />
            </svg>
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="筛选候选（标题 / 专辑 / 歌手 / 插件）"
              className="h-8 w-full rounded-full bg-black/25 pr-3 pl-8 text-cap text-ink outline-none transition-colors placeholder:text-ink-4 focus:bg-black/40"
            />
          </div>
          {attempts.length > 1 && (
            <select
              value={pluginFilter}
              onChange={(e) => setPluginFilter(e.target.value)}
              className="h-8 shrink-0 rounded-full bg-black/25 px-3 text-cap text-ink outline-none transition-colors focus:bg-black/40"
            >
              <option value="">全部插件（{attempts.length}）</option>
              {attempts.map((a) => (
                <option key={a.plugin} value={a.plugin}>
                  {a.plugin}
                  {a.candidates.length > 0 ? `（${a.candidates.length}）` : '（未命中）'}
                </option>
              ))}
            </select>
          )}
          <span className="shrink-0 text-cap text-ink-4">
            {query || pluginFilter ? '筛出 ' : '共 '}
            {filtered.reduce((n, a) => n + a.candidates.length, 0)} 条候选
          </span>
        </div>
      )}

      {/* 内容 */}
      <div className="min-h-0 flex-1 overflow-auto px-5 py-4">
        <div className="mx-auto max-w-[860px] space-y-4">
          {error && (
            <p className="rounded-lg bg-accent-soft px-4 py-3 text-note text-accent">{error}</p>
          )}

          {busy && !data && <p className="py-6 text-center text-nav text-ink-4">正在问插件…</p>}

          {data && !anyShown && (
            <div className="rounded-xl border border-line-weak bg-surface p-4">
              <p className="mb-2 text-note text-ink-2">
                {totalCandidates === 0 ? '插件没有给出可用的结果。' : '没有候选匹配这个筛选。'}
              </p>
              {/* 插件给的原因照原样展示 —— 它常直接指出该先改哪个字段。 */}
              <ul className="space-y-1 text-cap leading-4 text-ink-3">
                {filtered.map((a) => (
                  <li key={a.plugin}>
                    <b className="font-medium text-ink-2">{a.plugin}</b>：{a.note}
                  </li>
                ))}
              </ul>
              {totalCandidates === 0 && (
                <p className="mt-2.5 text-cap leading-4 text-ink-4">
                  提示：插件要靠歌手和时长认歌。先把「标题 / 歌手」改对再刮，命中率会高很多。
                </p>
              )}
            </div>
          )}

          {filtered.map((attempt) => (
            <section key={attempt.plugin}>
              {/* 分组头：插件名 + 该组的结论 */}
              <div className="mb-2 flex flex-wrap items-baseline gap-x-2.5 gap-y-1">
                <b className="text-note font-medium text-ink-2">{attempt.plugin}</b>
                <span className="text-cap text-ink-4">{attempt.note}</span>
              </div>

              {attempt.candidates.length === 0 ? (
                <p className="rounded-lg border border-line-weak bg-black/15 px-3 py-2.5 text-cap leading-4 text-ink-4">
                  未命中
                </p>
              ) : (
                <div className="space-y-2">
                  {attempt.candidates.map((c, i) => {
                    const key = `${attempt.plugin}#${i}`;
                    return (
                      <CandidateCard
                        key={key}
                        proposal={c}
                        plugin={attempt.plugin}
                        currentOf={currentOf}
                        expanded={effectiveOpen === key}
                        onToggle={() => setOpen(effectiveOpen === key ? '' : key)}
                        onApply={() => onApply(c)}
                        onUseCover={onUseCover}
                      />
                    );
                  })}
                </div>
              )}
            </section>
          ))}
        </div>
      </div>

      {/* 底部说明：把「这一步不写盘」再强调一次 —— 它是整个功能的前提 */}
      <div className="shrink-0 border-t border-line-weak px-5 py-2.5 text-cap leading-4 text-ink-4">
        这里只是建议，<b className="font-medium text-ink-3">不会</b>动你的文件。选一条「填进表单」，
        再回编辑页走「预览 → 写入」。
      </div>
    </div>
  );
}
