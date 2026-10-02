import { useCallback, useEffect, useState } from 'react';
import type { Job, LibraryRoot, LibraryStats } from '@music-robot/core';
import { Panel, PanelRow as Row } from '@/components/panel.tsx';
import { api } from '@/lib/client.ts';
import { messageOf } from '@/lib/session.tsx';
import { setWriteFiles as setWriteFilesPref, useWriteFiles } from '@/lib/scrape-prefs.ts';
import { useAsync } from '@/lib/use-async.tsx';

/**
 * 设置 → 音乐库（**是设置页的第一个分节，仅 admin 可见**）。
 *
 * 这一节回答四个问题，顺序就是面板顺序：
 *   1. **文件在哪** —— 库根路径（含「这一根读不到」这类硬问题）
 *   2. **库里有什么** —— 入库 / 已刮削 / 待刮削 / 失败 的数量
 *   3. 怎么把新文件**扫进来**（扫描）
 *   4. 怎么把标签**补全**（刮削）
 *
 * ⚠️ **路径是只读的，这一节不写任何配置文件** —— 理由见 [`RootsPanel`] 的注释。
 * 用户 2026-10-02 问过「加第二个路径」；界面上给的是**能直接粘贴的那一行**，
 * 而不是一个点了没反应的输入框。
 */

/** 轮询一个后台任务直到它离开 running。 */
function useJobPoll() {
  const [job, setJob] = useState<Job | null>(null);
  useEffect(() => {
    if (!job || job.status !== 'running') return;
    const kind = job.kind;
    const batchId = job.batch_id;
    const t = setInterval(async () => {
      try {
        const next =
          kind === 'scan' ? await api.jobs.scan(batchId) : await api.jobs.scrape(batchId);
        setJob(next);
      } catch {
        /* 轮询失败不打断页面，下一次再试 */
      }
    }, 1000);
    return () => clearInterval(t);
  }, [job]);
  return [job, setJob] as const;
}

function Progress({ job }: { job: Job }) {
  const pct = job.total > 0 ? Math.round(((job.done + job.failed + job.skipped) / job.total) * 100) : 0;
  return (
    <div className="mt-4 border-t border-line pt-4">
      <div className="flex items-baseline gap-2.5">
        <b className="text-sm">
          {job.kind === 'scan' ? '扫描中' : '刮削中'}
          {job.status !== 'running' && (job.status === 'done' ? '（已完成）' : '（失败）')}
        </b>
        <span className="text-note text-ink-3">
          {job.done} 成功 · {job.failed} 失败 · {job.skipped} 跳过 / 共 {job.total}
        </span>
      </div>
      <div className="my-3 h-1.5 overflow-hidden rounded-full bg-white/12">
        <i className="block h-full rounded-full bg-accent transition-[width]" style={{ width: `${pct}%` }} />
      </div>
      {job.message && <div className="text-note text-ink-3">{job.message}</div>}
    </div>
  );
}

/** 小圆角标签。`tone` 决定配色，只用来表达「好 / 有问题 / 中性」。 */
function Chip({ tone, children }: { tone: 'ok' | 'bad' | 'mute'; children: React.ReactNode }) {
  const cls =
    tone === 'ok'
      ? 'bg-white/8 text-ink-2'
      : tone === 'bad'
        ? 'bg-accent-soft text-accent'
        : 'bg-white/6 text-ink-3';
  return <span className={`shrink-0 rounded-full px-2 py-[3px] text-xs ${cls}`}>{children}</span>;
}

/**
 * 一个库根的展示。
 *
 * 三件事必须一眼可见，因为它们各自对应一类「库是空的但不知道为什么」：
 *   * 路径**是不是存在**（`readable`）—— 外接盘没挂上时就是这个样子；
 *   * 解析到**哪个绝对路径**（`resolved`）—— 配置里写的是 `./fixtures` 这种相对路径，
 *     不显示解析结果的话没人知道它到底指哪；
 *   * 有没有**嵌在别的根里** —— 嵌套会让同一批文件被扫两遍。
 */
