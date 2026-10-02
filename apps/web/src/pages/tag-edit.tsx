import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useParams } from 'react-router';
import type { ScrapeProposal, ScrapeQueryResult, SongTagValues, TagDiff, TagFieldPatch, TagPatchResult } from '@music-robot/core';
import { api } from '@/lib/client.ts';
import { useSession } from '@/lib/session.tsx';
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
  lyricsTimed: '同步歌词',
  cover: '封面',
};

/** 封面大小上限，与后端 `tags::MAX_COVER_BYTES` 对齐（前端先拦一下，报错更快）。 */
const MAX_COVER_BYTES = 8 * 1024 * 1024;

/**
 * 刮削提议的字段 ↔ 表单字段。
 *
 * 为什么要一张表：插件协议用**单数**（`artist` / `genre`），而表单与 PATCH 用
 * **复数**（`artists` / `genres`）。一一对应写在一处，免得散在渲染与填充两个地方。
 */
const SCRAPE_FIELDS: { key: string; formKey: keyof Form; label: string }[] = [
  { key: 'title', formKey: 'title', label: '标题' },
  { key: 'artist', formKey: 'artists', label: '歌手' },
  { key: 'album', formKey: 'album', label: '专辑' },
  { key: 'year', formKey: 'year', label: '年份' },
  { key: 'genre', formKey: 'genres', label: '流派' },
  { key: 'track', formKey: 'track', label: '音轨' },
];

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
 *
 * `cover` 单独传（它在左栏，不在表单里）：`{ data }` = 换封面，`null` = 删封面。
 */
