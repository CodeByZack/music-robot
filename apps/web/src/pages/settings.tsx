import { useCallback, useEffect, useState } from 'react';
import { RESUME_KEY_PREFIX, type Job } from '@music-robot/core';
import { useOverlayClose } from '@/lib/use-overlay-close.ts';
import { setWriteFiles as setWriteFilesPref, useWriteFiles } from '@/lib/scrape-prefs.ts';
import { api } from '@/lib/client.ts';
import { useAsync } from '@/lib/use-async.tsx';
import { messageOf, useSession } from '@/lib/session.tsx';

/** 轮询一个后台任务直到它离开 running。 */
function useJobPoll() {
  const [job, setJob] = useState<Job | null>(null);
  useEffect(() => {
    if (!job || job.status !== 'running') return;
    const kind = job.kind;
    const t = setInterval(async () => {
      try {
        const next = kind === 'scan' ? await api.jobs.scan(job.batch_id) : await api.jobs.scrape(job.batch_id);
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
        <span className="text-[12.5px] text-ink-3">
          {job.done} 成功 · {job.failed} 失败 · {job.skipped} 跳过 / 共 {job.total}
        </span>
      </div>
      <div className="my-3 h-1.5 overflow-hidden rounded-full bg-white/12">
        <i className="block h-full rounded-full bg-accent transition-[width]" style={{ width: `${pct}%` }} />
      </div>
      {job.message && <div className="text-[12.5px] text-ink-3">{job.message}</div>}
    </div>
  );
}

function Panel({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="mb-4 rounded-lg bg-surface p-[18px]">
      <div className="mb-3 text-[10px] tracking-[.24em] text-ink-4 uppercase">{title}</div>
      {children}
    </div>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children?: React.ReactNode }) {
  return (
    <div className="flex items-center gap-3.5 border-b border-line-weak py-[13px] last:border-b-0">
      <div className="min-w-0 flex-1">
        <b className="block text-[13.5px] font-normal">{label}</b>
        {hint && <span className="text-xs text-ink-3">{hint}</span>}
      </div>
      {children}
    </div>
  );
}

/** 设置的分节。左边那栏就按这个渲染。 */
const SECTIONS = [
  { id: 'library', label: '音乐库' },
  { id: 'playback', label: '播放' },
  { id: 'account', label: '账号' },
] as const;

type SectionId = (typeof SECTIONS)[number]['id'];

export default function SettingsPage() {
  const [section, setSection] = useState<SectionId>('library');

  // 关闭设置：优先回上一页，带动画（见 lib/use-overlay-close.ts）
  const { closing, close } = useOverlayClose('/');

  // Esc 关闭
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [close]);
  const { user, logout } = useSession();
  const [scanJob, setScanJob] = useJobPoll();
  const [scrapeJob, setScrapeJob] = useJobPoll();
  // 默认 **false = 只入库**。危险的那一档必须用户显式打开，
  // 而不是默认打开再让人去关 —— 刮削不可撤销，默认值就是安全边界。
  // 这一档是**全局共享**的：曲目表里每行「重新刮削」读的是同一个值（lib/scrape-prefs.ts），
  // 否则两处语义不同，用户没法解释。
  const writeFiles = useWriteFiles();
  const [ack, setAck] = useState(false); // 打开写入后还要再确认一次
  const [err, setErr] = useState<string | null>(null);
  const settings = useAsync(useCallback(() => api.settings.get(), []));

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
      const first = kind === 'scan' ? await api.jobs.scan(acc.batch_id) : await api.jobs.scrape(acc.batch_id);
      const full = { ...job, ...first };
      if (kind === 'scan') setScanJob(full);
      else setScrapeJob(full);
    } catch (e) {
      setErr(messageOf(e));
    }
  }

  const vol = settings.data?.settings.volume;
  const mode = settings.data?.settings.play_mode;
  const resumeCount = Object.keys(settings.data?.settings ?? {}).filter((k) => k.startsWith(RESUME_KEY_PREFIX)).length;

  return (
    /* 全屏接管（对齐飞牛的设置页）—— 不弹居中面板、不留遮罩缝隙。
       关：右上角 ✕ / Esc。进场 / 退场动画见 global.css 的 .anim-overlay-*。 */
    <div
      className={[
        'fixed inset-0 z-50 flex flex-col overflow-hidden bg-[#14121b]',
        closing ? 'anim-overlay-out' : 'anim-overlay-in',
      ].join(' ')}
    >
      <div className="flex h-[58px] shrink-0 items-center gap-2 border-b border-line-weak px-5">
          <h2 className="text-[15px] font-medium">设置</h2>
          <span className="flex-1" />
          <button
            type="button"
            onClick={close}
            title="关闭"
            aria-label="关闭设置"
            className="flex size-8 items-center justify-center rounded-full text-ink-3 transition-colors hover:bg-surface-hover hover:text-ink"
          >
            <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round">
              <path d="M4 4l8 8M12 4l-8 8" />
            </svg>
          </button>
        </div>

        <div className="flex min-h-0 flex-1 flex-col min-[701px]:flex-row">
          {/* 分节导航：窄屏横向 tab，桌面左栏 */}
          <nav className="flex shrink-0 gap-1 overflow-x-auto border-b border-line-weak p-2 min-[701px]:w-[186px] min-[701px]:flex-col min-[701px]:border-r min-[701px]:border-b-0 min-[701px]:p-3">
            {SECTIONS.map((s) => (
              <button
                key={s.id}
                type="button"
                onClick={() => setSection(s.id)}
                className={[
                  'shrink-0 rounded-md px-3 py-2 text-left text-[13px] transition-colors',
                  section === s.id ? 'bg-surface text-ink' : 'text-ink-3 hover:bg-surface-hover hover:text-ink',
                ].join(' ')}
              >
                {s.label}
              </button>
            ))}
          </nav>

          <div className="min-h-0 flex-1 overflow-auto p-5">
            {err && <p className="mb-4 rounded-md bg-accent-soft px-[14px] py-3 text-[12.5px] text-accent">{err}</p>}

            {section === 'library' && (
              <>
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
            <span className="text-[13.5px]">把刮削结果写入音乐文件</span>
            <br />
            <span className="text-xs text-ink-3">
              {writeFiles
                ? '会直接覆盖原文件标签，没有备份、不能撤销'
                : '只更新数据库，一个字节都不碰你的文件'}
            </span>
          </span>
        </label>

        {writeFiles && (
          <>
            <div className="mt-3.5 flex items-start gap-3 rounded-md bg-accent-soft px-[14px] py-3 leading-[18px] text-accent">
              <input
                id="ack"
                type="checkbox"
                checked={ack}
                onChange={(e) => setAck(e.target.checked)}
                className="mt-0.5 shrink-0 accent-accent"
              />
              <label htmlFor="ack" className="cursor-pointer text-[12.5px]">
                我确认要覆盖原文件。建议先关掉这个开关跑一次「只入库」看看结果，
                确认插件写出来的标签是你要的，再打开它。
              </label>
            </div>
          </>
        )}

        <div className="mt-4 flex flex-wrap gap-2.5">
          <button
            onClick={() => start('scrape')}
            disabled={(writeFiles && !ack) || scrapeJob?.status === 'running'}
            className="h-[34px] rounded-full bg-accent px-4 text-[13px] font-medium text-white transition-colors hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40"
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

      <Panel title="曲库">
        <Row label="上次扫描" hint="扫描是重操作，并发触发会被 409 拒绝">
          <button
            onClick={() => start('scan')}
            disabled={scanJob?.status === 'running'}
            className="h-[34px] rounded-full bg-surface px-3.5 text-[13px] transition-colors hover:bg-surface-hover disabled:opacity-40"
          >
            {scanJob?.status === 'running' ? '扫描中…' : '重新扫描'}
          </button>
        </Row>
        {scanJob && <Progress job={scanJob} />}
      </Panel>
              </>
            )}

            {section === 'playback' && (
              <Panel title="播放">
        <Row label="音量" hint={vol === undefined ? '后端没有这个键' : `settings.volume = ${vol}`}>
          <span className="rounded-sm bg-black/30 px-2.5 py-[7px] font-mono text-xs text-ink-3">
            {vol ?? '—'}
          </span>
        </Row>
        <Row label="播放模式" hint={mode === undefined ? '后端没有这个键' : `settings.play_mode = ${mode}`}>
          <span className="rounded-sm bg-black/30 px-2.5 py-[7px] font-mono text-xs text-ink-3">
            {mode ?? '—'}
          </span>
        </Row>
        <Row
          label="断点续播"
          hint={`位置存在 settings 的 ${RESUME_KEY_PREFIX}<song_id> 键里（这个数只反映加载时读到几条）。暂停 / 换歌立刻落盘，播放中每 15 秒一次；快听完时自动清掉，下次从头播`}
        >
          <span className="text-xs text-ink-3">已记住 {resumeCount} 首</span>
        </Row>
      </Panel>
            )}

            {section === 'account' && (
              <Panel title="账号">
        <Row label={user?.username ?? ''} hint={user?.role === 'admin' ? '管理员' : '普通用户'}>
          <button
            onClick={async () => {
              // 同时清服务端的媒体 cookie —— 它是 HttpOnly，JS 删不掉
              try {
                await api.auth.logout();
              } catch {
                /* 登出接口失败不该挡住本地登出 */
              }
              logout();
            }}
            className="h-[34px] rounded-full bg-surface px-3.5 text-[13px] transition-colors hover:bg-surface-hover"
          >
            退出登录
          </button>
        </Row>
      </Panel>
            )}
          </div>
        </div>
    </div>
  );
}
