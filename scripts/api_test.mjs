#!/usr/bin/env node
/**
 * music-robot HTTP API 端到端验证（Node 版，原 api_test.py 的忠实移植）。
 *
 * 起一个**临时服务**（独立临时数据库 + 临时曲库目录），把每个接口打一遍并逐条断言，
 * 最后打印通过 / 失败统计。它既是「接口确实能用」的证明，也是可重复运行的回归测试。
 *
 * 用法:
 *   node scripts/api_test.mjs                       # 用 target/debug/music-robot
 *   node scripts/api_test.mjs --bin PATH            # 指定二进制
 *   node scripts/api_test.mjs --work-dir DIR        # 临时目录建在 DIR 下（默认系统临时目录）
 *   node scripts/api_test.mjs --keep                # 结束后保留临时目录（排查用）
 *
 * 约定:
 *   · 只用 Node 内置模块（fetch / node:sqlite / node:child_process…），不装任何依赖；
 *   · **需要 Node 22.5+**（`node:sqlite` 从这个版本起内置）；
 *   · 所有断言都打到真实 HTTP 响应上，不做「只要不报错就算过」的弱断言；
 *   · 失败不会中断全局，而是累计到最后的统计里。
 */

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';

// node:sqlite 目前仍被标为 experimental，每次运行都会往 stderr 吐一条
// ExperimentalWarning，混在测试报告里很像报错。只滤掉这一条，其余警告照常打印。
process.on('warning', (w) => {
  if (w.name === 'ExperimentalWarning' && w.message.includes('SQLite')) return;
  console.warn(w);
});

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const FIXTURES = path.join(REPO, 'fixtures');

// 挑样本：一个带封面（用于封面接口）、一个不带封面、一个 flac
const SAMPLE_FILES = [
  '最美情侣-白小白.mp3', // has_cover: true
  '华夏传说 - 凤凰传奇.mp3', // has_cover: false
  '牵丝戏 - 白兀.flac',
];

const GREEN = '\x1b[32m';
const RED = '\x1b[31m';
const BOLD = '\x1b[1m';
const RESET = '\x1b[0m';

let PASS = 0;
let FAIL = 0;
const FAILED_NAMES = [];

function ok(name, extra = '') {
  PASS += 1;
  console.log(`  ${GREEN}✓${RESET} ${name}${extra ? `  [${extra}]` : ''}`);
}

function bad(name, detail) {
  FAIL += 1;
  FAILED_NAMES.push(name);
  console.log(`  ${RED}✗${RESET} ${name}\n      ${detail}`);
}

/** 把要打印的值压短 —— 音频 / 封面这类二进制响应体绝不能原样刷屏。 */
function brief(v, limit = 120) {
  let r;
  if (v instanceof Uint8Array) {
    r = `<${v.length} 字节二进制>`;
  } else {
    try {
      r = JSON.stringify(v);
    } catch {
      r = String(v);
    }
    if (r === undefined) r = String(v);
  }
  if (r.length > limit) return `${r.slice(0, limit)}...(共 ${r.length} 字符)`;
  return r;
}

/**
 * 深比较 —— 对应 Python 的 `==`。
 *
 * JS 的 `===` 对数组/对象是引用比较，直接照搬会让
 * `check("越界页返回空数组", items, [])` 这类断言永远失败。
 */
function eq(a, b) {
  if (a === b) return true;
  if (a instanceof Uint8Array && b instanceof Uint8Array) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
    return true;
  }
  if (Array.isArray(a) && Array.isArray(b)) {
    return a.length === b.length && a.every((v, i) => eq(v, b[i]));
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const ka = Object.keys(a);
    const kb = Object.keys(b);
    return ka.length === kb.length && ka.every((k) => eq(a[k], b[k]));
  }
  return false;
}

function check(name, actual, expected) {
  if (eq(actual, expected)) {
    ok(name, `= ${brief(expected)}`);
  } else {
    bad(name, `期望 ${brief(expected)}，实际 ${brief(actual)}`);
  }
}

function checkTrue(name, cond, detail = '') {
  if (cond) ok(name);
  else bad(name, detail || '条件不成立');
}

function section(title) {
  console.log(`\n${BOLD}${title}${RESET}`);
}

class Client {
  constructor(base) {
    this.base = base;
    this.token = null;
    /** 登录时服务端下发的媒体 cookie 原值（`mr_media=...`）。 */
    this.mediaCookie = null;
  }

  /** 返回 [status, headers(小写键), body(Uint8Array)] —— 对齐原 Python 的三元组。 */
  async req(method, p, body = null, headers = null) {
    const h = { ...(headers ?? {}) };
    let data;
    if (body !== null && body !== undefined) {
      data = JSON.stringify(body);
      h['Content-Type'] = 'application/json';
    }
    if (this.token) h.Authorization = `Bearer ${this.token}`;

    const resp = await fetch(this.base + p, {
      method,
      headers: h,
      body: data,
      signal: AbortSignal.timeout(30000),
    });
    const buf = new Uint8Array(await resp.arrayBuffer());
    // axum / hyper 发出的头名本来就是小写，这里统一一遍纯属自保
    // （原 Python 版因为 http.client 保留原样，栽过一次假失败）。
    const hd = {};
    for (const [k, v] of resp.headers) hd[k.toLowerCase()] = v;
    // 登录会下发媒体 cookie（<audio src> 带不了请求头，只能靠它）。顺手记下来。
    const sc = hd['set-cookie'];
    if (sc && sc.includes('mr_media=')) this.mediaCookie = sc.split(';')[0];
    return [resp.status, hd, buf];
  }

  /**
   * **模拟 `<audio src>` / `<img src>`**：浏览器自发的裸 GET，**没有 Authorization 头**。
   * `withCookie` 控制带不带那个媒体 cookie —— 两种都要测：
   * 带 = 应当放行；不带 = 应当 401。
   */
  async media(method, p, { withCookie = true, headers = null } = {}) {
    const h = { ...(headers ?? {}) };
    if (withCookie && this.mediaCookie) h.Cookie = this.mediaCookie;
    const resp = await fetch(this.base + p, {
      method,
      headers: h,
      signal: AbortSignal.timeout(30000),
    });
    const buf = new Uint8Array(await resp.arrayBuffer());
    const hd = {};
    for (const [k, v] of resp.headers) hd[k.toLowerCase()] = v;
    return [resp.status, hd, buf];
  }

