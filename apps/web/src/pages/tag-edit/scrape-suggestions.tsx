/**
 * 刮削建议 —— **就地展开的一条区域**，不是浮层。
 *
 * ## 为什么不做成浮层 / 独立页面
 *
 * 第一版是盖满全屏的浮层，看着像换了个页面：编辑到一半点「刮削」，视野被整个接管，
 * 关掉才能接着填。而这个动作的真实用法是**看着候选决定填什么** ——
 * 应该能一边看建议、一边看下面的表单。
 *
 * 所以它现在是编辑区上方**内联展开**的一块：占位、可滚、可收起，编辑区仍在下面。
 * 展开时也不会把字段挤没：列表自己有 `max-h`，超出内滚。
 *
 * ## 为什么按插件分组
 *
 * 多插件时用户真正要判断的是**信哪个源**：同一个歌名，A 插件给出的是原版专辑、
 * B 插件给的是合辑 —— 这是「源之间的差异」。把它们混成一个扁平列表就看不出来了。
 *
 * ## 筛选
 *
 * 候选可能十几条（多插件 × 每个插件多条），给一个文本框按标题/专辑/歌手/插件名
 * 做子串匹配，外加插件下拉（选项标出各组候选数）。**纯前端过滤**，不重新问插件
 * —— 那要改协议，且每次都得再等一轮网络。
 */
import { useMemo, useState } from 'react';
import type { ScrapeProposal, ScrapeQueryResult } from '@music-robot/core';
import { SCRAPE_FIELDS, propText } from './parts.tsx';

/** 一条候选的标题：优先「标题 · 专辑」，都没有就退回插件名。 */
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

