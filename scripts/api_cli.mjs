#!/usr/bin/env node
/**
 * music-robot 交互式 API 客户端（TUI 风格菜单）。
 *
 * 连**已经在跑的服务**，不自己起进程 —— 想试哪个接口就选哪个，令牌在会话里保持。
 *
 * 用法:
 *   node scripts/api_cli.mjs                          # 默认连 http://127.0.0.1:8080
 *   node scripts/api_cli.mjs --base http://host:9000
 *
 * 约定:
 *   · 只用 Node 内置模块（node:readline/promises + 内置 fetch），不装任何依赖；
 *   · 需要 Node 22.5+（与 api_test.mjs 同一套环境假设）；
 *   · 二进制响应（音频 / 封面）只报字节数与类型，绝不刷屏。
 */

import readline from 'node:readline/promises';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';

// ───────────────────────────── 终端着色 ─────────────────────────────
const G = '\x1b[32m';
const Y = '\x1b[33m';
const R = '\x1b[31m';
const B = '\x1b[1m';
const D = '\x1b[2m';
const X = '\x1b[0m';

const rl = readline.createInterface({ input: process.stdin, output: process.stdout });

// ⚠️ 不要用 `rl.question()`。stdin 不是 TTY 时（管道 / 重定向）有个坑：创建接口到
//    第一次提问之间那段异步代码（起手探活 fetch）期间到达的行**没有 'line' 监听者**，
//    会被直接丢掉；等真去问的时候流已经 EOF，question() 抛 ERR_USE_AFTER_CLOSE。
//    所以从创建接口那一刻就把行收进队列，问的时候先看队列。顺带让 Ctrl-D 能优雅退出。
const lineQueue = [];
const lineWaiters = [];
let stdinClosed = false;
rl.on('line', (l) => {
  const w = lineWaiters.shift();
  if (w) w(l);
  else lineQueue.push(l);
});
rl.on('close', () => {
  stdinClosed = true;
  while (lineWaiters.length > 0) lineWaiters.shift()(null);
});

/** 问一行。EOF（Ctrl-D）时返回默认值，不抛。 */
async function ask(label, dflt = '') {
  process.stdout.write(dflt === '' ? `${label}: ` : `${label} ${D}[${dflt}]${X}: `);
  let raw;
  if (lineQueue.length > 0) raw = lineQueue.shift();
  else if (stdinClosed) raw = null;
  else raw = await new Promise((res) => lineWaiters.push(res));
  if (raw === null) return dflt;
  const a = String(raw).trim();
  return a === '' ? dflt : a;
}

const pause = () => ask('回车继续');

// ───────────────────────────── 会话状态 ─────────────────────────────
const state = {
  base: 'http://127.0.0.1:8080',
  token: null,
  username: null,
  lastSongId: null,
  lastPlaylistId: null,
  lastRequestId: null,
};

class Api {
  async req(method, p, body = null, headers = null) {
    const h = { ...(headers ?? {}) };
    let data;
    if (body !== null && body !== undefined) {
      data = JSON.stringify(body);
      h['Content-Type'] = 'application/json';
    }
    if (state.token) h.Authorization = `Bearer ${state.token}`;
    try {
      const resp = await fetch(state.base + p, {
        method,
        headers: h,
        body: data,
        signal: AbortSignal.timeout(30000),
      });
      const buf = new Uint8Array(await resp.arrayBuffer());
      const hd = {};
      for (const [k, v] of resp.headers) hd[k.toLowerCase()] = v;
      return [resp.status, hd, buf];
    } catch (e) {
      throw new Error(
        `连不上 ${state.base} —— 服务起了吗？（${e.cause?.code ?? e.message}）`,
      );
    }
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
const api = new Api();

// ───────────────────────────── 输出辅助 ─────────────────────────────
const code = (s) => (s < 300 ? G : s < 500 ? Y : R);

function showCall(method, p) {
  console.log(`${D}→ ${method} ${p}${X}`);
}

/** 打出状态码 + JSON 体；期望值只作提示，不做断言（这是手动工具，不是测试）。 */
function show(st, body, expect) {
  const tag = expect ? `  ${D}（期望 ${expect}）${X}` : '';
  console.log(`${code(st)}HTTP ${st}${X}${tag}`);
  if (body !== null && body !== undefined) console.log(JSON.stringify(body, null, 2));
}

/** 二进制响应只报大小与类型。 */
function showBinary(st, hd, buf) {
  console.log(`${code(st)}HTTP ${st}${X}  ${buf.length} 字节  ${hd['content-type'] ?? ''}`);
}

async function call(method, p, body = null, headers = null, expect = null) {
  showCall(method, p);
  const [st, hd, buf] = await api.req(method, p, body, headers);
  let parsed = null;
  try {
    parsed = JSON.parse(new TextDecoder().decode(buf));
  } catch {
    parsed = null;
  }
  if (parsed !== null) show(st, parsed, expect);
  else showBinary(st, hd, buf);
  return [st, hd, parsed];
}

// ───────────────────────────── 各个动作 ─────────────────────────────

async function actHealth() {
  await call('GET', '/healthz', null, null, 200);
}

async function actRegister() {
  console.log(`${D}注册是**初始化引导**：只有库里一个用户都没有时才可用，之后一律 403。${X}`);
  const username = await ask('用户名', 'root');
  const password = await ask('口令（≥8 字符）', 'password123');
  const [st, , body] = await call('POST', '/api/auth/register', { username, password }, null, 201);
  if (st === 201) {
    state.token = null;
    state.username = null;
    console.log(`${G}✓ 建好了。用「登录」进来。${X}`);
    console.log(`${D}  角色 = ${body?.user?.role ?? '?'}${X}`);
  }
}

async function actLogin() {
  const username = await ask('用户名', state.username ?? 'root');
  const password = await ask('口令', 'password123');
  const [st, , body] = await call('POST', '/api/auth/login', { username, password }, null, 200);
  if (st === 200) {
    state.token = body.token;
    state.username = body.user?.username ?? username;
    console.log(`${G}✓ 已登录：${state.username}（角色 ${body.user?.role ?? '?'}）${X}`);
  }
}

async function actAdminCreate() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const username = await ask('新用户名');
  const password = await ask('口令（≥8 字符）', 'password123');
  const role = await ask('角色 user/admin', 'user');
  await call('POST', '/api/admin/users', { username, password, role }, null, 201);
}

async function actWhoami() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  await call('GET', '/api/auth/me', null, null, 200);
}