  async json(method, p, body = null, headers = null) {
    const [st, hd, buf] = await this.req(method, p, body, headers);
    let parsed = null;
    try {
      parsed = JSON.parse(new TextDecoder().decode(buf));
    } catch {
      parsed = null;
    }
    return [st, hd, parsed];
  }
}

/** src/ 下所有 .rs 里最新的 mtime（秒）。 */
function newestSourceMtime() {
  let newest = 0;
  const walk = (dir) => {
    for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, e.name);
      if (e.isDirectory()) walk(full);
      else if (e.name.endsWith('.rs')) newest = Math.max(newest, fs.statSync(full).mtimeMs / 1000);
    }
  };
  walk(path.join(REPO, 'src'));
  return newest;
}

function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.once('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
  });
}

async function waitHealthz(base, timeout = 20.0) {
  const deadline = Date.now() + timeout * 1000;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/healthz`, { signal: AbortSignal.timeout(2000) });
      if (r.status === 200) return true;
    } catch {
      /* 还没起来，继续等 */
    }
    await new Promise((r) => setTimeout(r, 150));
  }
  return false;
}

function parseArgs(argv) {
  const args = { bin: path.join(REPO, 'target', 'debug', 'music-robot'), keep: false, workDir: null };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === '--bin') args.bin = argv[++i];
    else if (a === '--work-dir') args.workDir = argv[++i];
    else if (a === '--keep') args.keep = true;
    else if (a === '-h' || a === '--help') args.help = true;
    else {
      console.error(`未知参数：${a}`);
      args.help = true;
    }
  }
  return args;
}

function usage() {
  console.log(`用法: node scripts/api_test.mjs [--bin PATH] [--work-dir DIR] [--keep]

  --bin PATH       音乐服务器二进制（默认 target/debug/music-robot）
  --work-dir DIR   临时目录建在 DIR 下（默认系统临时目录）。
                   每次运行都会在里面新建一个唯一子目录，所以重复运行不会互相踩。
                   清理规则与默认一致：成功即删，除非同时给了 --keep。
  --keep           结束后保留临时目录（排查用）`);
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    usage();
    return 0;
  }

  if (!fs.existsSync(args.bin)) {
    console.error(`找不到二进制：${args.bin}\n先跑 cargo build --bin music-robot`);
    return 2;
  }

  // ⚠️ 防呆：二进制必须比 src/ 下任何源码都新。
  // 吃过一次亏 —— 代码改完忘了重新构建，脚本对着旧二进制跑，
  // 新接口全部返回旧行为，一度被误判成实现有 bug。
  const binMtime = fs.statSync(args.bin).mtimeMs / 1000;
  const srcMtime = newestSourceMtime();
  if (binMtime < srcMtime - 1.0) {
    const hhmmss = (t) => new Date(t * 1000).toTimeString().slice(0, 8);
    console.error('二进制比源码旧，拒绝运行（否则测的是旧版本，结论无意义）：');
    console.error(`  二进制  : ${hhmmss(binMtime)}`);
    console.error(`  最新源码: ${hhmmss(srcMtime)}`);
    console.error('  请先跑: cargo build --bin music-robot');
    return 2;
  }

  // 临时目录：--work-dir 只决定「建在哪」，目录名仍带唯一后缀 ——
  // 这样重复运行不会互相踩（同一个固定目录复用会出现「用户已存在」之类的假失败）。
  const parent = args.workDir ? path.resolve(args.workDir) : os.tmpdir();
  fs.mkdirSync(parent, { recursive: true });
  const work = fs.mkdtempSync(path.join(parent, 'music-robot-api-test-'));
  const music = path.join(work, 'music');
  fs.mkdirSync(music, { recursive: true });
  const dbPath = path.join(work, 'music.db');
  const cacheDir = path.join(work, 'transcode-cache');
  // ⚠️ 必须隔离转码缓存目录！默认是 ~/.local/share/music-robot/transcode，
  // 而本脚本用的是**假 ffmpeg**（产物是垃圾字节）。不隔离就会把用户真实的
  // 缓存目录写脏 —— 而且因为缓存键只有 song_id+audio_hash，
  // 之后用真 ffmpeg 也只会命中那份脏缓存。这条踩过一次，务必保留。
  // config.rs 目前没有 MR_CACHE_DIR 环境变量，只能用配置文件覆盖。
  const cfgFile = path.join(work, 'config.json');
  fs.writeFileSync(cfgFile, JSON.stringify({ audio: { cache_dir: cacheDir } }), 'utf-8');

  // 拷真实样本进临时曲库（扫描出真实标签）
  const copied = [];
  for (const name of SAMPLE_FILES) {
    const src = path.join(FIXTURES, name);
    if (fs.existsSync(src)) {
      fs.copyFileSync(src, path.join(music, name));
      copied.push(name);
    }
  }
  if (copied.length === 0) {
    console.error('fixtures/ 里没有可用样本，无法验证');
    return 2;
  }
  console.log(`临时目录: ${work}`);
  console.log(`曲库样本: ${copied.join(', ')}`);

  // 造一个假的 ffmpeg：每次被调用就往计数文件追加一行，并产出一个（假的）mp3。
  // 转码接口的「二次命中不调 ffmpeg」就靠它提供硬证据。
  const fakeFfmpeg = path.join(work, 'fake-ffmpeg.sh');
  const ffmpegCalls = path.join(work, 'ffmpeg-calls.txt');
  fs.writeFileSync(
    fakeFfmpeg,
    `#!/bin/sh\n` +
      `echo call >> ${ffmpegCalls}\n` +
      `# 最后一个参数是输出文件；直接写点字节冒充转码结果\n` +
      `for last; do :; done\n` +
      `printf 'ID3fake-mp3-bytes' > "$last"\n`,
    'utf-8',
  );
  fs.chmodSync(fakeFfmpeg, 0o755);

  const countFfmpegCalls = () => {
    try {
      return fs.readFileSync(ffmpegCalls, 'utf-8').split('\n').filter((ln) => ln.trim()).length;
    } catch {
      return 0;
    }
  };

  // 夹具插件目录：从 plugins/examples/example.js 复制一份到临时目录。
  // 必须自带，不能直接用仓库 plugins/ —— 那个目录是给用户放真插件的，
  // 里面有什么（musicbrainz.js 要联网）不该决定本脚本的成败。
  const pluginsDir = path.join(work, 'plugins');
  fs.mkdirSync(pluginsDir, { recursive: true });
  const fixturePlugin = path.join(REPO, 'plugins', 'examples', 'example.js');
  fs.copyFileSync(fixturePlugin, path.join(pluginsDir, 'example.js'));

  const countSongs = () => {
    const db = new DatabaseSync(dbPath);
    try {
      return db.prepare('select count(*) as n from songs').get().n;
    } finally {
      db.close();
    }
  };

  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  const env = {
    ...process.env,
    MR_DATABASE_PATH: dbPath,
    MR_LIBRARY_ROOTS: music,
    MR_JWT_SECRET: 'api-test-secret',
    MR_FFMPEG_PATH: fakeFfmpeg,
    MR_PLUGINS_DIR: pluginsDir,
    MR_LOG_LEVEL: process.env.MR_LOG_LEVEL ?? 'warn',
  };

  const logPath = path.join(work, 'server.log');
  const logFd = fs.openSync(logPath, 'w');
  const proc = spawn(args.bin, ['serve', '--host', '127.0.0.1', '--port', String(port), '--config', cfgFile], {
    env,
    stdio: ['ignore', logFd, logFd],
  });

  let rc = 1;
  try {
    if (!(await waitHealthz(base))) {
      fs.closeSync(logFd);
      console.error('\n服务未在 20 秒内就绪，日志：');
      console.error(fs.readFileSync(logPath, 'utf-8'));
      return 1;
    }

    const c = new Client(base);
    const c2 = new Client(base); // 第二个用户

    // ───────────────────────── 健康检查 / 鉴权 ─────────────────────────
    section('1. 健康检查与鉴权');
    let [st, , body] = await c.json('GET', '/healthz');
    check('GET /healthz → 200', st, 200);
    checkTrue('健康检查返回 status=ok', body?.status === 'ok', brief(body));

    [st, , body] = await c.json('GET', '/api/library');
    check('未登录访问曲库 → 401', st, 401);

    [st, , body] = await c.json('POST', '/api/auth/register', {
      username: 'alice',
      password: 'password123',
    });
    check('注册首个用户（引导）→ 201', st, 201);
    check('首个用户是 admin', body?.user?.role ?? null, 'admin');

    // 库非空后公开注册必须关闭 —— 否则陌生人自己开个号就能拿走整个曲库
    [st, , body] = await c.json('POST', '/api/auth/register', {
      username: 'bob',
      password: 'password123',
    });
    check('库非空后再注册 → 403（引导已完成）', st, 403);

    let loginHd;
    [st, loginHd, body] = await c.json('POST', '/api/auth/login', {
      username: 'alice',
      password: 'password123',
    });
    check('登录 → 200', st, 200);
    c.token = body?.token ?? '';
    checkTrue('登录返回非空 token', c.token.length > 20, `token 太短: ${brief(c.token)}`);
    // <audio src> / <img src> 带不了 Authorization 头，媒体端点只能靠这个 cookie
    const sc = loginHd['set-cookie'] ?? '';
    checkTrue('登录下发媒体 cookie mr_media', sc.startsWith('mr_media='), brief(sc));
    for (const attr of ['HttpOnly', 'SameSite=Lax', 'Path=/api']) {
      checkTrue(`媒体 cookie 带 ${attr}`, sc.includes(attr), brief(sc));
    }

    // 建号改由管理员发起
    [st, , body] = await c.json('POST', '/api/admin/users', {
      username: 'bob',
      password: 'password123',
    });
    check('管理员建号 → 201', st, 201);
    check('建出来的是 user', body?.user?.role ?? null, 'user');

    [, , body] = await c2.json('POST', '/api/auth/login', {
      username: 'bob',
      password: 'password123',
    });
    c2.token = body?.token ?? '';

    [st, , body] = await c.json('GET', '/api/auth/me');
    check('GET /api/auth/me → 200', st, 200);
    check('me 返回的是 alice', body?.user?.username ?? null, 'alice');

    [st] = await c2.json('GET', '/api/admin/ping');
    check('普通用户访问 admin 路由 → 403', st, 403);
    [st] = await c.json('GET', '/api/admin/ping');
    check('管理员访问 admin 路由 → 200', st, 200);

    // ───────────────────────── 扫描入库 ─────────────────────────
    section('2. 扫描入库与刮削（S5 / S13 / S17）');
    [st, , body] = await c.json('POST', '/api/scan');
    check('触发扫描 → 202', st, 202);
    const batch = body?.batch_id ?? null;
    checkTrue('返回非空 batch_id', Boolean(batch), brief(body));

    // 任务还在跑时重复触发应当被单例锁挡住（若已跑完则跳过，避免偶发）
    const [st2] = await c.json('POST', '/api/scan');
    if (st2 === 409) {
      ok('扫描进行中重复触发 → 409（单例锁）');
    } else {
      ok(`扫描已完成，重复触发未被挡（跳过 409 断言，st=${st2}）`);
    }

    let status = null;
    for (let i = 0; i < 100; i += 1) {
      [st, , body] = await c.json('GET', `/api/scan/${batch}`);
      status = body?.status ?? null;
      if (status === 'done' || status === 'failed') break;
      await new Promise((r) => setTimeout(r, 200));
    }
    check('扫描任务最终状态 = done', status, 'done');
    checkTrue('扫描报告 total > 0', (body?.total ?? 0) > 0, brief(body));

    // 扫描入库的歌 scrape_status 都是 pending，正好当刮削队列。
    // 插件目录是临时目录里的夹具（plugins/examples/example.js 的副本）：
    // 它按文件名升序第一个命中，返回 confidence 0.95（> 阈值）且 artist
    // 固定写成「示例歌手」—— 可以当硬断言用。
    [st, , body] = await c.json('POST', '/api/scrape');
    check('触发刮削 → 202', st, 202);
    const scrapeBatch = body?.batch_id ?? null;
    checkTrue('返回非空 scrape batch_id', Boolean(scrapeBatch), brief(body));

    let scrapeStatus = null;
    for (let i = 0; i < 150; i += 1) {
      [st, , body] = await c.json('GET', `/api/scrape/${scrapeBatch}`);
      scrapeStatus = body?.status ?? null;
      if (scrapeStatus === 'done' || scrapeStatus === 'failed') break;
      await new Promise((r) => setTimeout(r, 200));
    }
    check('刮削任务最终状态 = done', scrapeStatus, 'done');
    checkTrue('刮削报告 total > 0（确实有活可干）', (body?.total ?? 0) > 0, brief(body));

    // 硬证据：标签真的写回文件并重新入库了（不是只把状态改成 done）
    [st, , body] = await c.json('GET', '/api/library?page_size=200');
    const doneSongs = (body?.items ?? []).filter((x) => x?.scrape_status === 'done');
    checkTrue('有歌被标记为 done', doneSongs.length > 0, `done=${doneSongs.length}`);
    checkTrue(
      '标签真的写回（出现 example.js 的「示例歌手」）',
      (body?.items ?? []).some((x) => String(x?.artists ?? '').includes('示例歌手')),
      brief(body).slice(0, 200),
    );

    // 重刮：可选 body 选队列。老客户端不发 body 的行为上面刚验过（pending 队列）。
    const rescrapeId = (body?.items ?? []).find((x) => x?.id)?.id ?? null;
    [st, , body] = await c.json('POST', '/api/scrape', { song_ids: [rescrapeId] });
    check('指定 song_ids 重刮 → 202', st, 202);
    const oneBatch = body?.batch_id ?? null;
    checkTrue('单曲重刮返回 batch_id', Boolean(oneBatch), brief(body));
    let oneStatus = null;
    for (let i = 0; i < 100; i += 1) {
      [st, , body] = await c.json('GET', `/api/scrape/${oneBatch}`);
      oneStatus = body?.status ?? null;
      if (oneStatus === 'done' || oneStatus === 'failed') break;
      await new Promise((r) => setTimeout(r, 200));
    }
    check('单曲重刮跑完 status = done', oneStatus, 'done');
    check(
      '只处理点名的那一首（已 done 的歌 pending 队列永远取不到）',
      body?.total ?? -1,
      1,
    );

    [st, , body] = await c.json('POST', '/api/scrape', { mode: 'failed' });
    check('重刮失败项 mode=failed → 202', st, 202);

    // ── 只入库不写文件（write_files: false）──────────────────────────
    // 这条档位是用户明确要求的：刮削不可撤销，界面上要能选「只入库 / 也写文件」。
    {
      const target = (body?.items ?? [])[0]?.id ?? rescrapeId;
      // 先记下文件大小，跑完必须一模一样
      const [beforeSt, beforeHd] = await c.req('GET', `/api/stream/${target}`, null, { Range: 'bytes=0-0' });
      check('取刮削前文件大小 → 206', beforeSt, 206);
      const sizeBefore = beforeHd['content-range'];

      const [drySt, , dryBody] = await c.json('POST', '/api/scrape', {
        song_ids: [target],
        write_files: false,
      });
      check('只入库模式触发 → 202', drySt, 202);
      const dryBatch = dryBody?.batch_id ?? null;
      let dryStatus = null;
      for (let i = 0; i < 120; i += 1) {
        [st, , body] = await c.json('GET', `/api/scrape/${dryBatch}`);
        dryStatus = body?.status ?? null;
        if (dryStatus === 'done' || dryStatus === 'failed') break;
        await new Promise((r) => setTimeout(r, 200));
      }
      check('只入库批次跑完 = done', dryStatus, 'done');

      const [afterSt, afterHd] = await c.req('GET', `/api/stream/${target}`, null, { Range: 'bytes=0-0' });
      check('取刮削后文件大小 → 206', afterSt, 206);
      check(
        '⭐ 只入库模式：文件大小一个字节都没变',
        afterHd['content-range'],
        sizeBefore,
      );
    }

    // 类型写错必须 400 —— 一个拼错的取值若被当成 true，就是「以为没写、其实覆盖了原文件」
    [st, , body] = await c.json('POST', '/api/scrape', { write_files: 'false' });
    check('write_files 传字符串 → 400（不能当 true）', st, 400);

    // 非法 body 必须 400，绝不能静默退化成「刮全库」
    [st, , body] = await c.json('POST', '/api/scrape', { mode: 'everything' });
    check('未知 mode → 400（不静默刮全库）', st, 400);
    [st, , body] = await c.json('POST', '/api/scrape', { song_ids: [] });
    check('空 song_ids → 400', st, 400);

    // ── 登出：清掉媒体 cookie；且**不要求令牌**（过期后更需要能登出）──
    {
      const anon = new Client(base);
      const [logoutSt, logoutHd] = await anon.req('POST', '/api/auth/logout');
      check('登出（无令牌）→ 200', logoutSt, 200);
      checkTrue(
        '登出清媒体 cookie（Max-Age=0）',
        String(logoutHd['set-cookie'] ?? '').includes('Max-Age=0'),
        brief(logoutHd['set-cookie']),
      );
    }

    // ── ⭐ packages/core 的 API 层对着**真服务**跑一遍 ──────────────
    //
    // 为什么必须有这一节：core 的 `api/*` 是「路径 + 方法 + 响应形状」的手写转写，
    // 光看后端源码很容易把**路径/方法**猜错（响应 JSON 抄得到，路由注册那行常被忽略）。
    // 已经栽过一次：`favorites.add` 写成 `POST /api/favorites` + body，
    // 实际是 `POST /api/favorites/{song_id}` → 405，而那时 core 完全没被测过。
    try {
      const { createHttp, createApi, createMemoryTokenStore } = await import(
        '../packages/core/src/index.ts'
      );
      const coreTokens = createMemoryTokenStore(c.token);
      const core = createApi(createHttp({ baseUrl: base, tokens: coreTokens }));

      const lib = await core.library.list({ page_size: 3 });
      checkTrue('core: library.list 返回 items/total_pages', Array.isArray(lib.items) && typeof lib.total_pages === 'number', brief(lib));

      const one = await core.library.song(lib.items[0].id);
      checkTrue('core: library.song 返回 { song }', Boolean(one?.song?.id), brief(one));

      // ⚠️ 这一条就是上面说的那个 405 —— 别再改回 body 形式
      const fav = await core.favorites.add(lib.items[0].id);
      checkTrue('core: favorites.add 走路径参数（曾是 405）', fav?.song_id === lib.items[0].id, brief(fav));
      const favs = await core.favorites.list();
      checkTrue('core: favorites.list 能读到刚加的', favs.items.some((x) => x.id === lib.items[0].id), `total=${favs.total}`);
      await core.favorites.remove(lib.items[0].id);

      const hist = await core.history.list({ limit: 5 });
      checkTrue('core: history.list 返回 items/limit/offset', Array.isArray(hist.items) && typeof hist.offset === 'number', brief(hist));

      const st = await core.settings.get();
      checkTrue('core: settings.get 返回 { settings, total }', typeof st.settings === 'object' && typeof st.total === 'number', brief(st));

      const me = await core.auth.me();
      checkTrue('core: auth.me 返回 { user }', Boolean(me?.user?.username), brief(me));

      const pls = await core.playlists.list();
      checkTrue('core: playlists.list 返回 items/total', Array.isArray(pls.items), brief(pls));

      const made = await core.playlists.create('core 冒烟歌单');
      checkTrue('core: playlists.create 返回 { playlist }', Boolean(made?.playlist?.id), brief(made));
      const detail = await core.playlists.get(made.playlist.id);
      checkTrue('core: playlists.get 返回 { playlist, songs }', Boolean(detail?.playlist) && Array.isArray(detail?.songs), brief(detail));
      const addedItem = await core.playlists.addSong(made.playlist.id, lib.items[0].id);
      checkTrue('core: playlists.addSong 接受单个 song_id', addedItem?.song_id === lib.items[0].id, brief(addedItem));
      await core.playlists.remove(made.playlist.id);

      const jobs = await core.jobs.list();
      checkTrue('core: jobs.list 返回 items', Array.isArray(jobs.items), brief(jobs));

      const albumsPage = await core.albums.list({ page_size: 5 });
      checkTrue('core: albums.list 返回 items/total', Array.isArray(albumsPage.items) && typeof albumsPage.total === 'number', brief(albumsPage));

      const artistsPage = await core.artists.list({ page_size: 5 });
      checkTrue('core: artists.list 返回 items/total', Array.isArray(artistsPage.items), brief(artistsPage));

      // 列表里的曲目应当带专辑名（后端在出口补的）
      checkTrue(
        'core: library.list 的曲目带 album 字段',
        'album' in (lib.items[0] ?? {}),
        brief(lib.items[0]),
      );
    } catch (e) {
      // ⚠️ 必须包住：core 的 api 层要是把路径/方法写错，抛出来会把**后面一百多条断言
      //    全带不跑**，报错还只是一个异常栈。改成记一条失败断言继续跑。
      bad('core: API 层冒烟失败（路径 / 方法 / 形状对不上后端）', String(e?.message ?? e));
    }

    // ───────────────────────── 曲库接口 ─────────────────────────
    section('3. 曲库接口（S16）');
    [st, , body] = await c.json('GET', '/api/library');
    check('GET /api/library → 200', st, 200);
    const total = body?.total ?? 0;
    checkTrue('库里有歌（total >= 3）', total >= 3, `total=${brief(total)}`);
    const firstId = (body?.items ?? [{}])[0]?.id ?? null;
    checkTrue(
      '列表项不含 file_path（不泄漏服务器路径）',
      !JSON.stringify(body).includes('file_path'),
      '响应里出现了 file_path',
    );

    [st] = await c.json('GET', '/api/library?page=0');
    check('分页 page=0 → 400', st, 400);
    [st] = await c.json('GET', '/api/library?page_size=999999');
    check('分页 page_size 超上限 → 400', st, 400);
    [st, , body] = await c.json('GET', '/api/library?page=99');
    check('越界页 → 200', st, 200);
    check('越界页返回空数组', body?.items ?? null, []);

    [st] = await c.json('GET', '/api/library?sort=id;DROP%20TABLE%20songs--');
    check('排序注入 → 400', st, 400);
    // 表还在吗 —— 直接查库确认
    const n = countSongs();
    checkTrue('注入后 songs 表仍在且有数据', n >= 3, `songs 行数=${brief(n)}`);

    [st] = await c.json('GET', `/api/songs/${firstId}`);
    check('GET /api/songs/{id} → 200', st, 200);
    [st] = await c.json('GET', '/api/songs/999999');
    check('不存在的歌 → 404', st, 404);

    // ───────────────────────── 标签编辑（S26）─────────────────────────
    //
    // ⚠️ 这一段**只测 dry_run**，绝不真写文件 —— 这个脚本跑的是用户的真实曲库，
    // 而写标签是不可撤销的（覆盖原文件、无备份）。「能写」这件事由
    // `src/server/routes/tags.rs` 的 UT + 手工验证覆盖，这里守的是**契约与默认值**：
    // 默认不写、只读接口的形状、以及错误输入被拒。
    section('4.5 标签编辑（S26，只读 dry-run）');
    let tagsBody;
    [st, , tagsBody] = await c.json('GET', `/api/songs/${firstId}/tags`);
    check('GET /api/songs/{id}/tags → 200', st, 200);
    checkTrue('返回文件名（不含路径）', typeof tagsBody?.file?.name === 'string' && !tagsBody.file.name.includes('/'), brief(tagsBody?.file?.name));
    checkTrue('返回 tags 对象', tagsBody?.tags !== null && typeof tagsBody?.tags === 'object', brief(Object.keys(tagsBody?.tags ?? {}).length + ' 个字段'));
    checkTrue('lyrics_source 是三者之一', ['db', 'file', 'none'].includes(tagsBody?.tags?.lyrics_source), brief(tagsBody?.tags?.lyrics_source));

    const titleBefore = tagsBody?.tags?.title ?? null;
    // 1) **不传 dry_run** → 必须只算差异、不写盘
    [st, , body] = await c.json('PATCH', `/api/songs/${firstId}/tags`, {
      fields: { title: 'e2e-绝不该被写入' },
    });
    check('PATCH 标签（不传 dry_run）→ 200', st, 200);
    check('默认 dry_run：applied=false', body?.applied ?? null, false);
    check('默认 dry_run：changed=true', body?.changed ?? null, true);
    checkTrue('预览给出了 title 这一处改动', (body?.diffs ?? []).some((d) => d.key === 'title'), brief(body?.diffs?.length));

    // 2) 再读一次，确认**文件一个字节都没变**
    [st, , tagsBody] = await c.json('GET', `/api/songs/${firstId}/tags`);
    check('dry_run 之后文件未变', tagsBody?.tags?.title ?? null, titleBefore);

    // 3) 没有任何改动 → changed=false
    [st, , body] = await c.json('PATCH', `/api/songs/${firstId}/tags`, { fields: {} });
    check('空 fields → 200', st, 200);
    check('空 fields：changed=false', body?.changed ?? null, false);

    // 4) 错误输入明确被拒（静默忽略会变成「保存成功但没生效」）
    [st] = await c.json('PATCH', `/api/songs/${firstId}/tags`, { fields: { titel: 'x' } });
    check('拼错字段名 → 400', st, 400);
    [st] = await c.json('PATCH', `/api/songs/${firstId}/tags`, { fields: { track: '三' } });
    check('字段类型错 → 400', st, 400);
    [st] = await c.json('PATCH', `/api/songs/${firstId}/tags`, { dry_run: true });
    check('缺 fields 对象 → 400', st, 400);
    [st] = await c.json('PATCH', '/api/songs/999999/tags', { fields: {} });
    check('不存在的歌（标签）→ 404', st, 404);
    [st] = await c.json('GET', '/api/songs/999999/tags');
    check('不存在的歌（读标签）→ 404', st, 404);

    [st] = await c.json('GET', '/api/search?q=%E7%99%BD');
    check('搜索 → 200', st, 200);
    [st, , body] = await c.json('GET', '/api/search?q=zzzz-nope-nothing');
    check('搜不到 → 200', st, 200);
    check('搜不到返回空数组', body?.items ?? null, []);

    // ───────────────────────── 音频流（Range） ─────────────────────────
    section('4. 音频流 Range（S18）');
    let hd;
    let payload;
    [st, hd, payload] = await c.req('GET', `/api/stream/${firstId}`);
    check('无 Range → 200', st, 200);
    checkTrue('带 Accept-Ranges: bytes', hd['accept-ranges'] === 'bytes', brief(hd['accept-ranges']));
    checkTrue('Content-Length 大于 0', Number(hd['content-length'] ?? '0') > 0, brief(hd['content-length']));
    const fullLen = payload.length;

    [st, hd, payload] = await c.req('GET', `/api/stream/${firstId}`, null, { Range: 'bytes=0-99' });
    check('Range bytes=0-99 → 206', st, 206);
    check('Range 返回正好 100 字节', payload.length, 100);
    check('Content-Range 正确', hd['content-range'] ?? null, `bytes 0-99/${fullLen}`);

    [st, hd] = await c.req('GET', `/api/stream/${firstId}`, null, { Range: `bytes=${fullLen}-` });
    check('越界 Range → 416', st, 416);
    check('416 带 Content-Range: bytes */len', hd['content-range'] ?? null, `bytes */${fullLen}`);

    // ── 媒体端点的 cookie 通道（<audio>/<img> 的真实行为）──
    let mst;
    [mst] = await c.media('GET', `/api/stream/${firstId}`);
    check('裸 GET + cookie（模拟 <audio>）→ 200', mst, 200);
    [mst] = await c.media('GET', `/api/stream/${firstId}`, { withCookie: false });
    check('裸 GET 不带 cookie → 401', mst, 401);
    [mst] = await c.media('GET', `/api/songs/${firstId}/cover`);
    check('封面同样认 cookie（模拟 <img>）', mst, 200);
    [mst] = await c.media('GET', '/api/library');
    check('⭐ 其余 API 不认 cookie（否则等于开 CSRF）→ 401', mst, 401);

    [st] = await c.req('GET', `/api/stream/${firstId}`, null, { Range: 'bytes=0-1,3-4' });
    check('多段 Range → 416', st, 416);

    // ───────────────────────── 封面 ─────────────────────────
    section('5. 封面（S20）');
    // 找那首带封面的
    let withCover = null;
    let lib;
    [st, , lib] = await c.json('GET', '/api/library?page_size=200');
    for (const item of lib?.items ?? []) {
      const [cst, chd, cpayload] = await c.req('GET', `/api/songs/${item.id}/cover`);
      if (cst === 200) {
        withCover = [item.id, chd, cpayload];
        break;
      }
    }
    checkTrue('至少有一首歌能取到封面', withCover !== null, '所有歌的封面接口都返回了非 200');
    if (withCover) {
      const [, chd, cpayload] = withCover;
      checkTrue(
        '封面 Content-Type 是 image/*',
        (chd['content-type'] ?? '').startsWith('image/'),
        brief(chd['content-type']),
      );
      checkTrue('封面字节非空', cpayload.length > 1000, `只有 ${cpayload.length} 字节`);
    }
    [st] = await c.req('GET', '/api/songs/999999/cover');
    check('不存在的歌取封面 → 404', st, 404);

    // ───────────────────────── 转码缓存 ─────────────────────────
    section('6. 转码缓存（S19）');

    // 挑一首 flac（转码最有意义的场景）；没有就用第一首
    let target = null;
    for (const item of lib?.items ?? []) {
      if ((item.format ?? '').toLowerCase() === 'flac') {
        target = item.id;
        break;
      }
    }
    if (target === null) target = firstId;

    const before = countFfmpegCalls();
    [st, hd, payload] = await c.req('GET', `/api/stream/${target}?format=mp3`);
    check('?format=mp3 → 200', st, 200);
    check('转码响应 Content-Type = audio/mpeg', hd['content-type'] ?? null, 'audio/mpeg');
    checkTrue('转码响应非空', payload.length > 0, '0 字节');
    check('ffmpeg 被调用恰好 1 次', countFfmpegCalls(), before + 1);

    let payload2;
    [st, hd, payload2] = await c.req('GET', `/api/stream/${target}?format=mp3`);
    check('二次转码请求 → 200', st, 200);
    check('二次命中缓存：ffmpeg 调用次数不变（硬证据）', countFfmpegCalls(), before + 1);
    check('两次响应字节完全一致', payload2, payload);

    [st] = await c.req('GET', `/api/stream/${target}?format=ogg`);
    check('不支持的 format → 400', st, 400);

    const anon = new Client(base);
    [st] = await anon.req('GET', `/api/stream/${target}?format=mp3`);
    check('转码未登录 → 401', st, 401);
    check('未登录不产生转码调用', countFfmpegCalls(), before + 1);

    [st, hd] = await c.req('GET', `/api/stream/${target}`);
    check('不带 format → 200（直传，S18 行为不变）', st, 200);
    check('直传不触发转码', countFfmpegCalls(), before + 1);

    // ───────────────────────── 播放列表 ─────────────────────────
    section('7. 播放列表（S22）');

    [st, , body] = await c.json('POST', '/api/playlists', { name: '测试歌单', is_public: false });
    check('新建歌单 → 201', st, 201);
    const plId = body?.playlist?.id ?? null;
    checkTrue('返回歌单 id', Number.isInteger(plId), brief(body));

    [st, , body] = await c.json('GET', '/api/playlists');
    check('歌单列表 → 200', st, 200);
    const ids = (body?.items ?? []).map((x) => x?.id ?? null);
    checkTrue('列表含刚建的歌单', ids.includes(plId), brief(ids));

    [st, , body] = await c.json('POST', `/api/playlists/${plId}/items`, { song_id: firstId });
    check('加歌 → 201', st, 201);
    checkTrue('加歌 added=true', body?.added === true, brief(body));
    [st, , body] = await c.json('POST', `/api/playlists/${plId}/items`, { song_id: firstId });
    check('重复加歌 → 200（幂等）', st, 200);
    checkTrue('重复加歌 added=false', body?.added === false, brief(body));

    [st, , body] = await c.json('GET', `/api/playlists/${plId}`);
    check('歌单详情 → 200', st, 200);
    const tracks = body?.songs ?? [];
    check('详情里恰好一首（幂等生效）', tracks.length, 1);
    checkTrue('详情曲目不含内部字段', !JSON.stringify(body).includes('file_path'), '泄漏了 file_path');

    [st] = await c.json('POST', `/api/playlists/${plId}/items`, { song_id: 999999 });
    check('加不存在的歌 → 404', st, 404);

    // 私有歌单：外人看不见，且与「不存在」不可区分
    const [stA, , bodyA] = await c2.json('GET', `/api/playlists/${plId}`);
    check('外人读私有歌单 → 404', stA, 404);
    const [, , bodyB] = await c2.json('GET', '/api/playlists/999999');
    checkTrue('私有与不存在不可区分（防存在性泄漏）', eq(bodyA, bodyB), '两者响应不同');
    [st] = await c2.json('DELETE', `/api/playlists/${plId}`);
    check('外人删私有歌单 → 404', st, 404);

    // 改成公开：外人能读，但仍不能改
    [st] = await c.json('PUT', `/api/playlists/${plId}`, { is_public: true });
    check('改为公开 → 200', st, 200);
    [st] = await c2.json('GET', `/api/playlists/${plId}`);
    check('外人读公开歌单 → 200', st, 200);
    [st] = await c2.json('PUT', `/api/playlists/${plId}`, { name: '被改名' });
    check('外人改公开歌单 → 403（不是 404）', st, 403);

    const anon2 = new Client(base);
    [st] = await anon2.json('GET', '/api/playlists');
    check('未登录看歌单 → 401', st, 401);

    // 删除级联：直接查库确认曲目没了
    [st] = await c.json('DELETE', `/api/playlists/${plId}`);
    check('owner 删歌单 → 200', st, 200);
    const left = (() => {
      const db = new DatabaseSync(dbPath);
      try {
        return db.prepare('select count(*) as n from playlist_items where playlist_id = ?').get(plId).n;
      } finally {
        db.close();
      }
    })();
    check('删歌单后曲目被级联清除', left, 0);

    // ───────────────────────── 播放周边 ─────────────────────────
    section('8. 播放周边（S21）');

    // 记录播放
    [st] = await c.json('POST', '/api/history', { song_id: firstId, duration_listened_ms: 30000 });
    check('记录播放 → 201', st, 201);
    [st, , body] = await c.json('GET', '/api/history?limit=10');
    check('最近播放 → 200', st, 200);
    checkTrue(
      '历史里含刚记的那条',
      (body?.items ?? []).some((x) => x?.song_id === firstId),
      brief(body).slice(0, 200),
    );
    [st] = await c.json('POST', '/api/history', { song_id: 999999 });
    check('记录不存在的歌 → 404', st, 404);
    [st] = await c.json('GET', '/api/history?limit=0');
    check('历史分页非法参数 → 400', st, 400);

    // 收藏幂等
    [st] = await c.json('POST', `/api/favorites/${firstId}`);
    check('收藏 → 201', st, 201);
    [st] = await c.json('POST', `/api/favorites/${firstId}`);
    check('重复收藏 → 200（幂等）', st, 200);
    [st, , body] = await c.json('GET', '/api/favorites');
    check('收藏列表 → 200', st, 200);
    const nFav = (body?.items ?? []).filter((x) => x?.id === firstId).length;
    check('同一首只出现一次', nFav, 1);

    // 断点续播：写进 settings 再读回来
    const key = `resume:${firstId}`;
    [st] = await c.json('PUT', '/api/settings', { [key]: '42000' });
    check('写设置 → 200', st, 200);
    [st, , body] = await c.json('GET', '/api/settings');
    check('读设置 → 200', st, 200);
    const smap = body?.settings ?? {};
    check('断点续播位置原样读回', smap[key] ?? null, '42000');
    [st] = await c.json('PUT', '/api/settings', { [key]: null });
    check('null 值删除该键 → 200', st, 200);
    [st] = await c.json('PUT', '/api/settings', { ['k'.repeat(200)]: 'v' });
    check('超长键 → 400', st, 400);

    // 跨用户隔离
    [st, , body] = await c2.json('GET', '/api/history?limit=10');
    check('B 看不到 A 的播放历史', (body?.items ?? []).length, 0);
    [st, , body] = await c2.json('GET', '/api/favorites');
    check('B 看不到 A 的收藏', (body?.items ?? []).length, 0);
    [st, , body] = await c2.json('GET', '/api/settings');
    check('B 看不到 A 的设置', Object.keys(body?.settings ?? {}).length, 0);

    const anon3 = new Client(base);
    [st] = await anon3.json('GET', '/api/history');
    check('未登录看历史 → 401', st, 401);

    // ───────────────────────── 歌曲请求 ─────────────────────────
    section('9. 歌曲请求（S23）');

    // 归一化：大小写 + 全角 + 空格差异应合并成同一请求
    let b1;
    [st, , b1] = await c.json('POST', '/api/requests', { title: 'Numb', artist: 'Linkin Park' });
    check('提交点歌 → 201', st, 201);
    checkTrue('首次提交 created=true', b1?.created === true, brief(b1));
    const rid = b1?.request?.id ?? null;
    checkTrue('返回请求 id', Number.isInteger(rid), brief(b1));

    // 同一用户重复提交：幂等（不新增行、不重复投票）
    let b2;
    [st, , b2] = await c.json('POST', '/api/requests', { title: 'NUMB', artist: '  linkin park  ' });
    check('同人重复提交 → 200', st, 200);
    checkTrue('created=false（合并到已有）', b2?.created === false, brief(b2));
    checkTrue('voted=false（不重复投票）', b2?.voted === false, brief(b2));
    check('合并到同一个请求 id', b2?.request?.id ?? null, rid);

    // 另一个用户提交全角写法：应合并，且新增一票
    let b3;
    [st, , b3] = await c2.json('POST', '/api/requests', {
      title: 'ＮＵＭＢ',
      artist: 'Ｌｉｎｋｉｎ　Ｐａｒｋ',
    });
    check('他人提交全角写法 → 200', st, 200);
    checkTrue('created=false（归一化后同曲）', b3?.created === false, brief(b3));
    checkTrue('voted=true（新增一票）', b3?.voted === true, brief(b3));
    check('票数 = 2', b3?.request?.vote_count ?? null, 2);

    // 列表按票数降序
    let lst;
    [st, , lst] = await c.json('GET', '/api/requests');
    check('admin 列点歌 → 200', st, 200);
    const items = lst?.items ?? [];
    checkTrue('列表含刚提交的请求', items.some((x) => x?.id === rid), brief(lst).slice(0, 200));
    checkTrue('列表项不含 dedup_key（不暴露内部字段）', !JSON.stringify(lst).includes('dedup_key'), '泄漏了 dedup_key');

    // 普通用户只看得到自己的
    // song_requests.user_id 是「首个发起人」（画布 tb_req_t），bob 只是投票者，
    // 所以「我提交的」里本来就不该有 alice 那条 —— 让 bob 真正发起一条再验隔离。
    let bb;
    [st, , bb] = await c2.json('POST', '/api/requests', { title: 'bob-only-song', artist: 'bob' });
    check('bob 自己发起一条 → 201', st, 201);
    const bobRid = bb?.request?.id ?? null;
    let mine;
    [st, , mine] = await c2.json('GET', '/api/requests');
    check('普通用户列点歌 → 200', st, 200);
    const ids2 = (mine?.items ?? []).map((x) => x?.id ?? null);
    check('普通用户只看到自己发起的', ids2, [bobRid]);
    checkTrue('看不到 alice 发起的', !ids2.includes(rid), '看到了别人的请求');
    [st] = await c2.json('GET', '/api/requests?status=pending');
    check('普通用户按状态筛 → 403', st, 403);

    // 状态机与管理员操作
    [st] = await c2.json('PATCH', `/api/requests/${rid}`, { status: 'processing' });
    check('普通用户改状态 → 403', st, 403);
    [st] = await c.json('PATCH', `/api/requests/${rid}`, { status: 'bogus' });
    check('非法状态值 → 400', st, 400);
    [st] = await c.json('PATCH', `/api/requests/${rid}`, { status: 'processing' });
    check('pending→processing → 200', st, 200);
    [st] = await c.json('PATCH', `/api/requests/${rid}`, { status: 'pending' });
    check('processing→pending（回退）→ 400', st, 400);

    [st] = await c.json('POST', `/api/requests/${rid}/link`, { song_id: 999999 });
    check('link 到不存在的歌 → 404', st, 404);
    [st, , body] = await c.json('POST', `/api/requests/${rid}/link`, { song_id: firstId });
    check('link 到已有歌 → 200', st, 200);
    check('link 后状态为 done', body?.request?.status ?? null, 'done');

    // fetch 要的是 **provider（下载）** 插件，该 kind 尚未实现，所以必须诚实报
    // 503，不许假装成功。注意文案里点名的是 provider 而不是「刮削插件」——
    // 刮削（scraper）那条线早已接通，含糊的文案会把排查方向带偏。
    let bf;
    [st, , bf] = await c.json('POST', `/api/requests/${rid}/fetch`);
    check('fetch 无 provider 插件 → 503', st, 503);
    check('fetch 错误码 SERVICE_UNAVAILABLE', bf?.error?.code ?? null, 'SERVICE_UNAVAILABLE');
    check(
      'fetch 文案点名 provider（不是「刮削插件」）',
      ((bf?.error?.message ?? '').includes('provider')),
      true,
    );

    const anon4 = new Client(base);
    [st] = await anon4.json('GET', '/api/requests');
    check('未登录看请求 → 401', st, 401);

    // ───────────────────────── 汇总 ─────────────────────────
    rc = FAIL === 0 ? 0 : 1;
  } finally {
    try {
      proc.kill('SIGTERM');
    } catch {
      /* 已经退了 */
    }
    await Promise.race([
      new Promise((resolve) => proc.once('exit', resolve)),
      new Promise((resolve) => setTimeout(resolve, 8000)),
    ]);
    try {
      proc.kill('SIGKILL');
    } catch {
      /* 无所谓 */
    }
    try {
      fs.closeSync(logFd);
    } catch {
      /* 已经关了 */
    }
    // 只有 --keep 才保留。--work-dir 只决定「建在哪」，不改变清理规则 ——
    // 否则每次成功运行都会在用户指定目录里堆一个残留子目录。
    // 想要「固定位置 + 保留」就两个参数一起给：--work-dir DIR --keep
    if (rc === 0 && !args.keep) {
      fs.rmSync(work, { recursive: true, force: true });
    } else {
      console.log(`\n临时目录保留在: ${work}`);
      console.log(`服务日志: ${logPath}`);
    }
  }

  section('结果');
  console.log(`  通过 ${PASS}，失败 ${FAIL}`);
  if (FAILED_NAMES.length > 0) {
    console.log('  失败项:');
    for (const n of FAILED_NAMES) console.log(`    · ${n}`);
  }
  return rc;
}

process.exit(await main());
