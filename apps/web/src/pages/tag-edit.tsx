/**
 * 标签编辑页（S26）—— **全屏浮层**，与设置页 / 播放页同一种形态。
 *
 * ## 布局
 *
 * ```
 * ┌─ 顶栏：编辑标签 · 文件 · [刮削] · [✕] ─────────────────────┐
 * ├─ 危险提示（通栏一行）────────────────────────────────────┤
 * │ 编辑区（滚动）                   │ 审查栏（固定）          │
 * │   左：封面 + 文件信息             │   改动预览              │
 * │   右：基本信息 / 编号 / 歌词       │   备份 + 两个按钮       │
 * └──────────────────────────────────┴────────────────────────┘
 * ```
 *
 * **审查栏**（`tag-edit/review-pane.tsx`）承载「操作 + 它的结果」：上一版按钮钉在底部
 * 横条上，而预览结果渲染在滚动内容的最末尾（歌词下面）—— 用户点完得往下滚才看得见，
 * 看完再滚回底部点写入，操作与结果被隔开了。现在两者同栏，结果就在按钮正上方。
 * 窄屏（<1100px）审查栏落到下方，仍是「结果在上、按钮在下」的同一顺序。
 *
 * 其余三块各自独立成文件，理由都是「这里的东西与业务无关，搬出来两边都清爽」：
 * `parts.tsx`（表单原子与字段转换）、`cover-editor.tsx`（封面）、
 * `scrape-panel.tsx`（刮削建议面板）。
 *
 * ## 为什么是「预览 → 写入」两步
 *
 * 写标签会**直接覆盖原文件**，而 `atomic_replace` 是 copy → tmp → verify → rename，
 * **没有备份、不可撤销**。所以主按钮是「预览改动」，真正写盘的按钮在预览确认之后
 * 才可用 —— 用户先看见「到底会改哪些字段、从什么改成什么」。
 *
 * 后端也按这个口径设计：`PATCH` 的 `dry_run` **默认为 true**，不传就只算差异。
 * 这个页面把「预览」和「写入」分成两个明确的动作，不依赖那个默认值。
 *
 * ## 只提交改过的字段
 *
 * 表单里的字段与「打开时读到的值」逐项比较，**只把变化了的发给后端**。
 * 全量提交虽然语义上也对（同值 = 不产生差异），但会把用户没打算碰的字段
 * 也交给标签引擎走一遍 —— 没必要，也容易出差错。
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useParams } from 'react-router';
import type { ScrapeProposal, ScrapeQueryResult, TagPatchResult } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { useSession } from '@/lib/session.tsx';
import { useOverlayClose } from '@/lib/use-overlay-close.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';
import { CoverEditor } from './tag-edit/cover-editor.tsx';
import {
  buildPatch,
  ListInput,
  Meta,
  NumberPair,
  Row,
  SCRAPE_FIELDS,
  Section,
  toForm,
  INPUT,
  propText,
  type Form,
} from './tag-edit/parts.tsx';
import { ReviewDetail, ActionBar, reviewStateOf } from './tag-edit/review-pane.tsx';
import { ScrapeSuggestions } from './tag-edit/scrape-suggestions.tsx';

export default function TagEditPage() {
  const { id: rawId } = useParams<{ id: string }>();
  const songId = Number(rawId);
  const { closing, close } = useOverlayClose('/');

  const load = useCallback(() => api.tags.get(songId), [songId]);
  const { data, error, loading, reload } = useAsync(load, [songId]);

  const [form, setForm] = useState<Form | null>(null);
  const [initial, setInitial] = useState<Form | null>(null);
  const [preview, setPreview] = useState<TagPatchResult | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // 备份默认**打开**：写标签不可撤销，多一个 .bak 是这里最划算的保险。
  const [backup, setBackup] = useState(true);
  // 封面：选中的新图（data URL），或「要删掉」
  const [newCover, setNewCover] = useState<string | null>(null);
  const [dropCover, setDropCover] = useState(false);
  // 换封面用的缓存失效序号。为什么必要：写盘后同一个 URL 在浏览器缓存里还是旧图
  // （已踩）。端点现在给 `no-store`，这里是第二道保险。
  const [coverNonce, setCoverNonce] = useState(0);
  // 窄屏时右栏详情区的展开状态（宽屏忽略它 —— 那边本来就常显）。
  // 默认折叠：窄屏空间金贵，实测「改动预览标题 + 提示 + 备份 + 两个按钮」
  // 加起来接近 190px，把编辑字段挤掉了半屏。
  const [sheetOpen, setSheetOpen] = useState(false);

  // ── 刮削（只查不写）────────────────────────────────────────────────────
  //
  // **不写盘**是这个功能的前提：刮削会发起外部请求、结果未必准，直接覆盖原文件
  // （且不可撤销）风险太大。所以流程是「刮削 → 选一条填进表单 → 改不改随他
  // → 预览 → 写入」，真正落盘仍然走既有的那两道闸。
  const [scrapeOpen, setScrapeOpen] = useState(false);
  const [scrape, setScrape] = useState<ScrapeQueryResult | null>(null);
  const [scrapeBusy, setScrapeBusy] = useState(false);
  const [scrapeError, setScrapeError] = useState<string | null>(null);
  // 刮削会发起网络请求，后端仅限管理员（与 /api/scrape 同档）—— 非管理员不显示入口。
  const { user } = useSession();
  const isAdmin = user?.role === 'admin';

  /** 封面要传给后端的部分。没动封面就是 `undefined`（ = 保持原样）。 */
  const coverPatch = useMemo(() => {
    if (newCover) return { data: newCover };
    if (dropCover) return null;
    return undefined;
  }, [newCover, dropCover]);

  // 载入完成后初始化表单。只在首次填充，免得用户编辑到一半被覆盖。
  useEffect(() => {
    if (!data || initial !== null) return;
    const next = toForm(data.tags);
    setForm(next);
    setInitial(next);
  }, [data, initial]);

  const patch = useMemo(
    () => (form && initial ? buildPatch(form, initial, coverPatch) : { fields: {}, invalid: [] }),
    [form, initial, coverPatch],
  );
  const dirty = Object.keys(patch.fields).length > 0;
  // 表单一旦改动，之前那次预览就过期了 —— 必须让用户重新预览再写。
  const [previewOf, setPreviewOf] = useState<string | null>(null);
  const signature = JSON.stringify(patch.fields);
  const previewFresh = previewOf === signature && preview !== null;

  useEffect(() => {
    if (!dirty) {
      setPreview(null);
      setPreviewOf(null);
    }
  }, [dirty]);

  function set<K extends keyof Form>(key: K, value: string) {
    setForm((prev) => (prev ? { ...prev, [key]: value } : prev));
    setNotice(null);
    setFailure(null);
  }

  async function run(write: boolean) {
    if (!data || !form || !initial) return;
    if (patch.invalid.length > 0) {
      setFailure(`这些字段不是有效的非负整数：${patch.invalid.join('、')}`);
      return;
    }
    if (Object.keys(patch.fields).length === 0) {
      setNotice('没有字段被修改。');
      return;
    }
    setBusy(true);
    setFailure(null);
    setNotice(null);
    try {
      const result = await api.tags.patch(songId, {
        fields: patch.fields,
        dry_run: !write,
        ...(write ? { backup } : {}),
      });
      setPreview(result);
      setPreviewOf(signature);
      // 预览回来了就把详情展开 —— 两步闸门的重点就是「先看清再写」，
      // 折叠着用户会以为没反应。
      if (!write) setSheetOpen(true);
      if (result.applied) {
        // 写成功后：把「初值」挪到新状态，这样再次编辑只比新值。
        // 但**不重读文件** —— 后端的 diff 已经是权威的「改了什么」。
        setNotice(
          `已写入文件：${result.file_name}。改动 ${result.diffs.length + (result.cover_op ? 1 : 0)} 处${
            backup ? '，原文件已备份为同名 .bak' : ''
          }。`,
        );
        setInitial(form);
        // 写成功 = 新封面已经进文件了：本地暂存清掉，回落去读服务端那张。
        // 必须同时干三件事，少一件界面就会说谎：
        //   ① 清掉本地预览图（不然一直显示那张 data URL）；
        //   ② +nonce 换 URL（缓存里那张旧图不能再用）；
        //   ③ 重取标签（“封面 image/jpeg”那一行会变）。
        setNewCover(null);
        setDropCover(false);
        setCoverNonce((n) => n + 1);
        reload();
      } else if (!result.changed) {
        setNotice('没有字段需要改动。');
      }
    } catch (e) {
      setFailure(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const fileName = data?.file.name ?? '';
  // 四态（未改动 / 未预览 / 预览过期 / 预览最新）。算在 review-pane 里，
  // 详情区与动作栏共用同一份判断，免得两处口径漂移。
  const reviewState = reviewStateOf(dirty, previewFresh, preview);
  const format = data?.file.format ?? '';
  const sizes = data?.file.size ? `${(data.file.size / 1024 / 1024).toFixed(1)} MB` : '';
  // Vorbis（FLAC）没有同步歌词的键 —— 那个框直接禁用，免得用户白填一遍再被打回。
  const noTimedLyrics = format === 'flac';
  // 帧名按容器说：MP3 里是 ID3 的 USLT，FLAC 里是 Vorbis 的 LYRICS 键。
  // 在 FLAC 上写「USLT」是不准确的 —— 那里根本没有 ID3 帧。
  const lyricsFrame = noTimedLyrics ? 'LYRICS' : 'USLT';

  /** 只查不写地问插件。结果交给刮削面板展示。 */
  async function runScrape() {
    setScrapeBusy(true);
    setScrapeError(null);
    try {
      setScrape(await api.tags.queryScrape(songId));      // 新内容到了就展开 —— 窄屏折叠着的话用户看不到有结果。
      setSheetOpen(true);    } catch (e) {
      setScrapeError(e instanceof Error ? e.message : String(e));
    } finally {
      setScrapeBusy(false);
    }
  }

  /** 打开面板并立刻查一次。 */
  function openScrape() {
    setScrapeOpen(true);
    setScrape(null);
    void runScrape();
  }

  /** 表单里对应字段的当前值（对比展示用）。 */
  const currentOf = (formKey: (typeof SCRAPE_FIELDS)[number]['formKey']) => (form ? form[formKey] : '');

  /**
   * 把提议填进表单（**一个字节都不写**）。
   *
   * **刻意不收起建议区**：浮层版必须收起（否则挡住编辑区），但内联版没这个必要 ——
   * 连着看几条、先填 A 觉得不对再换 B，是这里的常见用法。
   * 填了什么下面表单里立刻能看到。
   *
   * 只填插件**确实给出**的字段：`key in tags` 而不是看值真假 —— 缺失与空串的语义都是
   * 「不修改」，按真假判断就会把「没提这个字段」误当成「要清空」。
   * 插件要求清空的（值恰好是 `null`）落成空串 → 提交时又变回 `null`，语义正好对上。
   */
  function applyProposal(p: ScrapeProposal) {
    const t = p.tags;
    setForm((prev) => {
      if (!prev) return prev;
      const next = { ...prev };
      for (const { key, formKey } of SCRAPE_FIELDS) {
        if (key in t) next[formKey] = propText(t[key as keyof typeof t]);
      }
      if (p.lyrics) next.lyrics = p.lyrics;
      return next;
    });
    setNotice('已把这条候选填进表单 —— 还没写盘。看清楚再点「预览改动」。');
  }

  /**
   * 非管理员：**整页拦住**，而不是只藏某几个按钮。
   *
   * 为什么必须整页：`PATCH /api/songs/{id}/tags` 挂的是 `AdminUser`，
   * 而「预览改动」是 `dry_run = true` 走的**同一个接口** —— 所以普通用户
   * 改完表单再点预览只会收到 403。
   *
   * 实测过的体验：普通用户能打开这一页、能改字段，「预览改动」按钮也会解禁
   * （它的 disabled 只看「改没改」，不看权限），点下去才弹「需要管理员权限」。
   * 这种「看上去能用、点了才拒绝」比不给入口差得多 —— 用户会以为是自己点错了。
   *
   * 【可选】后端 `GET tags` 对**登录用户**开放（不是 admin），所以理论上可以做一个
   * 只读版（看得到当前标签、不能改）。但那是新功能，不在「不给用不了的入口」范围内。
   */
  if (!isAdmin) {
    return (
      <div
        className={[
          'app-bg fixed inset-0 z-50 flex flex-col items-center justify-center gap-4 px-6',
          closing ? 'anim-overlay-out' : 'anim-overlay-in',
        ].join(' ')}
      >
        <p className="text-lead font-medium">编辑标签仅管理员可用</p>
        <p className="max-w-[380px] text-center text-nav text-ink-3">
          标签编辑会直接覆盖原文件、不可撤销，所以只开放给管理员。
          你可以继续使用曲库、播放、歌单与点歌。
        </p>
        <button
          type="button"
          onClick={close}
          className="h-9 rounded-full bg-surface px-4 text-nav transition-colors hover:bg-surface-hover"
        >
          返回
        </button>
      </div>
    );
  }

  return (
    <div
      className={[
        'app-bg fixed inset-0 z-50 flex flex-col overflow-hidden',
        closing ? 'anim-overlay-out' : 'anim-overlay-in',
      ].join(' ')}
    >
      {/* 顶栏。 */}
      <header className="flex h-[58px] shrink-0 items-center gap-3 border-b border-line-weak px-5">
        <h2 className="shrink-0 text-lead font-medium">编辑标签</h2>
        <span className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-cap text-ink-4">
          {fileName}
          {format ? ` · ${format}` : ''}
          {sizes ? ` · ${sizes}` : ''}
        </span>
        <span className="flex-1" />
        <button
          type="button"
          onClick={close}
          title="关闭"
          aria-label="关闭标签编辑"
          className="flex size-8 shrink-0 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
        >
          <svg className="ico-sm" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round">
            <path d="M4 4l8 8M12 4l-8 8" />
          </svg>
        </button>
      </header>

      {/* 危险提示：通栏一行。写在最上面，别让人滑到底才看见。
          措辞注意：原文是「没有备份、不可撤销 —— 写入前可勾选备份」，两句读起来像
          互相矛盾（到底有没有备份）。现在把关系写清楚：默认没有，勾了才有。 */}
      <div className="shrink-0 border-b border-line-weak bg-accent-soft px-5 py-2 text-cap leading-4 text-accent">
        写入会<b className="font-medium">直接覆盖原文件，不可撤销</b>
        —— 请先看清预览；要留退路就勾选「写入前备份」。
      </div>

      {/* 刮削建议已移到右栏（与改动预览同栏）。 */}

      {loading ? (
        <div className="p-6">
          <LoadingNote />
        </div>
      ) : error ? (
        <div className="p-6">
          <ErrorNote message={error} />
        </div>
      ) : !form || !data ? null : (
        /* 宽屏两栏（编辑列 | 右栏）、窄屏上下。 */
        <div className="flex min-h-0 flex-1 flex-col min-[1100px]:flex-row">
          {/* 编辑区（滚动）。这里只管编辑字段 —— 建议与预览都在右栏。 */}
          <div className="min-h-0 flex-1 overflow-auto px-5 py-5">
            <div className="mx-auto flex max-w-[940px] flex-col gap-5 min-[760px]:flex-row min-[760px]:items-start">
              {/* 左栏：封面 + 文件信息。窄屏（<760px）整块变成「图在左、描述在右」一行，
                  文件信息作为 children 落进图的右侧 —— 见 CoverEditor 的说明。 */}
              <aside className="w-full shrink-0 min-[760px]:w-[210px]">
                <CoverEditor
                  songId={songId}
                  hasCover={data.tags.has_cover && !dropCover}
                  nonce={coverNonce}
                  picked={newCover}
                  onPick={setNewCover}
                  onRemove={() => setDropCover(true)}
                  onRefresh={() => setCoverNonce((n) => n + 1)}
                >
                  <dl className="space-y-1.5 text-cap">
                    <Meta
                      label="封面"
                      value={
                        newCover ? '待替换' : dropCover ? '待移除' : data.tags.has_cover ? data.tags.cover_mime ?? '有' : '无'
                      }
                    />
                    <Meta label="歌词" value={data.tags.lyrics ? `有（${lyricsFrame}）` : '无'} />
                    <Meta
                      label="同步歌词"
                      value={noTimedLyrics ? '不支持' : data.tags.lyrics_timed ? '有（SYLT）' : '无'}
                    />
                  </dl>
                </CoverEditor>
              </aside>

              <div className="min-w-0 flex-1 space-y-4">
                <Section title="基本信息">
                  <Row label="标题">
                    <input className={`${INPUT} w-full`} value={form.title} onChange={(e) => set('title', e.target.value)} />
                  </Row>
                  <Row label="歌手" hint="多个用逗号分隔">
                    <ListInput value={form.artists} onChange={(v) => set('artists', v)} />
                  </Row>
                  <Row label="专辑">
                    <input className={`${INPUT} w-full`} value={form.album} onChange={(e) => set('album', e.target.value)} />
                  </Row>
                  <Row label="专辑艺术家" hint="留空则按第一个歌手算">
                    <input
                      className={`${INPUT} w-full`}
                      value={form.album_artist}
                      onChange={(e) => set('album_artist', e.target.value)}
                    />
                  </Row>
                  <Row label="年份">
                    <input className={`${INPUT} w-full`} value={form.year} onChange={(e) => set('year', e.target.value)} />
                  </Row>
                </Section>

                <Section title="编号与分类">
                  {/* 音轨 / 碟号 两对并排：它们各自很窄，竖着排太浪费 */}
                  <div className="grid gap-x-6 min-[701px]:grid-cols-2">
                    <Row label="音轨">
                      <NumberPair
                        value={form.track}
                        total={form.track_total}
                        onValue={(v) => set('track', v)}
                        onTotal={(v) => set('track_total', v)}
                      />
                    </Row>
                    <Row label="碟号">
                      <NumberPair
                        value={form.disc}
                        total={form.disc_total}
                        onValue={(v) => set('disc', v)}
                        onTotal={(v) => set('disc_total', v)}
                      />
                    </Row>
                  </div>
                  <Row label="流派" hint="多个用逗号分隔">
                    <ListInput value={form.genres} onChange={(v) => set('genres', v)} />
                  </Row>
                  <Row label="作曲" hint="多个用逗号分隔">
                    <ListInput value={form.composers} onChange={(v) => set('composers', v)} />
                  </Row>
                  <Row label="注释">
                    <input className={`${INPUT} w-full`} value={form.comment} onChange={(e) => set('comment', e.target.value)} />
                  </Row>
                </Section>

                <Section title="歌词">
                  {/*
                    两个框，因为文件里就是**两个不同的帧**：
                      USLT = 无时间轴歌词，SYLT = 有时间轴歌词（LRC）。
                    以前页面只给一个框，里面还是从带时间轴那份**削掉时间轴**算出来的
                    副本 —— 看着像「文件里的歌词」，实际不是；一改还会把 SYLT 删了。
                  */}
                  <div className="space-y-4">
                    <div>
                      <div className="mb-1.5 flex flex-wrap items-baseline gap-x-2">
                        <span className="text-nav text-ink-2">歌词</span>
                        <span className="text-cap text-ink-4">
                          {lyricsFrame} · {form.lyrics.length} 字
                        </span>
                      </div>
                      <textarea
                        className={`${INPUT} h-[180px] w-full resize-y py-2.5 font-mono text-note leading-5`}
                        value={form.lyrics}
                        onChange={(e) => set('lyrics', e.target.value)}
                        placeholder="（文件里没有这一帧）"
                      />
                    </div>
                    <div>
                      <div className="mb-1.5 flex flex-wrap items-baseline gap-x-2">
                        <span className="text-nav text-ink-2">同步歌词</span>
                        <span className="text-cap text-ink-4">SYLT · 每行形如 [00:12.34]歌词</span>
                      </div>
                      <textarea
                        className={`${INPUT} h-[180px] w-full resize-y py-2.5 font-mono text-note leading-5 disabled:cursor-not-allowed disabled:opacity-40`}
                        value={form.lyrics_timed}
                        onChange={(e) => set('lyrics_timed', e.target.value)}
                        placeholder={noTimedLyrics ? '（FLAC 没有存放位置）' : '（文件里没有这一帧）'}
                        disabled={noTimedLyrics}
                      />
                      {noTimedLyrics && (
                        <p className="mt-1.5 text-cap leading-4 text-ink-4">
                          Vorbis（FLAC）只有「歌词」一个键，没有同步歌词的存放位置。
                        </p>
                      )}
                    </div>
                  </div>
                </Section>
              </div>
            </div>
          </div>

          {/* 右栏：**检查与落盘**一条流程 —— 刮削建议（找值）→ 改动预览（看差异）
              → 备份 + 两个按钮（写）。
              宽屏：固定 330px 在右，两段都常显。
              窄屏：详情（刮削 + 预览）**默认折叠**、动作栏常显 ——
                    空间金贵，详情常开着要占近 190px（实测）。折叠后约 80px。 */}
          <aside className="flex shrink-0 flex-col border-line-weak bg-black/15 max-[1099px]:border-t min-[1100px]:w-[330px] min-[1100px]:border-l">
            <div
              className={[
                'flex min-h-0 flex-col min-[1100px]:flex-1',
                // 窄屏：详情自己滚，且限高（不把编辑区挤没）
                'max-[1099px]:max-h-[44vh] max-[1099px]:overflow-auto',
                sheetOpen ? '' : 'max-[1099px]:hidden',
              ].join(' ')}
            >
              {isAdmin && (
                <div
                  className={[
                    'flex shrink-0 flex-col border-b border-line-weak',
                    // 展开时给列表一个上限，不把下面的改动预览挤没；收起时只占一行。
                    scrapeOpen ? 'min-h-0 max-[1099px]:max-h-[30vh] min-[1100px]:max-h-[46%]' : '',
                  ].join(' ')}
                >
                  <ScrapeSuggestions
                    open={scrapeOpen}
                    data={scrape}
                    busy={scrapeBusy}
                    error={scrapeError}
                    currentOf={currentOf}
                    onApply={applyProposal}
                    onUseCover={(d) => {
                      setNewCover(d);
                      setDropCover(false);
                      setNotice('已把刮削到的封面放进封面栏 —— 还没写盘。');
                    }}
                    onResearch={() => {
                      openScrape();
                    }}
                    onToggleOpen={() => setScrapeOpen((v) => !v)}
                  />
                </div>
              )}

              <div className="flex min-h-0 flex-1 flex-col">
                <ReviewDetail preview={preview} state={reviewState} />
              </div>
            </div>

            <ActionBar
              state={reviewState}
              preview={preview}
              busy={busy}
              dirty={dirty}
              fresh={previewFresh}
              backup={backup}
              onBackup={setBackup}
              onPreview={() => void run(false)}
              onWrite={() => void run(true)}
              invalid={patch.invalid}
              notice={notice}
              failure={failure}
              expanded={sheetOpen}
              onToggleExpanded={() => setSheetOpen((v) => !v)}
            />
          </aside>
        </div>
      )}
    </div>
  );
}