async function actScan() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const [st, , body] = await call('POST', '/api/scan', null, null, 202);
  if (st !== 202) return;
  const batch = body?.batch_id;
  process.stdout.write(`${D}等扫描跑完`);
  for (let i = 0; i < 100; i += 1) {
    const [, , s] = await api.json('GET', `/api/scan/${batch}`);
    if (s?.status === 'done' || s?.status === 'failed') {
      process.stdout.write(`${X}\n`);
      show(200, s);
      return;
    }
    process.stdout.write('.');
    await new Promise((r) => setTimeout(r, 300));
  }
  process.stdout.write(`${X}\n${Y}等超时了，用「任务状态」自己看。${X}\n`);
}

async function actLibrary() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const page = await ask('页码', '1');
  const size = await ask('每页', '10');
  const [st, , body] = await call('GET', `/api/library?page=${page}&page_size=${size}`, null, null, 200);
  if (st === 200 && body?.items?.length) {
    state.lastSongId = String(body.items[0].id);
    console.log(`\n${B}曲目：${X}`);
    for (const it of body.items) {
      console.log(
        `  ${G}${String(it.id).padStart(4)}${X}  ${(it.title ?? '?').slice(0, 34).padEnd(34)}  ${(it.artists ?? '').slice(0, 20).padEnd(20)}  ${it.format ?? ''}`,
      );
    }
    console.log(`${D}（首个 id 已记为默认）${X}`);
  }
}

async function actSearch() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const q = await ask('关键词', '白');
  await call('GET', `/api/search?q=${encodeURIComponent(q)}`, null, null, 200);
}

async function actSongDetail() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const id = await ask('歌曲 id', state.lastSongId ?? '');
  if (!id) return;
  state.lastSongId = id;
  await call('GET', `/api/songs/${id}`, null, null, 200);
}

async function actPlay() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const id = await ask('歌曲 id', state.lastSongId ?? '');
  if (!id) return;
  state.lastSongId = id;

  showCall('GET', `/api/stream/${id}`);
  let [st, hd, buf] = await api.req('GET', `/api/stream/${id}`);
  const full = buf.length;
  console.log(`${code(st)}HTTP ${st}${X}  ${D}（期望 200）${X}  整文件 ${full} 字节  ${hd['content-type']}  accept-ranges=${hd['accept-ranges']}`);

  showCall('GET', `/api/stream/${id}   Range: bytes=0-99`);
  [st, hd] = await api.req('GET', `/api/stream/${id}`, null, { Range: 'bytes=0-99' });
  console.log(`${code(st)}HTTP ${st}${X}  ${D}（期望 206）${X}  ${hd['content-range']}`);

  showCall('GET', `/api/stream/${id}   Range: bytes=${full}-`);
  [st] = await api.req('GET', `/api/stream/${id}`, null, { Range: `bytes=${full}-` });
  console.log(`${code(st)}HTTP ${st}${X}  ${D}（期望 416）${X}`);
}