function buildPatch(
  form: Form,
  initial: Form,
  cover?: { data: string } | null,
): { fields: TagFieldPatch; invalid: string[] } {
  const fields: TagFieldPatch = {};
  const invalid: string[] = [];
  if (cover !== undefined) fields.cover = cover;

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
  text('lyrics_timed', '同步歌词');
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

/**
 * 左栏的封面：预览 + 选择 / 移除。
 *
 * ⚠️ 显示的必须是**文件内嵌封面**，所以走 `/api/songs/{id}/tags/cover`，
 * 而不是通用的 `/api/songs/{id}/cover` —— 后者**优先返回专辑表里那张**
 * （见 `routes::cover` 的数据来源表）。用错端点的话，用户以为在看文件的封面、
 * 实际看的是专辑的，换掉之后界面还不变（这个坑实测踩过）。
 */
function CoverEditor({
  songId,
  hasCover,
  nonce,
  picked,
  onPick,
  onRemove,
  onRefresh,
}: {
  songId: number;
  hasCover: boolean;
  /** 变化就重新取图（写盘成功后父组件 +1）。 */
  nonce: number;
  /** 本地选中的新封面（data URL）。 */
  picked: string | null;
  onPick: (dataUrl: string | null) => void;
  onRemove: () => void;
  onRefresh: () => void;
}) {
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [err, setErr] = useState<string | null>(null);
  // 文件里那张加载失败（没有内嵌封面时端点给 404）→ 显示占位
  const [fileCoverOk, setFileCoverOk] = useState(true);

  useEffect(() => {
    setFileCoverOk(true);
  }, [songId, nonce]);

  async function choose(file: File | null) {
    setErr(null);
    if (!file) return;
    if (file.size > MAX_COVER_BYTES) {
      setErr(`图片太大（${(file.size / 1048576).toFixed(1)} MB），上限 ${MAX_COVER_BYTES / 1048576} MB`);
      return;
    }
    // readAsDataURL 给的就是 `data:image/jpeg;base64,...`，后端直接收这个形状。
    const url = await new Promise<string>((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(String(r.result));
      r.onerror = () => reject(new Error('读取图片失败'));
      r.readAsDataURL(file);
    }).catch((e: Error) => {
      setErr(e.message);
      return '';
    });
    if (url) onPick(url);
  }

  const showFileCover = hasCover && fileCoverOk;

  return (
    <div>
      <div className="aspect-square w-full max-w-[236px] overflow-hidden rounded-xl bg-surface">
        {picked ? (
          // 本地选中的图直接预览（不必等后端），改成什么一目了然
          <img src={picked} alt="新封面预览" className="size-full object-cover" />
        ) : showFileCover ? (
          <img
            src={`/api/songs/${songId}/tags/cover?v=${nonce}`}
            alt="文件内嵌封面"
            className="size-full object-cover"
            onError={() => setFileCoverOk(false)}
          />
        ) : (
          <div className="grid size-full place-items-center px-4 text-center text-cap leading-5 text-ink-4">
            文件里没有内嵌封面
          </div>
        )}
      </div>

      <input
        ref={fileRef}
        type="file"
        accept="image/jpeg,image/png,image/gif"
        className="hidden"
        onChange={(e) => {
          void choose(e.target.files?.[0] ?? null);
          // 清空 value：不然选同一张图不会再触发 change
          e.target.value = '';
        }}
      />

      <div className="mt-2.5 flex flex-wrap gap-2">
        <button
          type="button"
          onClick={() => fileRef.current?.click()}
          className="h-8 rounded-full bg-surface px-3 text-note text-ink-2 transition-colors hover:bg-surface-hover"
        >
          {picked || hasCover ? '更换封面' : '选择封面'}
        </button>
        {(picked || hasCover) && (
          <button
            type="button"
            onClick={() => {
              setErr(null);
              onPick(null);
              onRemove();
            }}
            className="h-8 rounded-full px-3 text-note text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
          >
            移除封面
          </button>
        )}
      </div>

      {err && <p className="mt-1.5 text-cap text-accent">{err}</p>}
      {picked && <p className="mt-1.5 text-cap text-accent">预览的是新封面，写入后才生效。</p>}
      <p className="mt-2 text-micro leading-4 text-ink-4">支持 JPEG / PNG / GIF，上限 8 MB。</p>
      {/* 这里显示的是**文件**封面；列表 / 播放页看到的可能是专辑那张，说一句免得对不上 */}
      <p className="mt-1.5 text-micro leading-4 text-ink-4">
        这里显示的是文件内嵌的封面，列表和播放页可能显示专辑那张。
      </p>
      <button
        type="button"
        onClick={onRefresh}
        className="mt-1.5 text-micro text-ink-4 underline transition-colors hover:text-ink-3"
      >
        刷新预览
      </button>
    </div>
  );
}

/** 左栏的一行文件信息。 */

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
  // 换封面用的缓存失效序号。
  // 为什么必要：写盘后同一个 URL 在浏览器缓存里还是旧图（已踩）。现在端点已经改成
  // `no-store`，这里是第二道保险 —— 万一将来有人把缓存策略改回长缓存，界面依然会刷。
  const [coverNonce, setCoverNonce] = useState(0);

  // ── 刮削（只查不写）────────────────────────────────────────────────────
  //
  // 这里**不写盘**是这个功能的前提：刮削会发起外部请求、结果未必准，
  // 直接覆盖原文件（且不可撤销）风险太大。所以流程是
  // 「刮削 → 提议填进表单 → 用户看一眼、改不改随他 → 预览 → 写入」，
  // 真正落盘仍然走既有的那两道闸。
  const [scrape, setScrape] = useState<ScrapeQueryResult | null>(null);
  const [scrapeBusy, setScrapeBusy] = useState(false);
  const [scrapeError, setScrapeError] = useState<string | null>(null);
  // 刮削会发起网络请求，后端仅限管理员（与 /api/scrape 同档）—— 非管理员不显示入口。
  const { user } = useSession();
  const canScrape = user?.role === 'admin';

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
        //   ③ 重取标签（“封面 image/jpeg”那一行、歌词来源都会变）。
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
  const format = data?.file.format ?? '';
  const sizes = data?.file.size ? `${(data.file.size / 1024 / 1024).toFixed(1)} MB` : '';
  // Vorbis（FLAC）没有同步歌词的键 —— 那个框直接禁用，免得用户白填一遍再被打回。
  const noTimedLyrics = format === 'flac';
  // 帧名按容器说：MP3 里是 ID3 的 USLT，FLAC 里是 Vorbis 的 LYRICS 键。
  // 在 FLAC 上写「USLT」是不准确的 —— 那里根本没有 ID3 帧。
  const lyricsFrame = noTimedLyrics ? 'LYRICS' : 'USLT';

  /** 只查不写地问插件。结果填进 `scrape`，由界面展示成一份待确认的提议。 */
  async function runScrape() {
    setScrapeBusy(true);
    setScrapeError(null);
    setScrape(null);
    try {
      setScrape(await api.tags.queryScrape(songId));
    } catch (e) {
      setScrapeError(e instanceof Error ? e.message : String(e));
    } finally {
      setScrapeBusy(false);
    }
  }

  /** 表单里对应字段的当前值（用于对比展示）。 */
  function currentOf(formKey: keyof Form): string {
    return form ? form[formKey] : '';
  }

  /** 提议值 → 表单字符串。`null`（插件要求清空）落到空串，提交时会被翻成 `null`。 */
  function propText(value: string | number | null | undefined): string {
    return value === null || value === undefined ? '' : String(value);
  }

  /**
   * 把提议填进表单（**一个字节都不写**）。
   *
   * 只填插件**确实给出**的字段：`key in tags` 而不是看值真假 —— 缺失与空串的语义都是
   * 「不修改」，如果按真假判断就会把「没提这个字段」误当成「要清空」。
   * 插件要求清空的（值恰好是 `null`）会落成空串 → 提交时又变回 `null`，语义正好对上。
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
    setScrape(null);
    setNotice('已把刮削结果填进表单 —— 还没写盘。看清楚再点「预览改动」。');
  }

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

              {/* 刮削入口。刻意放在这里（而不是底部操作条）：它的产物只是「草稿」，
                  和那两个真按钮不是一档的事。文案也要说清楚它不写盘。 */}
              {canScrape && (
                <div className="mb-5 flex flex-wrap items-center gap-3">
                  <button
                    type="button"
                    onClick={() => void runScrape()}
                    disabled={scrapeBusy}
                    className="h-8 shrink-0 rounded-full bg-surface px-3.5 text-note text-ink-2 transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-40"
                  >
                    {scrapeBusy ? '查询中…' : '刮削'}
                  </button>
                  <span className="text-cap leading-4 text-ink-4">
                    让插件查一下这首歌的标签。<b className="font-medium text-ink-3">只填进表单，不会直接写盘</b>
                    —— 看完、改完，再走下面的预览与写入。
                  </span>
                </div>
              )}

              {scrapeError && (
                <p className="mb-4 rounded-md bg-accent-soft px-4 py-3 text-note text-accent">{scrapeError}</p>
              )}

              {/* 刮削提议：只列插件确实给出的字段，并把「当前值 → 提议值」摆在一起。 */}
              {scrape && (
                <div className="mb-5 rounded-xl border border-line bg-surface p-4">
                  {scrape.proposal ? (
                    <>
                      <div className="mb-3 flex flex-wrap items-center gap-x-2.5 gap-y-1">
                        <b className="text-note font-medium">刮削提议</b>
                        <span className="text-cap text-ink-4">
                          {scrape.proposal.plugin}
                          {' · '}
                          {(scrape.proposal.confidence * 100).toFixed(0)}% 可信
                        </span>
                        {!scrape.proposal.meets_threshold && (
                          <span className="rounded-full bg-black/25 px-2 py-0.5 text-micro text-ink-3">
                            低于自动采用阈值，仅供参考
                          </span>
                        )}
                      </div>

                      <div className="space-y-0.5">
                        {SCRAPE_FIELDS.filter(({ key }) => key in scrape.proposal!.tags).map(
                          ({ key, formKey, label }) => {
                            const before = currentOf(formKey);
                            const after = propText(scrape.proposal!.tags[key as keyof typeof scrape.proposal.tags]);
                            const same = before.trim() === after.trim();
                            return (
                              <div key={key} className="flex flex-wrap items-baseline gap-x-2 py-0.5 text-note">
                                <span className="w-[52px] shrink-0 text-ink-3">{label}</span>
                                <span className={same ? 'text-ink-4' : 'text-ink-4 line-through'}>
                                  {before.trim() || '(无)'}
                                </span>
                                {!same && (
                                  <>
                                    <span className="text-ink-4">→</span>
                                    <span className="min-w-0 break-all text-accent">{after.trim() || '(清空)'}</span>
                                  </>
                                )}
                                {same && <span className="text-micro text-ink-4">未变化</span>}
                              </div>
                            );
                          })}
                        {/* 封面单独一行：它不在 tags 里，也要单独决定要不要用。 */}
                        {(scrape.proposal.cover || scrape.proposal.cover_skipped) && (
                          <div className="flex flex-wrap items-center gap-x-2 py-0.5 text-note">
                            <span className="w-[52px] shrink-0 text-ink-3">封面</span>
                            {scrape.proposal.cover ? (
                              <>
                                <img
                                  src={scrape.proposal.cover.data}
                                  alt="刮削到的封面"
                                  className="size-9 rounded object-cover"
                                />
                                <span className="text-ink-4">
                                  {(scrape.proposal.cover.size / 1024).toFixed(0)} KB
                                </span>
                                <button
                                  type="button"
                                  onClick={() => {
                                    setNewCover(scrape.proposal!.cover!.data);
                                    setNotice('已把刮削到的封面放进封面栏 —— 还没写盘。');
                                  }}
                                  className="h-7 shrink-0 rounded-full bg-black/25 px-2.5 text-cap text-ink-2 transition-colors hover:bg-surface-hover"
                                >
                                  用这张
                                </button>
                              </>
                            ) : (
                              <span className="text-ink-4">插件给了封面但太大，没有内联（回传也会被拒）</span>
                            )}
                          </div>
                        )}
                        {scrape.proposal.lyrics && (
                          <div className="flex flex-wrap items-baseline gap-x-2 py-0.5 text-note">
                            <span className="w-[52px] shrink-0 text-ink-3">歌词</span>
                            <span className="text-ink-4">{scrape.proposal.lyrics.length} 字</span>
                          </div>
                        )}
                      </div>

                      <div className="mt-3 flex flex-wrap items-center gap-2">
                        <button
                          type="button"
                          onClick={() => applyProposal(scrape.proposal!)}
                          className="h-8 rounded-full bg-accent px-3.5 text-note text-white transition-opacity hover:opacity-90"
                        >
                          填进表单
                        </button>
                        <button
                          type="button"
                          onClick={() => setScrape(null)}
                          className="h-8 rounded-full px-3 text-note text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
                        >
                          关掉
                        </button>
                      </div>
                    </>
                  ) : (
                    <>
                      <div className="mb-2 flex items-center justify-between gap-3">
                        <b className="text-note font-medium">刮削没有结果</b>
                        <button
                          type="button"
                          onClick={() => setScrape(null)}
                          className="h-7 rounded-full px-2.5 text-cap text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
                        >
                          关掉
                        </button>
                      </div>
                      {/* 插件给的原因照原样展示 —— 它常直接指出该先改哪个字段。 */}
                      <ul className="space-y-1 text-cap leading-4 text-ink-3">
                        {scrape.notes.map((note) => (
                          <li key={note}>· {note}</li>
                        ))}
                      </ul>
                      <p className="mt-2 text-cap leading-4 text-ink-4">
                        提示：插件要靠歌手和时长认歌。先把「标题 / 歌手」改对再刮，命中率会高很多。
                      </p>
                    </>
                  )}
                </div>
              )}

              {failure && (
                <p className="mb-4 rounded-md bg-accent-soft px-4 py-3 text-note text-accent">{failure}</p>
              )}
              {notice && (
                <p className="mb-4 rounded-md bg-surface px-4 py-3 text-note text-ink-2">{notice}</p>
              )}

              <div className="flex flex-col gap-6 min-[901px]:flex-row min-[901px]:items-start">
                {/* 左栏：封面（可换 / 可删）+ 文件信息。窄屏落到最上面。 */}
                <aside className="w-full shrink-0 min-[901px]:w-[236px]">
                  <CoverEditor
                    songId={songId}
                    hasCover={data.tags.has_cover && !dropCover}
                    nonce={coverNonce}
                    picked={newCover}
                    onPick={setNewCover}
                    onRemove={() => setDropCover(true)}
                    onRefresh={() => setCoverNonce((n) => n + 1)}
                  />
                  <dl className="mt-4 space-y-1.5 text-cap">
                    <Meta label="格式" value={format || '—'} />
                    <Meta label="大小" value={sizes || '—'} />
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
                          <span className="text-cap text-ink-4">
                            SYLT · 每行形如 [00:12.34]歌词
                          </span>
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

                  {/* 改动预览：只有真的点过「预览」才显示，且改动一变就作废 */}
                  {preview && (preview.diffs.length > 0 || preview.cover_op) && (
                    <div className="rounded-xl border border-line bg-surface p-4">
                      <div className="mb-2.5 flex items-center gap-2">
                        <b className="text-note font-medium">改动预览</b>
                        <span className="text-cap text-ink-4">
                          {preview.applied
                            ? '已写入文件'
                            : `共 ${preview.diffs.length + (preview.cover_op ? 1 : 0)} 处`}
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
                      {/*
                        封面单独一行的原因：引擎的 diff 只比**张数**，换一张封面是新旧都是 1 张、
                        diffs 里根本没有这一行。不单独显示的话，只换封面的场景预览会是一片空白，
                        用户看不出会发生什么。
                      */}
                      {preview.cover_op && (
                        <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 py-1 text-note">
                          <span className="w-[86px] shrink-0 text-ink-3">封面</span>
                          <span className="text-ink-4 line-through">
                            {preview.cover_op === 'remove' ? '原有封面' : '原封面'}
                          </span>
                          <span className="text-ink-4">→</span>
                          <span className="text-accent">
                            {preview.cover_op === 'remove' ? '移除' : '换成新选的图'}
                          </span>
                        </div>
                      )}
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
