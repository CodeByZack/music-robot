/**
 * 标签编辑页的**表单原子**与字段转换。
 *
 * 为什么单独一个文件：主页面要同时管「编辑字段」「刮削」「预览 / 写入」三件事，
 * 全塞一处已经过千行。这里的东西与业务无关（谁都能用），搬出来两边都清爽。
 */
import type { ReactNode } from 'react';
import type { SongTagValues, TagFieldPatch } from '@music-robot/core';

/** 表单形态：全部用字符串表示，提交时再转成后端的类型。 */
export interface Form {
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

/**
 * 刮削提议的字段 ↔ 表单字段。
 *
 * 为什么要一张表：插件协议用**单数**（`artist` / `genre`），而表单与 PATCH 用
 * **复数**（`artists` / `genres`）。一一对应写在一处，免得散在渲染与填充两个地方。
 */
export const SCRAPE_FIELDS: { key: string; formKey: keyof Form; label: string }[] = [
  { key: 'title', formKey: 'title', label: '标题' },
  { key: 'artist', formKey: 'artists', label: '歌手' },
  { key: 'album', formKey: 'album', label: '专辑' },
  { key: 'year', formKey: 'year', label: '年份' },
  { key: 'genre', formKey: 'genres', label: '流派' },
  { key: 'track', formKey: 'track', label: '音轨' },
];

/** 多值字段在界面上用逗号分隔（中英文逗号都收）。 */
export function splitList(raw: string): string[] {
  return raw
    .split(/[,，]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function joinList(items: string[]): string {
  return items.join(', ');
}

/** 空串 ↔ null：界面里「清空输入框」就是「清掉这个字段」。 */
export function textOrNull(raw: string): string | null {
  const trimmed = raw.trim();
  return trimmed === '' ? null : trimmed;
}

/** 数字输入：空 = 清空，非数字 = 报错（不静默当成 0）。 */
export function numOrNull(raw: string): { ok: true; value: number | null } | { ok: false } {
  const trimmed = raw.trim();
  if (trimmed === '') return { ok: true, value: null };
  const n = Number(trimmed);
  if (!Number.isInteger(n) || n < 0) return { ok: false };
  return { ok: true, value: n };
}

/** 提议值 → 表单字符串。`null`（插件要求清空）落到空串，提交时会被翻成 `null`。 */
export function propText(value: string | number | null | undefined): string {
  return value === null || value === undefined ? '' : String(value);
}

export function toForm(values: SongTagValues): Form {
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
 *
 * `cover` 单独传（它在左栏，不在表单里）：`{ data }` = 换封面，`null` = 删封面。
 */
export function buildPatch(
  form: Form,
  initial: Form,
  cover?: { data: string } | null,
): { fields: TagFieldPatch; invalid: string[] } {
  const fields: TagFieldPatch = {};
  const invalid: string[] = [];
  if (cover !== undefined) fields.cover = cover;

  const text = (key: keyof Form & keyof TagFieldPatch) => {
    if (form[key] === initial[key]) return;
    (fields as Record<string, unknown>)[key] = textOrNull(form[key]);
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

  text('title');
  text('album');
  text('album_artist');
  text('year');
  text('comment');
  text('lyrics');
  text('lyrics_timed');
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
 * 输入框底色用 `black/25` 而**不是** `surface`：
 * 面板本身就是 8% 白，输入框再用 8% 白的话两者边界完全看不出来（第一版就是这样，整片糊在一起）。
 *
 * ⚠️ 这里**不含宽度**：宽度由调用处给（`w-full` 或固定宽）。
 * 放进来的话会和局部的 `w-[78px]` 撞车 —— 谁生效取决于 Tailwind 产物里的先后顺序，
 * 实测是 `w-full` 赢，于是「音轨 / 总数」两个框被拉成了满宽（已踩）。
 */
export const INPUT =
  'rounded-md bg-black/25 px-3 py-[7px] text-nav text-ink outline-none transition-colors placeholder:text-ink-4 focus:bg-black/40';

/**
 * 一行字段：**标签在左、输入框在右、提示在下**。
 *
 * 提示（“多个用逗号分隔”这种）放在**输入框下面**而不是右侧一个独立列 ——
 * 右侧列在窄一点的时候会被压成竖排（“总/数”），很难看。
 */
export function Row({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
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
export function NumberPair({
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
      />
    </div>
  );
}

/**
 * 多值字段用的输入框（歌手 / 流派 / 作曲）。
 *
 * 用**自适应高度的 textarea** 而不是单行 input：真实曲库里这两个字段经常被塞进
 * 脏数据 —— 实测有一首的「作曲」就是整段带时间轴的 LRC（一千多字）。
 * 单行 input 里那串东西只能横向卷，根本读不出来；textarea 会自动长高，
 * 一眼就能看出「这个标签是脏的」，也好整段重填。
 *
 * `field-sizing-content` 是 Tailwind v4 / Chromium 的自适应内容高度；
 * 老浏览器不支持时会退化成 `rows={1}` 的固定一行高 —— 依然可用。
 */
export function ListInput({
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

/** 分组卡片。 */
export function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="rounded-xl bg-surface p-4">
      <h3 className="mb-3 text-micro tracking-[.24em] text-ink-4 uppercase">{title}</h3>
      {children}
    </section>
  );
}

/** 一行「标签 : 值」，用在文件信息那种只读列表里。 */
export function Meta({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-3">
      <dt className="shrink-0 text-ink-4">{label}</dt>
      <dd className="min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-ink-3">{value}</dd>
    </div>
  );
}
