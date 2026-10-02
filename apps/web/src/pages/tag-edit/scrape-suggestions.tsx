/**
 * 刮削建议 —— 右栏的第一段（**窄栏形态**，330px）。
 *
 * ## 为什么在右栏
 *
 * 右栏现在是「**检查与落盘**」的一条流程：
 *
 *     刮削建议（找值） → 改动预览（看差异） → 备份 + 写入
 *
 * 三件事本来就是同一串动作，放一栏里顺理成章。放在编辑区上方时它得跟表单争地方，
 * 而且离「用这条填进去、然后预览写入」的下游太远。
 *
 * ## 窄栏里的排版
 *
 * 330px 放不下「标题 | 变化摘要 | 分数 | 按钮」一行，所以候选卡改成两行：
 *
 *     ▾ 盛夏 · 平凡的一天              100%  [用这条]
 *       歌手 毛不易 · 专辑 平凡的一天 等 3 处
 *
 * 第二行是**变化摘要** —— 不展开也能判断「这条值不值得用」。展开才逐字段对比。
 *
 * ## 为什么按插件分组
 *
 * 多插件时用户要判断的是**信哪个源**：同一个歌名，A 插件给出的是原版专辑、
 * B 插件给的是合辑。混成一个扁平列表就看不出来了。
 */
import { useMemo, useState } from 'react';
import type { ScrapeProposal, ScrapeQueryResult } from '@music-robot/core';
import { SCRAPE_FIELDS, propText } from './parts.tsx';

interface FieldRow {
  label: string;
  before: string;
  after: string;
  same: boolean;
}

/** 一条候选的标题：优先「标题 · 专辑」，都没有就退回插件名。 */
function headline(p: ScrapeProposal, plugin: string): string {
  const title = propText(p.tags.title).trim();
  const album = propText(p.tags.album).trim();
  if (title && album) return `${title} · ${album}`;
  return title || album || `${plugin} 的候选`;
}

/** 逐字段比出差异。只算一次，收起与展开共用。 */
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
 * 收起时那行摘要：变了哪几个字段、变成什么。
 *
 * 最多列两个，再多折叠成「等 N 处」—— 这行是给人扫一眼的，不是给读全文的。
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