function RootRow({ root, index }: { root: LibraryRoot; index: number }) {
  // resolved 与 path 相同时不重复显示（配置里本来就写的绝对路径）
  const showResolved = root.resolved !== null && root.resolved !== root.path;
  return (
    <div className="border-b border-line-weak py-3 last:border-b-0">
      <div className="flex flex-wrap items-center gap-2.5">
        <span className="text-micro tracking-[.18em] text-ink-4 uppercase">根 {index + 1}</span>
        <code className="min-w-0 flex-1 truncate font-mono text-xs text-ink" title={root.path}>
          {root.path}
        </code>
        {root.readable ? <Chip tone="ok">可读</Chip> : <Chip tone="bad">读不到</Chip>}
        {root.nested_in && <Chip tone="bad">嵌在别的根里</Chip>}
      </div>
      {(showResolved || root.nested_in || !root.readable) && (
        <div className="mt-1.5 space-y-1 pl-[52px] text-xs text-ink-3">
          {showResolved && (
            <div>
              实际指向 <code className="font-mono text-ink-2">{root.resolved}</code>
            </div>
          )}
          {root.nested_in && (
            <div className="text-accent">
              它在 <code className="font-mono">{root.nested_in}</code> 里面 —— 同一个根嵌套另一个根，
              会让同一批文件被扫两遍。建议只留外层那个。
            </div>
          )}
          {!root.readable && (
            <div className="text-accent">
              服务端看不到这个目录：外接盘没挂上、路径拼错、或者进程权限不够。
              扫描时这一根会<b>整根跳过</b>（不会启动失败），所以库看起来永远是空的。
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * 「曲库路径」面板 —— **只读展示 + 给一行能粘贴的配置**，绝不代写配置文件。
 *
 * 为什么不做一个「添加路径」的输入框直接写进 config.json：**写了也不会生效**。
 * 配置优先级是 CLI > 环境变量 > config.json > 默认值，而 `serve` 是先 `Config::load()`
 * 读文件、再 `apply_env()`。所以环境变量（含 `.env`）一旦设了，config.json 那一层
 * 就是死的。一个「点了看起来成功了、重启后什么都没变」的按钮比没有按钮更坏。
 *
 * 与其假装能改，不如把**该改哪里**说准：`roots_env` 非空 ⇒ 明确指出是环境变量赢了，
 * 并给出把新路径拼进现有值的那一行。
 */
function RootsPanel({ stats }: { stats: LibraryStats }) {
  const envValue = stats.roots_env.value;
  const [extra, setExtra] = useState('');
  const trimmed = extra.trim();
  // 分隔符：后端 `split_roots` 认 `:` 和 `,`。用 `:` 跟 env.example 的示例一致。
  const suggested = envValue === null ? null : `${envValue}:${trimmed}`;

  return (
    <Panel
      title="曲库路径"
      actions={<span className="text-xs text-ink-4">共 {stats.roots.length} 个根</span>}
    >
      {stats.roots.length === 0 ? (
        <p className="py-6 text-center text-nav text-ink-4">没有配置任何库根。</p>
      ) : (
        stats.roots.map((root, i) => <RootRow key={root.path} root={root} index={i} />)
      )}

      <div className="mt-4 rounded-md bg-black/20 p-4">
        <div className="text-note text-ink-2">
          {envValue === null ? (
            <>
              库根目前来自<b>配置文件或内置默认值</b>（环境变量{' '}
              <code className="font-mono">{stats.roots_env.var}</code> 没设）。
            </>
          ) : (
            <>
              库根由环境变量 <code className="font-mono text-accent">{stats.roots_env.var}</code>{' '}
              决定。它的优先级<b>高于配置文件</b> —— 所以改 config.json 里的{' '}
              <code className="font-mono">storage.library_roots</code> 不会有任何效果。
            </>
          )}
        </div>

        <div className="mt-3 text-xs text-ink-3">
          加第二个根：改下面这个变量，多个根用 <code className="font-mono">:</code> 或{' '}
          <code className="font-mono">,</code> 分隔（Windows 上只能用{' '}
          <code className="font-mono">,</code>，因为盘符本身就带冒号）。<b>改完要重启服务。</b>
        </div>

        {envValue !== null && (
          <>
            <div className="mt-2.5 flex flex-wrap items-center gap-2">
              <input
                value={extra}
                onChange={(e) => setExtra(e.target.value)}
                placeholder="/Volumes/外接盘/Music"
                spellCheck={false}
                className="h-[34px] min-w-[220px] flex-1 rounded-md bg-black/30 px-3 font-mono text-xs text-ink outline-none placeholder:text-ink-4 focus:ring-1 focus:ring-accent"
              />
            </div>
            {trimmed && suggested && (
              <div className="mt-2.5">
                <div className="text-xs text-ink-3">把这行写进 <code className="font-mono">.env</code>（替换原来那行）：</div>
                <code className="mt-1.5 block overflow-x-auto rounded-sm bg-black/40 px-3 py-2 font-mono text-xs whitespace-pre text-ink-2">
                  {stats.roots_env.var}="{suggested}"
                </code>
              </div>
            )}
          </>
        )}

        <div className="mt-3 border-t border-line-weak pt-3 text-xs text-accent">
          加了新根之后，<b>文件不会自己进来</b> —— 文件监听还没接上，必须再手动跑一次「扫描」。
        </div>
      </div>
    </Panel>
  );
}

/** 一格计数。数字大、标签小。 */
function Tile({ label, value, hint }: { label: string; value: number; hint?: string }) {
  return (
    <div className="rounded-md bg-black/20 px-3.5 py-3">
      <div className="text-item font-medium tabular-nums">{value.toLocaleString()}</div>
      <div className="text-xs text-ink-3">{label}</div>
      {hint && <div className="mt-0.5 text-micro text-ink-4">{hint}</div>}
    </div>
  );
}

/**
 * 「曲库概况」面板。
 *
 * 口径都写进 hint 里了，因为它们各自有一个**很容易踩的误解**：
 *   * 已刮削 + 待刮削 + 刮削中 + 失败 == 入库数（软删的既不进总数也不进这四档）；
 *   * 「歌手」是**去重后的 `artists` 字符串个数**，不是人头 —— 合唱的一首算一个；
 *   * 「软删」是磁盘上已经找不到、数据库还留着的行。
 */
function CountsPanel({ stats }: { stats: LibraryStats }) {
  const c = stats.counts;
  const scraped = c.scrape.done;
  const pct = c.songs > 0 ? Math.round((scraped / c.songs) * 100) : 0;
  return (
    <Panel
      title="曲库概况"
      actions={<span className="text-xs text-ink-4">已刮削 {pct}%</span>}
    >
      <div className="grid grid-cols-2 gap-2.5 min-[701px]:grid-cols-4">
        <Tile label="已入库" value={c.songs} hint="不含已软删" />
        <Tile label="已刮削" value={scraped} />
        <Tile label="待刮削" value={c.scrape.pending} />
        <Tile label="刮削失败" value={c.scrape.failed} />
      </div>
      <div className="mt-2.5 grid grid-cols-2 gap-2.5 min-[701px]:grid-cols-4">
        <Tile label="刮削中" value={c.scrape.processing} />
        <Tile label="专辑" value={c.albums} />
        <Tile label="歌手" value={c.artists} hint="按 artists 串去重" />
        <Tile label="已软删" value={c.deleted} hint="文件没了、记录还在" />
      </div>
      <div className="mt-3 text-xs text-ink-4">
        前四项之和等于「已入库」—— 软删的歌既不进总数也不进这四档。
      </div>
    </Panel>
  );
}

export function LibrarySection() {
  const stats = useAsync(useCallback(() => api.library.stats(), []));
  const [scanJob, setScanJob] = useJobPoll();
  const [scrapeJob, setScrapeJob] = useJobPoll();
  // 默认 **false = 只入库**。危险的那一档必须用户显式打开，
  // 而不是默认打开再让人去关 —— 刮削不可撤销，默认值就是安全边界。
  // 这一档是**全局共享**的：曲目表里每行「重新刮削」读的是同一个值（lib/scrape-prefs.ts），
  // 否则两处语义不同，用户没法解释。
  const writeFiles = useWriteFiles();
  const [ack, setAck] = useState(false); // 打开写入后还要再确认一次
  const [err, setErr] = useState<string | null>(null);

  async function start(kind: 'scan' | 'scrape') {
    setErr(null);
    try {
      const acc =
        kind === 'scan'
          ? await api.jobs.startScan()
          : await api.jobs.startScrape({ write_files: writeFiles });
      const job: Job = {
        batch_id: acc.batch_id,
        kind: acc.kind,
        status: 'running',
        started_at: Date.now(),
        finished_at: null,
        total: 0,
        done: 0,
        failed: 0,
        skipped: 0,
        message: null,
      };
      // 立刻拉一次拿到总数，进度条才有分母
      const first =
        kind === 'scan' ? await api.jobs.scan(acc.batch_id) : await api.jobs.scrape(acc.batch_id);
      const full = { ...job, ...first };
      if (kind === 'scan') setScanJob(full);
      else setScrapeJob(full);
    } catch (e) {
      setErr(messageOf(e));
    }
  }

  return (
    <>
      {err && (
        <p className="mb-4 rounded-md bg-accent-soft px-4 py-3 text-note text-accent">{err}</p>
      )}

      {stats.error ? (
        <Panel title="曲库路径">
          <p className="py-8 text-center text-nav text-ink-3">{stats.error}</p>
        </Panel>
      ) : stats.loading || !stats.data ? (
        <Panel title="曲库路径">
          <p className="py-8 text-center text-nav text-ink-4">读取中…</p>
        </Panel>
      ) : (
        <>
          <RootsPanel stats={stats.data} />
          <CountsPanel stats={stats.data} />
        </>
      )}

      <Panel title="扫描">
        <Row
          label="把曲库根下的音频文件入库"
          hint="扫描是重操作，并发触发会被 409 拒绝。只读文件、不改内容；新歌一律进「待刮削」。"
        >
          <button
            onClick={() => start('scan')}
            disabled={scanJob?.status === 'running'}
            className="h-[34px] rounded-full bg-surface px-3.5 text-nav transition-colors hover:bg-surface-hover disabled:opacity-40"
          >
            {scanJob?.status === 'running' ? '扫描中…' : '开始扫描'}
          </button>
        </Row>
        {scanJob && <Progress job={scanJob} />}
      </Panel>

      <Panel title="刮削">
        {/* 这个开关现在**真的有用** —— 后端 POST /api/scrape 支持 write_files。
            以前只能在界面上拦一道，拦不住写入本身。 */}
        <label className="flex cursor-pointer items-center gap-3">
          <span
            onClick={() => {
              setWriteFilesPref(!writeFiles);
              setAck(false); // 换档位就把确认清掉，别让上一次的勾选顺延到更危险的那档
            }}
            className={[
              'relative h-[23px] w-10 shrink-0 rounded-full transition-colors',
              writeFiles ? 'bg-accent' : 'bg-white/16',
            ].join(' ')}
          >
            <span
              className={[
                'absolute top-[3px] left-[3px] size-[17px] rounded-full bg-ink transition-transform',
                writeFiles ? 'translate-x-[17px]' : '',
              ].join(' ')}
            />
          </span>
          <span className="min-w-0">
            <span className="text-nav">把刮削结果写入音乐文件</span>
            <br />
            <span className="text-xs text-ink-3">
              {writeFiles
                ? '会直接覆盖原文件标签，没有备份、不能撤销'
                : '只更新数据库，一个字节都不碰你的文件'}
            </span>
          </span>
        </label>

        {writeFiles && (
          <div className="mt-3.5 flex items-start gap-3 rounded-md bg-accent-soft px-4 py-3 leading-[18px] text-accent">
            <input
              id="ack"
              type="checkbox"
              checked={ack}
              onChange={(e) => setAck(e.target.checked)}
              className="mt-0.5 shrink-0 accent-accent"
            />
            <label htmlFor="ack" className="cursor-pointer text-note">
              我确认要覆盖原文件。建议先关掉这个开关跑一次「只入库」看看结果，
              确认插件写出来的标签是你要的，再打开它。
            </label>
          </div>
        )}

        <div className="mt-4 flex flex-wrap gap-2.5">
          <button
            onClick={() => start('scrape')}
            disabled={(writeFiles && !ack) || scrapeJob?.status === 'running'}
            className="h-[34px] rounded-full bg-accent px-4 text-nav font-medium text-white transition-colors hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
          >
            {scrapeJob?.status === 'running'
              ? '刮削中…'
              : writeFiles
                ? '开始刮削并写入文件'
                : '开始刮削（只入库）'}
          </button>
        </div>
        {scrapeJob && <Progress job={scrapeJob} />}
      </Panel>
    </>
  );
}