async function actCover() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const id = await ask('歌曲 id', state.lastSongId ?? '');
  if (!id) return;
  showCall('GET', `/api/songs/${id}/cover`);
  const [st, hd, buf] = await api.req('GET', `/api/songs/${id}/cover`);
  console.log(`${code(st)}HTTP ${st}${X}  ${D}（期望 200；没封面的歌是 404）${X}  ${buf.length} 字节  ${hd['content-type'] ?? ''}`);
  if (st === 200 && buf.length > 0) {
    const out = path.join(os.tmpdir(), `mr-cover-${id}.jpg`);
    fs.writeFileSync(out, buf);
    console.log(`${G}✓ 存到 ${out}${X}`);
  }
}

async function actTranscode() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const id = await ask('歌曲 id', state.lastSongId ?? '');
  if (!id) return;
  showCall('GET', `/api/stream/${id}?format=mp3`);
  const t0 = Date.now();
  const [st, hd, buf] = await api.req('GET', `/api/stream/${id}?format=mp3`);
  const ms = Date.now() - t0;
  console.log(`${code(st)}HTTP ${st}${X}  ${D}（期望 200；没 ffmpeg 会 503）${X}  ${buf.length} 字节  ${hd['content-type'] ?? ''}  ${ms}ms`);
  console.log(`${D}再点一次通常更快 —— 命中磁盘缓存就不调 ffmpeg 了${X}`);
}

async function actPlaylistList() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const [st, , body] = await call('GET', '/api/playlists', null, null, 200);
  if (st === 200 && body?.items?.length) {
    for (const p of body.items) {
      console.log(`  ${G}${String(p.id).padStart(4)}${X}  ${p.name}  ${p.is_public ? '公开' : '私有'}  ${p.song_count ?? '?'} 首`);
    }
    state.lastPlaylistId = String(body.items[0].id);
  }
}

async function actPlaylistCreate() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const name = await ask('歌单名', '测试歌单');
  const isPublic = (await ask('公开？y/N', 'n')).toLowerCase().startsWith('y');
  const [st, , body] = await call('POST', '/api/playlists', { name, is_public: isPublic }, null, 201);
  if (st === 201) state.lastPlaylistId = String(body.playlist.id);
}

async function actPlaylistAdd() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const pl = await ask('歌单 id', state.lastPlaylistId ?? '');
  const song = await ask('歌曲 id', state.lastSongId ?? '');
  if (!pl || !song) return;
  state.lastPlaylistId = pl;
  await call('POST', `/api/playlists/${pl}/items`, { song_id: Number(song) }, null, '201 首次 / 200 重复');
}

async function actPlaylistDetail() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const pl = await ask('歌单 id', state.lastPlaylistId ?? '');
  if (!pl) return;
  state.lastPlaylistId = pl;
  await call('GET', `/api/playlists/${pl}`, null, null, 200);
}

async function actPlaylistDelete() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const pl = await ask('歌单 id', state.lastPlaylistId ?? '');
  if (!pl) return;
  await call('DELETE', `/api/playlists/${pl}`, null, null, 200);
}

async function actHistory() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const id = await ask('记录播放的歌曲 id（留空 = 只看历史）', '');
  if (id) await call('POST', '/api/history', { song_id: Number(id), duration_listened_ms: 30000 }, null, 201);
  await call('GET', '/api/history?limit=10', null, null, 200);
}

async function actFavorites() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const id = await ask('歌曲 id（留空 = 只看收藏列表）', '');
  if (id) {
    await call('POST', `/api/favorites/${id}`, null, null, '201 首次 / 200 重复');
    state.lastSongId = id;
  }
  await call('GET', '/api/favorites', null, null, 200);
}

async function actSettings() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  await call('GET', '/api/settings', null, null, 200);
  const key = await ask('写一个键（留空跳过）', '');
  if (key) {
    const val = await ask('值（写 null 删除该键）', '42000');
    await call('PUT', '/api/settings', { [key]: val === 'null' ? null : val }, null, 200);
  }
}

async function actRequestSubmit() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const title = await ask('歌名', 'Numb');
  const artist = await ask('歌手', 'Linkin Park');
  const [st, , body] = await call('POST', '/api/requests', { title, artist }, null, '201 首次 / 200 合并');
  if (body?.request?.id) state.lastRequestId = String(body.request.id);
}

async function actRequestList() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  await call('GET', '/api/requests', null, null, 200);
}

async function actRequestPatch() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const rid = await ask('请求 id', state.lastRequestId ?? '');
  if (!rid) return;
  state.lastRequestId = rid;
  const status = await ask('新状态 pending/processing/done/rejected', 'processing');
  await call('PATCH', `/api/requests/${rid}`, { status }, null, 200);
}

