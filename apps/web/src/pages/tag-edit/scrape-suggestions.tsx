/**
 * 刮削建议 —— 编辑区上方的一张**卡片**，不是色带、不是浮层。
 *
 * ## 为什么做成卡片、并跟编辑列同宽
 *
 * 第一版是通栏的 `bg-black/20` 色带。实测它在 1505px 视口里占 0→1505，
 * 而内容从 138px 开始、下面的卡片从 113px 开始 —— **内容跟谁都不对齐**，
 * 看着就是「贴上去的一条」。现在它和 `基本信息` 那些卡片一样是
 * `rounded-xl bg-surface`，宽度由外层编辑列**继承**（见 `tag-edit.tsx` 的注释），
 * 左右边界与卡片完全一致。
 *
 * ## 为什么每行要塞「变化摘要」
 *
 * 候选行只有「标题」和「分数 + 按钮」时，900px 宽的中间是空的，看着很散。
 * 把**变化在哪**放进中间：`歌手 毛不易 · 专辑 平凡的一天 等 3 处` ——
 * 这样不用展开就能判断「这条值不值得用」，行也不空了。
 *
 * ## 为什么按插件分组
 *
 * 多插件时用户真正要判断的是**信哪个源**：同一个歌名，A 插件给出的是原版专辑、
 * B 插件给的是合辑 —— 这是「源之间的差异」。混成一个扁平列表就看不出来了。
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

interface FieldRow {
  label: string;
  before: string;
  after: string;
  same: boolean;
}

/** 逐字段比出差异（收起与展开都要用，算一次）。 */
function diffRows(
  p: ScrapeProposal,
  currentOf: (formKey: (typeof SCRAPE_FIELDS)[number]['formKey']) => string,
): FieldRow[] {
  return SCRAPE_FIELDS.filter(({ key }) => key in p.tags).map(({ key, formKey, label }) => {
    const before = currentOf(formKey).trim();
    const after = propText(p.tags[key as keyof typeof p.tags]).trim();
    return { label, before, after, same: before === after };
  });
}

/**
 * 收起时行中间那一句话：变了哪几个字段、变成什么。
 *
 * 最多列两个，再多了折叠成「等 N 处」—— 行里是给人扫一眼的，不是给读全文的。
 */
function changeSummary(rows: FieldRow[]): string {
  const changed = rows.filter((r) => !r.same);
  if (changed.length === 0) return '与当前值相同';
  const head = changed
    .slice(0, 2)
    .map((r) => `${r.label} ${r.after || '（清空）'}`)
    .join(' · ');
  return changed.length > 2 ? `${head} 等 ${changed.length} 处` : head;
}

