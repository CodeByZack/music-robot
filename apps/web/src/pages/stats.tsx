import { useCallback, useState } from 'react';
import type { DailyPlayRow, PlayStats, Song, TopArtistRow, TopSongRow } from '@music-robot/core';
import Cover from '@/components/cover.tsx';
import { Panel } from '@/components/panel.tsx';
import { api } from '@/lib/client.ts';
import { usePlayer } from '@/lib/player.tsx';
import { ErrorNote, LoadingNote, useAsync } from '@/lib/use-async.tsx';

/**
 * 播放统计（`/stats`）。
 *
 * 数据全部来自 `GET /api/history/stats`（**只有自己的播放行**，后端按 AuthUser 隔离）。
 *
 * # 三条不能含糊的口径（都在界面上说出来，别让它变成「猜的数字」）
 *
 * 1. **时间范围影响三个数字**（总量 / 两个榜单 / 柱状图）—— 它们来自同一组筛过的行，
 *    所以「按天之和 = 总播放次数」恒成立。这也是后端有专门断言的不变量。
 * 2. **累计时长会偏小**：`duration_listened_ms` 是 2026-10-02 起前端才上报的，
 *    更早的历史行是 NULL（按 0 累加）。所以这一格挂了说明文字 —— 不写就成了假精确。
 * 3. **总次数 ≥ 榜单之和**：歌手榜跳过了已软删的歌。不解释的话用户会去加。
 */
const RANGES: { days: number; label: string }[] = [
  { days: 7, label: '最近 7 天' },
  { days: 30, label: '最近 30 天' },
  { days: 90, label: '最近 90 天' },
  // 0 = 后端约定的「全部时间」
  { days: 0, label: '全部时间' },
];

export default function StatsPage() {
  const [days, setDays] = useState(30);
  // 时区偏移必须由客户端给：后端按它切「一天」。
  // ⚠️ 符号相反 —— `getTimezoneOffset()` 在东八区返回 -480，而后端要的是 +480。
  const tz = -new Date().getTimezoneOffset();
  const load = useCallback(
    () => api.history.stats({ days, top: 10, tz_offset_minutes: tz }),
    [days, tz],
  );
  const { data, error, loading } = useAsync<PlayStats>(load, [days]);

  const totals = data?.totals;

  return (
    <div className="flex-1 overflow-auto px-5 pt-2 pb-32 max-[1024px]:px-4 max-[640px]:px-3">
      <div className="flex flex-wrap items-end gap-4 pt-2.5 pb-5">
        <div className="min-w-0">
          <h1 className="text-2xl leading-8 font-semibold tracking-[-.2px] max-[640px]:text-xl">
            播放统计
          </h1>
          <div className="mt-1 text-nav text-ink-3">
            {data ? `统计范围：${RANGES.find((r) => r.days === days)?.label}` : '读取中…'}
          </div>
        </div>
        <span className="flex-1" />
        {/* 范围切换放右上角（与「音乐库」那种页面同位置） */}
        <div className="flex shrink-0 flex-wrap gap-1">
          {RANGES.map((r) => (
            <button
              key={r.days}
              type="button"
              onClick={() => setDays(r.days)}
              className={[
                'rounded-full px-3 py-1.5 text-cap transition-colors',
                days === r.days
                  ? 'bg-surface text-ink'
                  : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
              ].join(' ')}
            >
              {r.label}
            </button>
          ))}
        </div>
      </div>

      {error ? (
        <ErrorNote message={error} />
      ) : loading ? (
        <LoadingNote />
      ) : !totals || totals.plays === 0 ? (
        /* 空状态把「为什么是空的」说清：可能是这段范围里没听，也可能是从来没用过 */
        <p className="py-10 text-center text-nav text-ink-3">
          {days === 0 ? '还没有播放记录。' : '这段时间里没有播放记录。'}
          {days !== 0 && '试试切到「全部时间」。'}
        </p>
      ) : (
        <>
          <div className="mb-4 grid grid-cols-2 gap-4 min-[901px]:grid-cols-4">
            <Tile label="播放次数" value={String(totals.plays)} unit="次" />
            <Tile
              label="累计收听"
              value={fmtDuration(totals.listened_ms)}
              hint="只含已上报时长的播放"
            />
            <Tile label="听过的歌" value={String(totals.songs)} unit="首" />
            <Tile
              label="有播放的天数"
              value={String((data?.daily ?? []).length)}
              unit="天"
              hint={days === 0 ? '全部时间' : `最近 ${days} 天内`}
            />
          </div>

          <Panel title={days === 0 ? '每天播放量（全部）' : `每天播放量（最近 ${days} 天）`}>
            <DailyChart daily={data?.daily ?? []} days={days} />
          </Panel>

          <Panel title="最常听的歌">
            <SongRank rows={data?.top_songs ?? []} />
          </Panel>

          <Panel title="最常听的歌手">
            <ArtistRank rows={data?.top_artists ?? []} />
          </Panel>
        </>
      )}
    </div>
  );
}