async function actRequestFetch() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  const rid = await ask('请求 id', state.lastRequestId ?? '');
  if (!rid) return;
  console.log(`${D}⚠️ 这条恒 503：provider（下载）插件 kind 还没实现 —— 是已知缺口，不是 bug${X}`);
  await call('POST', `/api/requests/${rid}/fetch`, null, null, 503);
}

async function actJobs() {
  if (!state.token) return console.log(`${Y}先登录。${X}`);
  await call('GET', '/api/jobs', null, null, 200);
}

async function actSwitchBase() {
  const b = await ask('服务地址', state.base);
  state.base = b.replace(/\/+$/, '');
  state.token = null;
  state.username = null;
  console.log(`${G}✓ 已切到 ${state.base}，令牌已清除${X}`);
}

// ───────────────────────────── 菜单 ─────────────────────────────

const MENU = [
  ['会话', null],
  ['健康检查', actHealth],
  ['注册（仅初始化引导）', actRegister],
  ['登录', actLogin],
  ['当前用户 /api/auth/me', actWhoami],
  ['管理员建号', actAdminCreate],
  ['切换服务地址 / 清除令牌', actSwitchBase],
  ['曲库', null],
  ['扫描入库', actScan],
  ['曲库列表', actLibrary],
  ['搜索', actSearch],
  ['歌曲详情', actSongDetail],
  ['播放 / Range 探测', actPlay],
  ['取封面（存文件）', actCover],
  ['转码为 mp3', actTranscode],
  ['任务列表', actJobs],
  ['歌单', null],
  ['歌单列表', actPlaylistList],
  ['新建歌单', actPlaylistCreate],
  ['加歌到歌单', actPlaylistAdd],
  ['歌单详情', actPlaylistDetail],
  ['删除歌单', actPlaylistDelete],
  ['播放周边', null],
  ['历史（可顺带记录播放）', actHistory],
  ['收藏', actFavorites],
  ['设置', actSettings],
  ['点歌请求', null],
  ['提交点歌', actRequestSubmit],
  ['点歌列表', actRequestList],
  ['改状态（admin）', actRequestPatch],
  ['fetch（已知恒 503）', actRequestFetch],
];

function header() {
  const who = state.token ? `${G}${state.username}${X}` : `${Y}未登录${X}`;
  const song = state.lastSongId ? `默认曲目 ${state.lastSongId}` : '默认曲目 -';
  console.log(`\n${B}════ music-robot 交互式 API ════${X}`);
  console.log(`  ${state.base}   ${who}   ${D}${song}${X}`);
  console.log(`${D}────────────────────────────────${X}`);
  let n = 1;
  for (const [label, fn] of MENU) {
    if (fn === null) {
      console.log(`  ${B}── ${label} ──${X}`);
    } else {
      console.log(`  ${G}${String(n).padStart(2)}${X}) ${label}`);
      n += 1;
    }
  }
  console.log(`   ${G}0${X}) 退出`);
}

async function main() {
  const argv = process.argv.slice(2);
  const bi = argv.indexOf('--base');
  if (bi >= 0 && argv[bi + 1]) state.base = argv[bi + 1].replace(/\/+$/, '');
  if (argv.includes('-h') || argv.includes('--help')) {
    console.log('用法: node scripts/api_cli.mjs [--base http://127.0.0.1:8080]');
    return 0;
  }

  // 先探一下服务在不在，省得进去每一步都报错
  try {
    const r = await fetch(`${state.base}/healthz`, { signal: AbortSignal.timeout(3000) });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    console.log(`${G}✓ 服务在线：${state.base}${X}`);
  } catch (e) {
    console.log(`${R}✗ 连不上 ${state.base}：${e.cause?.code ?? e.message}${X}`);
    console.log(`${D}  先起服务：./target/debug/music-robot serve --host 127.0.0.1 --port 8080${X}`);
    const go = await ask('仍要继续吗？y/N', 'n');
    if (!go.toLowerCase().startsWith('y')) {
      rl.close();
      return 1;
    }
  }

  // 菜单项按显示顺序编号（跳过标题行）
  const actions = MENU.filter(([, fn]) => fn !== null).map(([, fn]) => fn);

  for (;;) {
    header();
    const choice = await ask('选择', '0');
    if (choice === '0' || choice.toLowerCase() === 'q') break;
    const idx = Number(choice) - 1;
    if (!Number.isInteger(idx) || idx < 0 || idx >= actions.length) {
      console.log(`${Y}没有这一项。${X}`);
      continue;
    }
    console.log();
    try {
      await actions[idx]();
    } catch (e) {
      console.log(`${R}✗ ${e.message}${X}`);
    }
    await pause();
  }

  rl.close();
  console.log(`${D}再见。${X}`);
  return 0;
}

process.exit(await main());