function CandidateRow({
  proposal,
  plugin,
  rows,
  expanded,
  onToggle,
  onApply,
  onUseCover,
}: {
  proposal: ScrapeProposal;
  plugin: string;
  rows: FieldRow[];
  expanded: boolean;
  onToggle: () => void;
  onApply: () => void;
  onUseCover: (dataUrl: string) => void;
}) {
  const pct = Math.round(proposal.confidence * 100);
  const changed = rows.filter((r) => !r.same).length;

  return (
    <li className={expanded ? 'bg-black/25' : 'transition-colors hover:bg-black/20'}>
      {/* 收起状态就是一行：标题 · 变化摘要 · 分数 · 用这条 */}
      <div className="flex items-center gap-2.5 px-2.5 py-1.5">
        <button
          type="button"
          onClick={onToggle}
          title={expanded ? '收起' : '展开看逐字段对比'}
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
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
          <span className="w-[180px] shrink-0 truncate text-note text-ink">{headline(proposal, plugin)}</span>
          <span
            className={[
              'min-w-0 flex-1 truncate text-cap',
              changed === 0 ? 'text-ink-4' : 'text-ink-3',
            ].join(' ')}
          >
            {changeSummary(rows)}
          </span>
        </button>
        <span
          className={[
            'w-[38px] shrink-0 text-right text-cap tabular-nums',
            proposal.meets_threshold ? 'text-ink-3' : 'text-ink-4',
          ].join(' ')}
          title={proposal.meets_threshold ? '达到自动采用阈值' : '低于自动采用阈值，仅供参考'}
        >
          {pct}%
        </span>
        <button
          type="button"
          onClick={onApply}
          className="h-6.5 shrink-0 rounded-full bg-black/25 px-2.5 text-cap text-ink-2 transition-colors hover:bg-accent hover:text-white"
        >
          用这条
        </button>
      </div>

      {/* 展开：逐字段对比 + 附带物 */}
      {expanded && (
        <div className="px-2.5 pb-2 pl-8">
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
    </li>
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

  // 逐字段对比只算一次，收起/展开共用。
  const rowsOf = useMemo(() => {
    const map = new Map<string, FieldRow[]>();
    for (const a of attempts) {
      a.candidates.forEach((c, i) => map.set(`${a.plugin}#${i}`, diffRows(c, currentOf)));
    }
    return map;
  }, [attempts, currentOf]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return attempts
      .filter((a) => !pluginFilter || a.plugin === pluginFilter)
      .map((a) => ({
        ...a,
        candidates: a.candidates
          .map((c, i) => ({ c, key: `${a.plugin}#${i}` }))
          .filter(({ c }) => !q || haystack(c, a.plugin, headline(c, a.plugin)).includes(q)),
      }));
  }, [attempts, query, pluginFilter]);

  const shown = filtered.reduce((n, a) => n + a.candidates.length, 0);
  const firstKey = (() => {
    for (const a of filtered) {
      const first = a.candidates[0];
      if (first) return first.key;
    }
    return null;
  })();
  const effectiveOpen = open ?? firstKey;

  return (
    /* 卡片：与「基本信息」那些 Section 同一套观感，宽度由外层编辑列继承。 */
    <section className="rounded-xl bg-surface p-3">
      {/* 工具行 */}
      <div className="mb-2 flex flex-wrap items-center gap-x-3 gap-y-2 px-0.5">
        <b className="shrink-0 text-note font-medium">刮削建议</b>
        <span className="shrink-0 text-cap text-ink-4">
          {query || pluginFilter ? `筛出 ${shown} 条` : `共 ${total} 条候选`}
        </span>

        <span className="flex-1" />

        {total > 0 && (
          <div className="relative w-[150px] shrink-0">
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
              className="h-7 w-full rounded-full bg-black/25 pr-2.5 pl-7 text-cap text-ink outline-none transition-colors placeholder:text-ink-4 focus:bg-black/40"
            />
          </div>
        )}

        {attempts.length > 1 && (
          <select
            value={pluginFilter}
            onChange={(e) => setPluginFilter(e.target.value)}
            className="h-7 shrink-0 rounded-full bg-black/25 px-2.5 text-cap text-ink outline-none transition-colors focus:bg-black/40"
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

      {/* 候选列表：自己有 max-h，超出内滚 —— 不把下面的编辑区挤没。 */}
      <div className="max-h-[34vh] min-h-0 overflow-auto">
        {error && <p className="mb-1.5 rounded-md bg-accent-soft px-2.5 py-2 text-cap text-accent">{error}</p>}

        {busy && !data && <p className="px-1 py-3 text-cap text-ink-4">正在问插件…</p>}

        {data && shown === 0 && (
          <div className="rounded-lg bg-black/20 px-3 py-2.5">
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
            <div key={a.plugin} className="mb-2 last:mb-0">
              {/* 组头：插件名 + 该插件自己的结论 */}
              <div className="flex flex-wrap items-baseline gap-x-2 px-1 pb-1">
                <span className="rounded bg-black/30 px-1.5 py-0.5 text-micro font-medium text-ink-3">
                  {a.plugin}
                </span>
                <span className="text-micro text-ink-4">{a.note}</span>
              </div>
              <ul className="divide-y divide-line-weak overflow-hidden rounded-lg bg-black/15">
                {a.candidates.map(({ c, key }) => (
                  <CandidateRow
                    key={key}
                    proposal={c}
                    plugin={a.plugin}
                    rows={rowsOf.get(key) ?? []}
                    expanded={effectiveOpen === key}
                    onToggle={() => setOpen(effectiveOpen === key ? '' : key)}
                    onApply={() => onApply(c)}
                    onUseCover={onUseCover}
                  />
                ))}
              </ul>
            </div>
          ),
        )}
      </div>
    </section>
  );
}