/**
 * 一个数字格子。
 *
 * 顺带说明为什么不用「渐变卡」那套（首页四张卡）：那四张是**入口**（点了会跳走），
 * 而这里是**读数**（点不动）。长得一样会让人去点。
 */
function Tile({
  label,
  value,
  unit,
  hint,
}: {
  label: string;
  value: string;
  unit?: string;
  hint?: string;
}) {
  return (
    <div className="rounded-xl bg-surface px-4 py-3.5">
      <div className="text-micro tracking-[.24em] text-ink-4 uppercase">{label}</div>
      <div className="mt-1.5 flex items-baseline gap-1">
        <b className="text-2xl leading-8 font-semibold tabular-nums">{value}</b>
        {unit && <span className="text-cap text-ink-3">{unit}</span>}
      </div>
      {hint && <div className="mt-0.5 text-micro text-ink-4">{hint}</div>}
    </div>
  );
}

/**
 * 每天播放量。**纯 SVG 柱状图，没有依赖**。
 *
 * 只画有播放的天（后端返回的就是这些），并按日期补出中间的空档 ——
 * 否则「稀疏的几天」会被并排画得像连续的，看着像每天都在听。
 */
function DailyChart({ daily, days }: { daily: DailyPlayRow[]; days: number }) {
  // 后端给的是「有播放的天」，这里按**范围**铺满日期轴（稀疏数据不能并排画）
  const span = days === 0 ? daily.length : days;
  const cells = buildDailyCells(daily, span);

  if (daily.length === 0) {
    return <p className="py-6 text-center text-nav text-ink-4">这段时间里没有播放记录。</p>;
  }
  const max = Math.max(...cells.map((c) => c.plays), 1);

  return (
    <div>
      {/* 用 div 而不是 SVG：柱高就是百分比，天然自适应宽度、也不用算 viewBox */}
      <div className="flex h-[120px] items-end gap-[2px]">
        {cells.map((c) => (
          <div
            key={c.day}
            // 每根柱子给个 title：鼠标停在上面能看到「哪天、几次」
            title={c.plays > 0 ? `${c.day}：${c.plays} 次` : `${c.day}：没有播放`}
            className="group min-w-0 flex-1"
            style={{ height: '100%' }}
          >
            <div className="flex h-full flex-col justify-end">
              <div
                className={[
                  'w-full rounded-t-[2px] transition-colors',
                  c.plays > 0 ? 'bg-accent/70 group-hover:bg-accent' : 'bg-white/[.06]',
                ].join(' ')}
                style={{ height: `${Math.max((c.plays / max) * 100, c.plays > 0 ? 4 : 1)}%` }}
              />
            </div>
          </div>
        ))}
      </div>
      <div className="mt-2 flex justify-between text-micro text-ink-4">
        <span>{cells[0]?.day}</span>
        <span>峰值 {max} 次/天</span>
        <span>{cells[cells.length - 1]?.day}</span>
      </div>
    </div>
  );
}

/**
 * 把「有播放的天」铺成连续的日期轴。
 *
 * `span` 是范围天数（`days = 0` 的全部时间就按实际天数铺，不再补空档 ——
 * 补出几百根空柱子没有意义，反而把有数据的那几根压成一条线）。
 * 用**本地日期**拼 `YYYY-MM-DD`，与后端按同一时区切天的口径一致。
 */
