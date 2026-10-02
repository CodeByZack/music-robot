import { useCallback, useEffect, useMemo, useState } from 'react';
import { useParams } from 'react-router';
import type { LyricsSource, SongTagValues, TagDiff, TagFieldPatch, TagPatchResult } from '@music-robot/core';
import Cover from '@/components/cover.tsx';
import { api } from '@/lib/client.ts';
import { useOverlayClose } from '@/lib/use-overlay-close.ts';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

/**
 * 标签编辑页（S26）—— **全屏浮层**，与设置页 / 播放页同一种形态。
 *
 * ## 为什么是「预览 → 写入」两步
 *
 * 写标签会**直接覆盖原文件**，而 `atomic_replace` 是 copy → tmp → verify → rename，
 * **没有备份、不可撤销**。所以这个页面的主按钮是「预览改动」，真正写盘的按钮
 * 在预览确认之后才可用 —— 用户先看见「到底会改哪些字段、从什么改成什么」。
 *
 * 后端也按这个口径设计：`PATCH` 的 `dry_run` **默认为 true**，不传就只算差异。
 * 这个页面把「预览」和「写入」分成两个明确的动作，不依赖那个默认值。
 *
 * ## 只提交改过的字段
 *
 * 表单里的字段与「打开时读到的值」逐项比较，**只把变化了的发给后端**。
 * 全量提交虽然语义上也对（同值 = 不产生差异），但会把用户没打算碰的字段
 * 也交给标签引擎走一遍——没必要，也容易出差错。
 */

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
  cover: '封面',
};

const LYRICS_SOURCE_HINT: Record<LyricsSource, string> = {
  db: '这份歌词只在数据库里（刮削/入库时读到），文件里没有。修改它会把歌词写进文件。',
  file: '来自文件内嵌歌词。',
  none: '这个文件里没有歌词。',
};

/** 左栏「文件信息」里的短标签。 */
const LYRICS_SOURCE_LABEL: Record<LyricsSource, string> = {
  db: '库内',
  file: '文件内嵌',
  none: '无',
};

/** 表单形态：全部用字符串表示，提交时再转成后端的类型。 */
interface Form {
  title: string;
  artists: string;
  album: string;
  album_artist: string;
  year: string;
  track: string;
  track_total: string;
  disc: string;
  disc_total: string;
  genres: string;
  composers: string;
  comment: string;
  lyrics: string;
  lyrics_timed: string;
}