/** 一行候选。收起时就是一行（够判断「要不要用这条」），展开才逐字段比。 */
function CandidateRow({
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
  const rows = SCRAPE_FIELDS.filter(({ key }) => key in proposal.tags).map(
    ({ key, formKey, label }) => {
      const before = currentOf(formKey).trim();
      const after = propText(proposal.tags[key as keyof typeof proposal.tags]).trim();
      return { label, before, after, same: before === after };
    },
  );
  const changed = rows.filter((r) => !r.same).length;
  const pct = Math.round(proposal.confidence * 100);

  return (
    <div className="rounded-md transition-colors hover:bg-black/20">
      <div className="flex items-center gap-2 py-1">
        <button
          type="button"
          onClick={onToggle}
          title={expanded ? '收起' : '展开看字段差异'}
          className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
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
          <span className="min-w-0 truncate text-note text-ink-2">{headline(proposal, plugin)}</span>
          {expanded && changed > 0 && <span className="shrink-0 text-micro text-accent">{changed} 处不同</span>}
        </button>
        <span
          className={[
            'w-[42px] shrink-0 text-right text-cap tabular-nums',
            proposal.meets_threshold ? 'text-ink-3' : 'text-ink-4',
          ].join(' ')}
          title={proposal.meets_threshold ? '达到自动采用阈值' : '低于自动采用阈值，仅供参考'}
        >
          {pct}%
        </span>
        <button
          type="button"
          onClick={onApply}
          className="h-7 shrink-0 rounded-full bg-surface px-3 text-cap text-ink-2 transition-colors hover:bg-accent hover:text-white"
        >
          用这条
        </button>
      </div>

      {expanded && (
        <div className="pb-2 pl-6">
          {rows.length === 0 ? (
            <p className="text-cap text-ink-4">这条候选没给任何字段。</p>
          ) : (
            <div className="space-y-0.5">
              {rows.map((r) => (
                <div key={r.label} className="flex flex-wrap items-baseline gap-x-2 text-cap leading-5">
                  <span className="w-[46px] shrink-0 text-ink-4">{r.label}</span>
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
            <div className="mt-1.5 flex flex-wrap items-center gap-2">
              {proposal.cover ? (
                <>
                  <img src={proposal.cover.data} alt="刮削到的封面" className="size-8 rounded object-cover" />
                  <span className="text-cap text-ink-4">{(proposal.cover.size / 1024).toFixed(0)} KB</span>
                  <button
                    type="button"
                    onClick={() => onUseCover(proposal.cover!.data)}
                    className="h-6 rounded-full bg-black/25 px-2 text-micro text-ink-2 transition-colors hover:bg-surface-hover"
                  >
                    用这张封面
                  </button>
                </>
              ) : (
                <span className="text-cap text-ink-4">插件给了封面但太大，没有内联（回传也会被拒）</span>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export function ScrapeSuggestions({
  data,
  busy,
  error,
  currentOf,
  onApply,
  onUseCover,
  onResearch,
  onCollapse,
}: {
  data: ScrapeQueryResult | null;
  busy: boolean;
  error: string | null;
  currentOf: (formKey: (typeof SCRAPE_FIELDS)[number]['formKey']) => string;
  onApply: (p: ScrapeProposal) => void;
  onUseCover: (dataUrl: string) => void;
  onResearch: () => void;
  onCollapse: () => void;
}) {
  const [query, setQuery] = useState('');
  const [pluginFilter, setPluginFilter] = useState('');
  // 展开的是哪一条（`插件#序号`）。默认展开第一条 —— 它是最可信的。
  const [open, setOpen] = useState<string | null>(null);

  const attempts = data?.plugins ?? [];
  const total = attempts.reduce((n, a) => n + a.candidates.length, 0);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return attempts
      .filter((a) => !pluginFilter || a.plugin === pluginFilter)
      .map((a) => ({
        ...a,
        candidates: a.candidates.filter((c) => !q || haystack(c, a.plugin, headline(c, a.plugin)).includes(q)),
      }));
  }, [attempts, query, pluginFilter]);

  const shown = filtered.reduce((n, a) => n + a.candidates.length, 0);
  const firstKey = (() => {
    for (const a of filtered) if (a.candidates.length > 0) return `${a.plugin}#0`;
    return null;
  })();
  const effectiveOpen = open ?? firstKey;

  return (
    /* 宽度跟**编辑列**对齐：宽屏时右侧留出审查栏那 330px（见 tag-edit 里 aside 的宽度），
       内层再限到与编辑区同一个 max-w —— 否则候选行会横拉到 1500px，
       分数和按钮飘到右边跟标题对不上。 */
    <section className="shrink-0 border-b border-line-weak bg-black/20 min-[1100px]:pr-[330px]">
      <div className="mx-auto max-w-[940px] px-5">
        {/* 工具行：标题 + 筛选 + 动作 */}
        <div className="flex flex-wrap items-center gap-2 py-2.5">
        <b className="shrink-0 text-note font-medium">刮削建议</b>

        <div className="relative min-w-[160px] max-w-[280px] flex-1">
          <svg
            className="ico-xs pointer-events-none absolute top-1/2 left-2.5 -translate-y-1/2 text-ink-4"
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
            placeholder="筛选候选"
            className="h-7 w-full rounded-full bg-black/30 pr-2.5 pl-7 text-cap text-ink outline-none transition-colors placeholder:text-ink-4 focus:bg-black/45"
          />
        </div>

        {attempts.length > 1 && (
          <select
            value={pluginFilter}
            onChange={(e) => setPluginFilter(e.target.value)}
            className="h-7 shrink-0 rounded-full bg-black/30 px-2.5 text-cap text-ink outline-none transition-colors focus:bg-black/45"
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
          {query || pluginFilter ? `筛出 ${shown} 条` : `共 ${total} 条候选`}
        </span>

        <span className="flex-1" />

        <button
          type="button"
          onClick={onResearch}
          disabled={busy}
          className="h-7 shrink-0 rounded-full px-2.5 text-cap text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink disabled:cursor-not-allowed disabled:opacity-40"
        >
          {busy ? '查询中…' : '重新刮削'}
        </button>
        <button
          type="button"
          onClick={onCollapse}
          title="收起"
          aria-label="收起刮削建议"
          className="flex size-7 shrink-0 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
        >
          <svg className="ico-xs" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.8} strokeLinecap="round">
            <path d="M3.5 6 8 10.5 12.5 6" />
          </svg>
        </button>
      </div>

      {/* 候选列表：自己有 max-h，超出内滚 —— 展开时不把下面的编辑区挤没。 */}
        <div className="max-h-[36vh] min-h-0 overflow-auto pb-2.5">
        {error && <p className="mb-1.5 rounded-md bg-accent-soft px-3 py-2 text-cap text-accent">{error}</p>}

        {busy && !data && <p className="py-3 text-cap text-ink-4">正在问插件…</p>}

        {data && shown === 0 && (
          <div className="rounded-md bg-black/20 px-3 py-2">
            <p className="mb-1 text-cap text-ink-2">
              {total === 0 ? '插件没有给出可用的结果。' : '没有候选匹配这个筛选。'}
            </p>
            {/* 插件给的原因照原样展示 —— 它常直接指出该先改哪个字段。 */}
            {total === 0 && (
              <ul className="space-y-0.5 text-cap leading-4 text-ink-3">
                {attempts.map((a) => (
                  <li key={a.plugin}>
                    <b className="font-medium text-ink-2">{a.plugin}</b>：{a.note}
                  </li>
                ))}
              </ul>
            )}
            {total === 0 && (
              <p className="mt-1.5 text-cap leading-4 text-ink-4">
                插件要靠歌手和时长认歌。先把「标题 / 歌手」改对再刮，命中率会高很多。
              </p>
            )}
          </div>
        )}

        {filtered.map((a) =>
          a.candidates.length === 0 ? null : (
            <div key={a.plugin} className="mb-1.5 last:mb-0">
              {/* 组头：插件名 + 该插件自己的结论 */}
              <div className="flex flex-wrap items-baseline gap-x-2 pt-1 pb-0.5">
                <span className="rounded bg-black/30 px-1.5 py-0.5 text-micro font-medium text-ink-3">
                  {a.plugin}
                </span>
                <span className="text-micro text-ink-4">{a.note}</span>
              </div>
              <div className="space-y-0.5">
                {a.candidates.map((c, i) => {
                  const key = `${a.plugin}#${i}`;
                  return (
                    <CandidateRow
                      key={key}
                      proposal={c}
                      plugin={a.plugin}
                      currentOf={currentOf}
                      expanded={effectiveOpen === key}
                      onToggle={() => setOpen(effectiveOpen === key ? '' : key)}
                      onApply={() => onApply(c)}
                      onUseCover={onUseCover}
                    />
                  );
                })}
              </div>
            </div>
          ),
        )}        </div>      </div>
    </section>
  );
}