function buildDailyCells(daily: DailyPlayRow[], span: number): { day: string; plays: number }[] {
  const byDay = new Map(daily.map((d) => [d.day, d.plays]));
  if (span > 120) {
    // 范围太大就不补空档了（补出来每根柱子不到 1px）
    return daily.map((d) => ({ day: d.day, plays: d.plays }));
  }
  const out: { day: string; plays: number }[] = [];
  const today = new Date();
  for (let back = span - 1; back >= 0; back -= 1) {
    const d = new Date(today);
    d.setDate(today.getDate() - back);
    const key = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(
      d.getDate(),
    ).padStart(2, '0')}`;
    out.push({ day: key, plays: byDay.get(key) ?? 0 });
  }
  return out;
}

/** 歌榜。点一行直接播这一榜（与首页那张网格同一行为）。 */
function SongRank({ rows }: { rows: TopSongRow[] }) {
  const player = usePlayer();
  const songs = rows.map((r) => r.song).filter((s): s is Song => s !== null);
  if (rows.length === 0) {
    return <p className="py-6 text-center text-nav text-ink-4">这段时间里没有播放记录。</p>;
  }
  return (
    <div>
      {rows.map((row, i) => {
        const song = row.song;
        const idx = song ? songs.findIndex((s) => s.id === song.id) : -1;
        return (
          <div
            key={song?.id ?? `gone-${i}`}
            className="flex items-center gap-3 border-b border-line-weak py-2.5 last:border-b-0"
          >
            <span className="w-[22px] shrink-0 text-cap text-ink-4 tabular-nums">{i + 1}</span>
            {song ? (
              <button
                type="button"
                onClick={() => idx >= 0 && player.playList(songs, idx)}
                className="flex min-w-0 flex-1 items-center gap-3 text-left"
                title={`播放《${song.title ?? '（无标题）'}》`}
              >
                <Cover id={song.id} className="size-9" rounded="rounded-md" glyphClass="text-note" />
                <span className="min-w-0">
                  <span className="block truncate text-nav text-ink">{song.title ?? '（无标题）'}</span>
                  <span className="block truncate text-xs text-ink-3">
                    {song.artists ?? '（未知歌手）'}
                  </span>
                </span>
              </button>
            ) : (
              /* 聚合与取曲目之间刚被软删 —— 如实说，别让这一行变成空白 */
              <span className="min-w-0 flex-1 truncate text-nav text-ink-4">
                这首歌已不在库里
              </span>
            )}
            <span className="shrink-0 text-right">
              <b className="block text-nav tabular-nums">{row.plays} 次</b>
              <span className="block text-cap text-ink-4">{fmtDuration(row.listened_ms)}</span>
            </span>
          </div>
        );
      })}
    </div>
  );
}

/**
 * 歌手榜。**不做成跳转**：`songs.artists` 是整串（多个歌手是 "A / B" 一整串），
 * 而歌手页要的是精确匹配的名字 —— 「A / B」这一串跳过去是能命中（精确匹配整串），
 * 但用户点「A / B」多半想看的是其中一个。与其跳到一个语义可疑的页面，不如不跳。
 */
function ArtistRank({ rows }: { rows: TopArtistRow[] }) {
  if (rows.length === 0) {
    return (
      <p className="py-6 text-center text-nav text-ink-4">
        这段时间里没有播放记录（歌手榜会跳过已不在库里的歌）。
      </p>
    );
  }
  const max = Math.max(...rows.map((r) => r.plays), 1);
  return (
    <div>
      {rows.map((row, i) => (
        <div
          key={`${row.name}-${i}`}
          className="flex items-center gap-3 border-b border-line-weak py-2.5 last:border-b-0"
        >
          <span className="w-[22px] shrink-0 text-cap text-ink-4 tabular-nums">{i + 1}</span>
          <span className="min-w-0 flex-1">
            <span className="block truncate text-nav text-ink">{row.name}</span>
            {/* 进度条：一眼看出「比第二名多多少」。宽度用百分比，不用绝对像素 */}
            <span className="mt-1 block h-1 overflow-hidden rounded-full bg-white/[.08]">
              <span
                className="block h-full rounded-full bg-accent/60"
                style={{ width: `${(row.plays / max) * 100}%` }}
              />
            </span>
          </span>
          <span className="shrink-0 text-right">
            <b className="block text-nav tabular-nums">{row.plays} 次</b>
            <span className="block text-cap text-ink-4">{fmtDuration(row.listened_ms)}</span>
          </span>
        </div>
      ))}
    </div>
  );
}

/**
 * 毫秒 → 给人看的时长（`3 小时 12 分` / `12 分 30 秒` / `45 秒`）。
 *
 * 刻意**不显示秒以下的精度**，也**不做四舍五入的「约」** —— 数值本身就是
 * 「已知的那部分」，再加修饰词只会更含糊。0 显示成 `0 秒` 而不是 `—`：
 * 「听了 0 秒」是真实情况（那首歌没上报时长），不是缺数据。
 */
export function fmtDuration(ms: number): string {
  const totalSec = Math.floor(ms / 1000);
  if (totalSec < 60) return `${totalSec} 秒`;
  const minutes = Math.floor(totalSec / 60);
  if (minutes < 60) return `${minutes} 分 ${totalSec % 60} 秒`;
  const hours = Math.floor(minutes / 60);
  return `${hours} 小时 ${minutes % 60} 分`;
}