/** 多值字段在界面上用逗号分隔（中英文逗号都收）。 */
function splitList(raw: string): string[] {
  return raw
    .split(/[,，]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function joinList(items: string[]): string {
  return items.join(', ');
}

/** 空串 ↔ null：界面里「清空输入框」就是「清掉这个字段」。 */
function textOrNull(raw: string): string | null {
  const trimmed = raw.trim();
  return trimmed === '' ? null : trimmed;
}

/** 数字输入：空 = 清空，非数字 = 报错（不静默当成 0）。 */
function numOrNull(raw: string): { ok: true; value: number | null } | { ok: false } {
  const trimmed = raw.trim();
  if (trimmed === '') return { ok: true, value: null };
  const n = Number(trimmed);
  if (!Number.isInteger(n) || n < 0) return { ok: false };
  return { ok: true, value: n };
}

function toForm(values: SongTagValues): Form {
  return {
    title: values.title ?? '',
    artists: joinList(values.artists),
    album: values.album ?? '',
    album_artist: values.album_artist ?? '',
    year: values.year ?? '',
    track: values.track == null ? '' : String(values.track),
    track_total: values.track_total == null ? '' : String(values.track_total),
    disc: values.disc == null ? '' : String(values.disc),
    disc_total: values.disc_total == null ? '' : String(values.disc_total),
    genres: joinList(values.genres),
    composers: joinList(values.composers),
    comment: values.comment ?? '',
    lyrics: values.lyrics ?? '',
    lyrics_timed: values.lyrics_timed ?? '',
  };
}

/**
 * 表单 → PATCH 字段。**只输出与初值不同的项**：
 * 没变的省略（后端保持原样）、清空的传 `null`、改过的传新值。
 */
function buildPatch(form: Form, initial: Form): { fields: TagFieldPatch; invalid: string[] } {
  const fields: TagFieldPatch = {};
  const invalid: string[] = [];

  const text = (key: keyof Form & keyof TagFieldPatch, label: string) => {
    if (form[key] === initial[key]) return;
    (fields as Record<string, unknown>)[key] = textOrNull(form[key]);
    void label;
  };
  const list = (key: 'artists' | 'genres' | 'composers') => {
    if (form[key] === initial[key]) return;
    const items = splitList(form[key]);
    fields[key] = items.length === 0 ? null : items;
  };
  const number = (key: 'track' | 'track_total' | 'disc' | 'disc_total', label: string) => {
    if (form[key] === initial[key]) return;
    const parsed = numOrNull(form[key]);
    if (!parsed.ok) {
      invalid.push(label);
      return;
    }
    fields[key] = parsed.value;
  };

  text('title', '标题');
  text('album', '专辑');
  text('album_artist', '专辑艺术家');
  text('year', '年份');
  text('comment', '注释');
  text('lyrics', '歌词');
  text('lyrics_timed', '逐字歌词');
  list('artists');
  list('genres');
  list('composers');
  number('track', '音轨');
  number('track_total', '音轨总数');
  number('disc', '碟号');
  number('disc_total', '碟号总数');

  return { fields, invalid };
}

/**
 * 一行字段：**标签在左、输入框在右、提示在下**。
 *
 * 提示（“多个用逗号分隔”这种）放在**输入框下面**而不是右侧一个独立列 ——
 * 右侧列在窄一点的时候会被压成竖排（“总/数”），很难看。
 */
function Row({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[84px_minmax(0,1fr)] items-center gap-x-4">
      <span className="text-nav text-ink-3">{label}</span>
      <div className="min-w-0 py-1">{children}</div>
      {hint ? (
        <>
          <span />
          <span className="pb-1 text-micro text-ink-4">{hint}</span>
        </>
      ) : null}
    </div>
  );
}

/** 「当前 / 总数」一对。总数用 **placeholder** 而不是右侧文字标签（那个会被挤成竖排）。 */
function NumberPair({
  value,
  total,
  onValue,
  onTotal,
}: {
  value: string;
  total: string;
  onValue: (v: string) => void;
  onTotal: (v: string) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <input
        className={`${INPUT} w-[78px] text-center`}
        inputMode="numeric"
        value={value}
        onChange={(e) => onValue(e.target.value)}
      />
      <span className="shrink-0 text-ink-4">/</span>
      <input
        className={`${INPUT} w-[78px] text-center`}
        inputMode="numeric"
        placeholder="总数"
        value={total}
        onChange={(e) => onTotal(e.target.value)}
      />    </div>
  );
}

/** 左栏的一行文件信息。 */
function Meta({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-3">
      <dt className="shrink-0 text-ink-4">{label}</dt>
      <dd className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-ink-3">{value}</dd>
    </div>
  );
}

/**
 * 多值字段用的输入框（歌手 / 流派 / 作曲）。
 *
 * 用**自适应高度的 textarea** 而不是单行 input：真实曲库里这两个字段经常被塞进
 * 脏数据 —— 实测有一首的「作曲」就是整段带时间轴的 LRC（一千多字）。
 * 单行 input 里那串东西只能横向卷，根本读不出来；textarea 会自动长高，
 * 一眼就能看出“这个标签是脏的”，也好整段重填。
 *
 * `field-sizing-content` 是 Tailwind v4 / Chromium 的自适应内容高度；
 * 老浏览器不支持时会退化成 `rows={1}` 的固定一行高 —— 依然可用。
 */
function ListInput({
  value,
  onChange,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <textarea
      rows={1}
      className={`${INPUT} field-sizing-content max-h-[132px] w-full resize-none leading-5`}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      placeholder={placeholder}
    />
  );
}

/**
 * 输入框底色用 `black/25` 而**不是** `surface`：
 * 面板本身就是 8% 白，输入框再用 8% 白的话两者边界完全看不出来（第一版就是这样，整片糊在一起）。
 *
 * ⚠️ 这里**不含宽度**：宽度由调用处给（`w-full` 或固定宽）。
 * 放进来的话会和局部的 `w-[78px]` 撞车 —— 谁生效取决于 Tailwind 产物里的先后顺序，
 * 实测是 `w-full` 赢，于是「音轨 / 总数」两个框被拉成了满宽（已踩）。
 */
const INPUT =
  'rounded-md bg-black/25 px-3 py-[7px] text-nav text-ink outline-none transition-colors placeholder:text-ink-4 focus:bg-black/40';

export default function TagEditPage() {
  const { id: rawId } = useParams<{ id: string }>();
  const songId = Number(rawId);
  const { closing, close } = useOverlayClose('/');

  const load = useCallback(() => api.tags.get(songId), [songId]);
  const { data, error, loading } = useAsync(load, [songId]);

  const [form, setForm] = useState<Form | null>(null);
  const [initial, setInitial] = useState<Form | null>(null);
  const [preview, setPreview] = useState<TagPatchResult | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // 备份默认**打开**：写标签不可撤销，多一个 .bak 是这里最划算的保险。
  const [backup, setBackup] = useState(true);

  // 载入完成后初始化表单。只在首次填充，免得用户编辑到一半被覆盖。
  useEffect(() => {
    if (!data || initial !== null) return;
    const next = toForm(data.tags);
    setForm(next);
    setInitial(next);
  }, [data, initial]);

  const patch = useMemo(
    () => (form && initial ? buildPatch(form, initial) : { fields: {}, invalid: [] }),
    [form, initial],
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
      if (result.applied) {
        // 写成功后：把「初值」挪到新状态，这样再次编辑只比新值。
        // 但**不重读文件** —— 后端的 diff 已经是权威的「改了什么」。
        setNotice(
          `已写入文件：${result.file_name}。改动 ${result.diffs.length} 处${
            backup ? '，原文件已备份为同名 .bak' : ''
          }。`,
        );
        setInitial(form);
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
  const format = data?.file.format ?? '';
  const sizes = data?.file.size ? `${(data.file.size / 1024 / 1024).toFixed(1)} MB` : '';

  return (
    <div
      className={[
        'fixed inset-0 z-50 flex flex-col overflow-hidden bg-[#14121b]',
        closing ? 'anim-overlay-out' : 'anim-overlay-in',
      ].join(' ')}
    >
      <div className="flex h-[58px] shrink-0 items-center gap-2 border-b border-line-weak px-5">
        <h2 className="text-lead font-medium">编辑标签</h2>
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
          className="flex size-8 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
        >
          <svg className="ico-sm" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round">
            <path d="M4 4l8 8M12 4l-8 8" />
          </svg>
        </button>
      </div>

      {loading ? (
        <div className="p-6">
          <LoadingNote />
        </div>
      ) : error ? (
        <div className="p-6">
          <ErrorNote message={error} />
        </div>
      ) : !form || !data ? null : (
        <>
          <div className="min-h-0 flex-1 overflow-auto px-5 py-5">
            <div className="mx-auto max-w-[1180px]">
              {/* 危险提示：写在最上面，别让人滑到底才看见 */}
              <div className="mb-5 rounded-lg border border-accent/40 bg-accent-soft px-4 py-3 text-note leading-5 text-accent">
                <b className="font-medium">写入会直接覆盖原文件，没有备份、不可撤销。</b>
                <span className="text-accent/80">
                  {' '}
                  建议先点「预览改动」看清变化；写入前可勾选备份，原文件会存成同名 .bak。
                </span>
              </div>

              {failure && (
                <p className="mb-4 rounded-md bg-accent-soft px-4 py-3 text-note text-accent">{failure}</p>
              )}
              {notice && (
                <p className="mb-4 rounded-md bg-surface px-4 py-3 text-note text-ink-2">{notice}</p>
              )}

              <div className="flex flex-col gap-6 min-[901px]:flex-row min-[901px]:items-start">
                {/* 左栏：封面 + 文件信息。窄屏落到最上面（封面本来就该先看见）。 */}
                <aside className="w-full shrink-0 min-[901px]:w-[236px]">
                  <Cover
                    id={songId}
                    className="aspect-square w-full max-w-[236px]"
                    rounded="rounded-xl"
                    glyphClass="text-glyph"
                  />
                  <dl className="mt-3.5 space-y-1.5 text-cap">
                    <Meta label="格式" value={format || '—'} />
                    <Meta label="大小" value={sizes || '—'} />
                    <Meta label="内嵌封面" value={data.tags.has_cover ? '有' : '无'} />
                    <Meta label="歌词" value={LYRICS_SOURCE_LABEL[data.tags.lyrics_source]} />
                  </dl>
                  <p className="mt-3 text-micro leading-4 text-ink-4">
                    封面取自文件（没有内嵌封面时回退到专辑封面）。本页暂不支持更换封面。
                  </p>
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
                    <div className="mb-2.5 text-cap leading-4 text-ink-4">
                      {LYRICS_SOURCE_HINT[data.tags.lyrics_source]}
                    </div>
                    <textarea
                      className={`${INPUT} h-[200px] w-full resize-y py-2.5 font-mono text-note leading-5`}
                      value={form.lyrics}
                      onChange={(e) => set('lyrics', e.target.value)}
                      placeholder="（没有歌词）"
                    />
                  </Section>

                  {/* 改动预览：只有真的点过「预览」才显示，且改动一变就作废 */}
                  {preview && preview.diffs.length > 0 && (
                    <div className="rounded-xl border border-line bg-surface p-4">
                      <div className="mb-2.5 flex items-center gap-2">
                        <b className="text-note font-medium">改动预览</b>
                        <span className="text-cap text-ink-4">
                          {preview.applied ? '已写入文件' : `共 ${preview.diffs.length} 处`}
                        </span>
                      </div>
                      {preview.diffs.map((d: TagDiff) => (
                        <div key={d.key} className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 py-1 text-note">
                          <span className="w-[86px] shrink-0 text-ink-3">{FIELD_LABEL[d.key] ?? d.key}</span>
                          <span className="text-ink-4 line-through">{d.before}</span>
                          <span className="text-ink-4">→</span>
                          <span className="text-accent">{d.after}</span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            </div>
          </div>

          {/* 底部操作条：常驻，不用滑到底 */}
          <div className="flex shrink-0 flex-wrap items-center gap-3 border-t border-line-weak px-5 py-3">
            <label className="flex cursor-pointer items-center gap-2 text-note text-ink-2">
              <input
                type="checkbox"
                checked={backup}
                onChange={(e) => setBackup(e.target.checked)}
                className="accent-accent"
              />
              写入前备份 .bak
            </label>
            <span className="flex-1" />
            {patch.invalid.length > 0 && (
              <span className="text-cap text-accent">{patch.invalid.join('、')} 需要非负整数</span>
            )}
            <button
              type="button"
              disabled={busy || !dirty}
              onClick={() => void run(false)}
              className="h-9 rounded-full bg-surface px-4 text-note transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-40"
            >
              {busy ? '处理中…' : '预览改动'}
            </button>
            <button
              type="button"
              // 必须先预览过、且预览结果对应当前改动，才能写 —— 这是这个页面最重要的一条约束
              disabled={busy || !previewFresh || !preview?.changed}
              onClick={() => void run(true)}
              className="h-9 rounded-full bg-accent px-4 text-note font-medium text-white transition-colors hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
            >
              写入文件
            </button>
          </div>
        </>
      )}
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="rounded-xl bg-surface p-4">
      <h3 className="mb-3 text-micro tracking-[.24em] text-ink-4 uppercase">{title}</h3>
      {children}
    </section>
  );
}