function CandidateCard({
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
    <li className={['rounded-lg', expanded ? 'bg-black/25' : 'bg-black/15'].join(' ')}>
      {/* 第一行：标题 + 分数 + 动作。330px 里放不下别的了。 */}
      <div className="flex items-center gap-1.5 px-2 pt-1.5">
        <button
          type="button"
          onClick={onToggle}
          title={expanded ? '收起' : '展开看逐字段对比'}
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
          <span className="min-w-0 truncate text-note text-ink">{headline(proposal, plugin)}</span>
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
      </div>

      {/* 第二行：变化摘要 + 「用这条」 */}
      <div className="flex items-center gap-1.5 px-2 pb-1.5 pl-[26px]">
        <span
          className={['min-w-0 flex-1 truncate text-cap', changed === 0 ? 'text-ink-4' : 'text-ink-3'].join(' ')}
          title={changeSummary(rows)}
        >
          {changeSummary(rows)}
        </span>
        <button
          type="button"
          onClick={onApply}
          className="h-6 shrink-0 rounded-full bg-surface px-2.5 text-micro text-ink-2 transition-colors hover:bg-accent hover:text-white"
        >
          用这条
        </button>
      </div>

      {/* 展开：逐字段对比 + 附带物 */}
      {expanded && (
        <div className="px-2 pb-2 pl-[26px]">
          {rows.length === 0 ? (
            <p className="text-cap text-ink-4">这条候选没给任何字段。</p>
          ) : (
            <div className="space-y-0.5">
              {rows.map((r) => (
                <div key={r.label} className="flex flex-wrap items-baseline gap-x-1.5 text-cap leading-5">
                  <span className="w-[42px] shrink-0 text-ink-4">{r.label}</span>
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
            <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
              {proposal.cover ? (
                <>
                  <img src={proposal.cover.data} alt="刮削到的封面" className="size-7 rounded object-cover" />
                  <button
                    type="button"
                    onClick={() => onUseCover(proposal.cover!.data)}
                    className="h-6 rounded-full bg-black/25 px-2 text-micro text-ink-2 transition-colors hover:bg-surface-hover"
                  >
                    用这张封面
                  </button>
                </>
              ) : (
                <span className="text-cap leading-4 text-ink-4">插件给了封面但太大，没有内联</span>
              )}
            </div>
          )}
        </div>
      )}
    </li>
  );
}

export function ScrapeSuggestions({
  open,
  data,
  busy,
  error,
  currentOf,
  onApply,
  onUseCover,
  onResearch,
  onToggleOpen,
}: {
  /** 列表是否展开。收起时只占一行（顶栏那行）。 */
  open: boolean;
  data: ScrapeQueryResult | null;
  busy: boolean;
  error: string | null;
  currentOf: (formKey: (typeof SCRAPE_FIELDS)[number]['formKey']) => string;
  onApply: (p: ScrapeProposal) => void;
  onUseCover: (dataUrl: string) => void;
  onResearch: () => void;
  onToggleOpen: () => void;
}) {
  // 展开的是哪一条（`插件#序号`）。默认展开第一条 —— 它是最可信的。
  const [detail, setDetail] = useState<string | null>(null);

  const attempts = data?.plugins ?? [];
  const total = attempts.reduce((n, a) => n + a.candidates.length, 0);
  const hasResult = data !== null || busy || error !== null;
  const rowsOf = useMemo(() => {
    const map = new Map<string, FieldRow[]>();
    for (const a of attempts) {
      a.candidates.forEach((c, i) => map.set(`${a.plugin}#${i}`, diffRows(c, currentOf)));
    }
    return map;
  }, [attempts, currentOf]);

  const firstKey = (() => {
    for (const a of attempts) {
      const first = a.candidates[0];
      if (first) return `${a.plugin}#0`;
    }
    return null;
  })();
  const effectiveDetail = detail ?? firstKey;

  return (
    <>
      {/* 顶栏那行：标题 + 动作。收起时整段就只有这一行。 */}
      <div className="flex shrink-0 items-center gap-2 px-3 py-2">
        <b className="shrink-0 text-note font-medium">刮削建议</b>

        {hasResult && (
          <span className="shrink-0 text-cap text-ink-4">{total} 条</span>
        )}

        <span className="flex-1" />

        {hasResult ? (
          <>
            <button
              type="button"
              onClick={onResearch}
              disabled={busy}
              title="重新问一次插件（改了表单里的标题/歌手之后常用）"
              className="h-6 shrink-0 rounded-full px-2 text-cap text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink disabled:cursor-not-allowed disabled:opacity-40"
            >
              {busy ? '查询中…' : '重新刮削'}
            </button>
            <button
              type="button"
              onClick={onToggleOpen}
              // tooltip 要带上「刮削建议」：侧边栏那个折叠按钮也写「收起 / 展开」，
              // 光看 tooltip 分不出是哪一个（写测试时被这个坑过一次）。
              title={open ? '收起刮削建议' : '展开刮削建议'}
              aria-label={open ? '收起刮削建议' : '展开刮削建议'}
              aria-expanded={open}
              className="flex size-6 shrink-0 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
            >
              <svg
                className={`ico-xs transition-transform ${open ? 'rotate-180' : ''}`}
                viewBox="0 0 16 16"
                fill="none"
                stroke="currentColor"
                strokeWidth={1.8}
                strokeLinecap="round"
              >
                <path d="M3.5 6 8 10.5 12.5 6" />
              </svg>
            </button>
          </>
        ) : (
          <button
            type="button"
            onClick={onResearch}
            disabled={busy}
            className="h-7 shrink-0 rounded-full bg-surface px-3 text-cap text-ink-2 transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-40"
          >
            {busy ? '查询中…' : '让插件查一下'}
          </button>
        )}
      </div>

      {/* 还没查过：一句说明，别让那个按钮来得没头没尾 */}
      {!hasResult && (
        <p className="shrink-0 px-3 pb-2.5 text-cap leading-4 text-ink-4">
          让插件查一下这首歌该是什么标签。<b className="font-medium text-ink-3">只填进表单，不写盘。</b>
        </p>
      )}

      {/* 列表：自己有滚动，不把下面的改动预览挤没。 */}
      {open && hasResult && (
        <div className="min-h-0 flex-1 overflow-auto px-3 pb-2.5">
          {error && <p className="mb-1.5 rounded-md bg-accent-soft px-2.5 py-2 text-cap text-accent">{error}</p>}

          {busy && !data && <p className="py-3 text-cap text-ink-4">正在问插件…</p>}

          {data && total === 0 && (
            <div className="rounded-lg bg-black/20 px-2.5 py-2">
              <p className="mb-1 text-cap text-ink-2">插件没有给出可用的结果。</p>
              {/* 插件给的原因照原样展示 —— 它常直接指出该先改哪个字段。 */}
              <ul className="space-y-0.5 text-cap leading-4 text-ink-3">
                {attempts.map((a) => (
                  <li key={a.plugin}>
                    <b className="font-medium text-ink-2">{a.plugin}</b>：{a.note}
                  </li>
                ))}
              </ul>
              <p className="mt-1.5 text-cap leading-4 text-ink-4">
                插件要靠歌手和时长认歌。先把「标题 / 歌手」改对再刮。
              </p>
            </div>
          )}

          {attempts.map((a) =>
            a.candidates.length === 0 ? null : (
              <div key={a.plugin} className="mb-2 last:mb-0">
                {/* 组头：插件名 + 它自己的结论（多插件时这才是重点） */}
                <div className="flex flex-wrap items-baseline gap-x-1.5 px-0.5 pb-1">
                  <span className="rounded bg-black/30 px-1.5 py-0.5 text-micro font-medium text-ink-3">
                    {a.plugin}
                  </span>
                  <span className="min-w-0 flex-1 text-micro leading-4 text-ink-4">{a.note}</span>
                </div>
                <ul className="space-y-1">
                  {a.candidates.map((c, i) => {
                    const key = `${a.plugin}#${i}`;
                    return (
                      <CandidateCard
                        key={key}
                        proposal={c}
                        plugin={a.plugin}
                        rows={rowsOf.get(key) ?? []}
                        expanded={effectiveDetail === key}
                        onToggle={() => setDetail(effectiveDetail === key ? '' : key)}
                        onApply={() => onApply(c)}
                        onUseCover={onUseCover}
                      />
                    );
                  })}
                </ul>
              </div>
            ),
          )}
        </div>
      )}
    </>
  );
}
