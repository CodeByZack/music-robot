#!/usr/bin/env node
/**
 * music-robot 交互式 API 客户端（方向键菜单 / 二级目录）。
 *
 * 连**已经在跑的服务**，不自己起进程。↑↓ 选择、Enter 进入、Esc/← 返回、q 退出。
 *
 * 用法:
 *   node scripts/api_cli.mjs                       # 默认连 http://127.0.0.1:8080
 *   node scripts/api_cli.mjs --base http://host:9000
 *
 * 约定:
 *   · 只用 Node 内置模块（node:readline + 内置 fetch），零依赖单文件；
 *   · 需要 Node 22.5+；需要**交互式终端**（管道 / 重定向下没有方向键，直接报错退出）；
 *   · 二进制响应（音频 / 封面）只报字节数与类型，绝不刷屏。
 */

import readline from 'node:readline';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';

// ───────────────────────────── 终端 ─────────────────────────────
const G = '\x1b[32m';
const Y = '\x1b[33m';
const R = '\x1b[31m';
const C = '\x1b[36m';
const B = '\x1b[1m';
const D = '\x1b[2m';
const X = '\x1b[0m';

const TTY = Boolean(process.stdin.isTTY && process.stdout.isTTY);
const setRaw = (on) => {
  if (TTY && process.stdin.setRawMode) process.stdin.setRawMode(on);
};
if (TTY) readline.emitKeypressEvents(process.stdin);

const clearScreen = () => process.stdout.write('\x1b[2J\x1b[3J\x1b[H');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ───────────────────────────── 输入 ─────────────────────────────
//
// 自己实现行编辑，**不用 readline.Interface**。
// 原因：`rl.question()` 和这里的 raw 模式 keypress 会互相打架 —— 实测
// `createInterface()` + `question()` 会**立刻 resolve 出 undefined**（prompt 都没打印），
// 于是 ask() 直接退回了默认值，「注册成功」其实是没输入过的假成功。
// 反正菜单本来就在 raw 模式下，自己处理几个键反而更短更可控。

/** 在 raw 模式下读一行。`secret` 时回显 `*`。 */
function readLine(prompt, dflt = '', secret = false) {
  return new Promise((resolve) => {
    let val = '';
    const shown = () => (secret ? '*'.repeat(val.length) : val);
    const draw = () => process.stdout.write(`\r\x1b[2K${prompt}${shown()}`);

    setRaw(true);
    process.stdin.resume();
    process.stdout.write(`\r\x1b[2K${prompt}`);

    const cleanup = () => process.stdin.removeListener('keypress', onKey);
    const onKey = (str, key) => {
      if (!key) return;
      if (key.ctrl && key.name === 'c') {
        cleanup();
        setRaw(false);
        process.stdout.write('\n');
        process.exit(0);
      }
      if (key.name === 'return' || key.name === 'enter') {
        cleanup();
        process.stdout.write('\n');
        resolve(val.trim() === '' ? dflt : val.trim());
        return;
      }
      if (key.name === 'backspace') {
        if (val.length > 0) {
          val = val.slice(0, -1);
          draw();
        }
        return;
      }
      if (key.name === 'escape') {
        cleanup();
        process.stdout.write('\n');
        resolve(dflt);
        return;
      }
      // 可打印字符才收（过滤方向键、Tab、各种控制键）
      if (typeof str === 'string' && str >= ' ' && !key.ctrl && !key.meta && key.name !== 'tab') {
        val += str;
        draw();
      }
    };
    process.stdin.on('keypress', onKey);
  });
}

const ask = (label, dflt = '') =>
  readLine(dflt === '' ? `${label}: ` : `${label} ${D}[${dflt}]${X}: `, dflt);

const askSecret = (label, dflt = '') =>
  readLine(dflt === '' ? `${label}: ` : `${label} ${D}[${dflt}]${X}: `, dflt, true);

/** 等任意键（在 raw 模式下）。 */
function waitKey(msg = '按任意键返回') {
  return new Promise((resolve) => {
    setRaw(true);
    // ⚠️ readline 的 close() 会把 stdin 暂停；不 resume 的话这里的 'keypress' 再也收不到数据。
    process.stdin.resume();
    process.stdout.write(`\n${D}${msg}…${X}`);
    const onKey = () => {
      process.stdin.removeListener('keypress', onKey);
      process.stdout.write('\n');
      resolve();
    };
    process.stdin.on('keypress', onKey);
  });
}

/**
 * 方向键菜单。返回被选中项的 `value`；Esc/←/q 返回 null；Ctrl-C 返回 `EXIT`。
 *
 * `items` 里的 `null` 是分隔空行，会被跳过不参与选中。
 */
const EXIT = Symbol('exit');

function select(items, crumb) {
  return new Promise((resolve) => {
    let idx = 0;
    while (idx < items.length && items[idx] === null) idx += 1;

    const draw = () => {
      clearScreen();
      const out = [];
      out.push(`${B}${C}music-robot${X} ${D}· 交互式 API 客户端${X}`);
      out.push(`${D}${crumb}${X}`);
      out.push('');
      items.forEach((it, i) => {
        if (it === null) {
          out.push('');
          return;
        }
        const on = i === idx;
        const hint = it.hint ? `  ${D}${it.hint}${X}` : '';
        out.push(on ? `  ${G}▸ ${B}${it.label}${X}${hint}` : `    ${it.label}${hint}`);
      });
      out.push('');
      out.push(`${D}↑↓ 移动 · Enter 选择 · Esc/← 返回 · q 退出${X}`);
      process.stdout.write(out.join('\n'));
    };

    const step = (delta) => {
      do {
        idx = (idx + delta + items.length) % items.length;
      } while (items[idx] === null);
      draw();
    };

    const finish = (v) => {
      process.stdin.removeListener('keypress', onKey);
      clearScreen();
      resolve(v);
    };

    const onKey = (str, key) => {
      if (!key) return;
      if (key.ctrl && key.name === 'c') return finish(EXIT);
      if (key.name === 'up' || key.name === 'k') return step(-1);
      if (key.name === 'down' || key.name === 'j') return step(1);
      if (key.name === 'return' || key.name === 'enter') return finish(items[idx].value);
      if (key.name === 'escape' || key.name === 'left' || key.name === 'q') return finish(null);
      return undefined;
    };

    setRaw(true);
    process.stdin.resume(); // 同上：ask() 关掉 readline 后 stdin 是暂停的
    process.stdin.on('keypress', onKey);
    draw();
  });
}

// ───────────────────────────── 会话状态 ─────────────────────────────
const state = {
  base: 'http://127.0.0.1:8080',
  token: null,
  username: null,
  role: null,
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
      throw new Error(`连不上 ${state.base} —— 服务起了吗？（${e.cause?.code ?? e.message}）`);
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

// ───────────────────────────── 输出 ─────────────────────────────
const color = (s) => (s < 300 ? G : s < 500 ? Y : R);

function head(method, p) {
  console.log(`${D}→ ${method} ${p}${X}`);
}

function body(st, parsed, expect) {
  const tag = expect ? `  ${D}（期望 ${expect}）${X}` : '';
  console.log(`${color(st)}HTTP ${st}${X}${tag}`);
  if (parsed !== null && parsed !== undefined) console.log(JSON.stringify(parsed, null, 2));
}

async function call(method, p, payload = null, headers = null, expect = null) {
  head(method, p);
  const [st, hd, buf] = await api.req(method, p, payload, headers);
  let parsed = null;
  try {
    parsed = JSON.parse(new TextDecoder().decode(buf));
  } catch {
    parsed = null;
  }
  if (parsed !== null) body(st, parsed, expect);
  else console.log(`${color(st)}HTTP ${st}${X}  ${buf.length} 字节  ${hd['content-type'] ?? ''}`);
  return [st, hd, parsed];
}

const needLogin = () => {
  if (!state.token) {
    console.log(`${Y}先到「会话 › 登录」。${X}`);
    return false;
  }
  return true;
};

// ───────────────────────────── 动作 ─────────────────────────────

async function actHealth() {
  await call('GET', '/healthz', null, null, 200);
}

async function actRegister() {
  console.log(`${D}注册是**初始化引导**：只有库里一个用户都没有时才可用，之后一律 403。${X}\n`);
  const username = await ask('用户名', 'root');
  const password = await askSecret('口令（≥8 字符）', 'password123');
  const [st, , b] = await call('POST', '/api/auth/register', { username, password }, null, 201);
  if (st === 201) console.log(`${G}✓ 建好了，角色 = ${b?.user?.role ?? '?'}。接着去「登录」。${X}`);
}

async function actLogin() {
  const username = await ask('用户名', state.username ?? 'root');
  const password = await askSecret('口令', 'password123');
  const [st, , b] = await call('POST', '/api/auth/login', { username, password }, null, 200);
  if (st === 200) {
    state.token = b.token;
    state.username = b.user?.username ?? username;
    state.role = b.user?.role ?? null;
    console.log(`${G}✓ 已登录：${state.username}（角色 ${state.role}）${X}`);
  }
}

async function actLogout() {
  state.token = null;
  state.username = null;
  state.role = null;
  console.log(`${G}✓ 已清除本地令牌（服务端无状态，无需登出接口）${X}`);
}

async function actWhoami() {
  if (!needLogin()) return;
  await call('GET', '/api/auth/me', null, null, 200);
}

async function actAdminCreate() {
  if (!needLogin()) return;
  const username = await ask('新用户名');
  const password = await askSecret('口令（≥8 字符）', 'password123');
  const role = await ask('角色 user/admin', 'user');
  await call('POST', '/api/admin/users', { username, password, role }, null, 201);
}

async function actSwitchBase() {
  const b = await ask('服务地址', state.base);
  state.base = b.replace(/\/+$/, '');
  state.token = null;
  state.username = null;
  state.role = null;
  console.log(`${G}✓ 已切到 ${state.base}，令牌已清除${X}`);
}

async function actScan() {
  if (!needLogin()) return;
  const [st, , b] = await call('POST', '/api/scan', null, null, 202);
  if (st !== 202) return;
  const batch = b?.batch_id;
  process.stdout.write(`${D}等扫描跑完`);
  for (let i = 0; i < 100; i += 1) {
    const [, , s] = await api.json('GET', `/api/scan/${batch}`);
    if (s?.status === 'done' || s?.status === 'failed') {
      process.stdout.write(`${X}\n`);
      body(200, s);
      return;
    }
    process.stdout.write('.');
    await sleep(300);
  }
  process.stdout.write(`${X}\n${Y}等超时了，去「任务 › 任务列表」自己看。${X}\n`);
}

/** 触发一次刮削并轮询到结束。payload 为 null = pending 队列（服务端行为与以前完全一致）。 */
async function runScrape(label, payload) {
  console.log(`${D}${label}${X}\n${Y}注意：刮削会直接覆盖原文件标签（无备份、无预览）${X}\n`);
  const [st, , b] = await call('POST', '/api/scrape', payload, null, 202);
  if (st !== 202) return;
  const batch = b?.batch_id;
  if (!batch) return;
  process.stdout.write(`${D}等刮削跑完`);
  for (let i = 0; i < 240; i += 1) {
    const [, , snap] = await api.json('GET', `/api/scrape/${batch}`);
    if (snap?.status === 'done' || snap?.status === 'failed') {
      process.stdout.write(`${X}\n`);
      body(200, snap);
      return;
    }
    process.stdout.write('.');
    await sleep(500);
  }
  process.stdout.write(`${X}\n${Y}等超时了，去「任务 › 任务列表」看看。${X}\n`);
}

async function actScrape() {
  if (!needLogin()) return;
  await runScrape('队列 = scrape_status 还是 pending 的歌；插件按文件名升序尝试、命中即停', null);
}

async function actScrapeFailed() {
  if (!needLogin()) return;
  await runScrape('只重刮 scrape_status = failed 的歌（失败不自动重试，得手动捞）', {
    mode: 'failed',
  });
}

async function actScrapeSong() {
  if (!needLogin()) return;
  const raw = await ask('要重刮的歌曲 id', state.lastSongId ?? '');
  const id = Number(raw);
  if (!Number.isInteger(id) || id <= 0) {
    console.log(`${Y}id 得是正整数，收到「${raw}」${X}`);
    return;
  }
  // 关键：已 done 的歌 pending 队列永远取不到，只有这条路能重新刮它。
  await runScrape(`只听点名的这一首（id=${id}；已 done 或 failed 都能重刮）`, {
    song_ids: [id],
  });
}

async function actLibrary() {
  if (!needLogin()) return;
  const page = await ask('页码', '1');
  const size = await ask('每页', '10');
  const [st, , b] = await call('GET', `/api/library?page=${page}&page_size=${size}`, null, null, 200);
  if (st === 200 && b?.items?.length) {
    console.log();
    for (const it of b.items) {
      console.log(
        `  ${G}${String(it.id).padStart(4)}${X}  ${(it.title ?? '?').slice(0, 34).padEnd(34)}  ${(it.artists ?? '').slice(0, 18).padEnd(18)}  ${it.format ?? ''}`,
      );
    }
    state.lastSongId = String(b.items[0].id);
    console.log(`${D}（首个 id=${state.lastSongId} 已记为默认）${X}`);
  }
}

const hitList = (st, b) => {
  if (st === 200 && b?.items?.length) {
    for (const it of b.items) {
      console.log(`  ${G}${String(it.id).padStart(4)}${X}  ${(it.title ?? '?').slice(0, 40)}  ${D}${it.artists ?? ''}${X}`);
    }
    state.lastSongId = String(b.items[0].id);
  }
};

async function actSearch() {
  if (!needLogin()) return;
  const q = await ask('关键词', '白');
  const [st, , b] = await call('GET', `/api/search?q=${encodeURIComponent(q)}`, null, null, 200);
  hitList(st, b);
}

async function actSongDetail() {
  if (!needLogin()) return;
  const id = await ask('歌曲 id', state.lastSongId ?? '');
  if (!id) return;
  state.lastSongId = id;
  await call('GET', `/api/songs/${id}`, null, null, 200);
}

async function actPlay() {
  if (!needLogin()) return;
  const id = await ask('歌曲 id', state.lastSongId ?? '');
  if (!id) return;
  state.lastSongId = id;

  head('GET', `/api/stream/${id}`);
  let [st, hd, buf] = await api.req('GET', `/api/stream/${id}`);
  const full = buf.length;
  console.log(
    `${color(st)}HTTP ${st}${X}  ${D}（期望 200）${X}  整文件 ${full} 字节  ${hd['content-type']}  accept-ranges=${hd['accept-ranges']}`,
  );

  head('GET', `/api/stream/${id}   Range: bytes=0-99`);
  [st, hd] = await api.req('GET', `/api/stream/${id}`, null, { Range: 'bytes=0-99' });
  console.log(`${color(st)}HTTP ${st}${X}  ${D}（期望 206）${X}  ${hd['content-range']}`);

  head('GET', `/api/stream/${id}   Range: bytes=${full}-`);
  [st] = await api.req('GET', `/api/stream/${id}`, null, { Range: `bytes=${full}-` });
  console.log(`${color(st)}HTTP ${st}${X}  ${D}（期望 416）${X}`);
}

async function actCover() {
  if (!needLogin()) return;
  const id = await ask('歌曲 id', state.lastSongId ?? '');
  if (!id) return;
  head('GET', `/api/songs/${id}/cover`);
  const [st, hd, buf] = await api.req('GET', `/api/songs/${id}/cover`);
  console.log(
    `${color(st)}HTTP ${st}${X}  ${D}（期望 200；没封面的歌是 404）${X}  ${buf.length} 字节  ${hd['content-type'] ?? ''}`,
  );
  if (st === 200 && buf.length > 0) {
    const out = path.join(os.tmpdir(), `mr-cover-${id}.jpg`);
    fs.writeFileSync(out, buf);
    console.log(`${G}✓ 存到 ${out}${X}`);
  }
}

async function actTranscode() {
  if (!needLogin()) return;
  const id = await ask('歌曲 id', state.lastSongId ?? '');
  if (!id) return;
  head('GET', `/api/stream/${id}?format=mp3`);
  const t0 = Date.now();
  const [st, hd, buf] = await api.req('GET', `/api/stream/${id}?format=mp3`);
  console.log(
    `${color(st)}HTTP ${st}${X}  ${D}（期望 200；没 ffmpeg 会 503）${X}  ${buf.length} 字节  ${hd['content-type'] ?? ''}  ${Date.now() - t0}ms`,
  );
  console.log(`${D}再点一次通常更快 —— 命中磁盘缓存就不调 ffmpeg${X}`);
}

async function actJobs() {
  if (!needLogin()) return;
  await call('GET', '/api/jobs', null, null, 200);
}

async function actPlaylistList() {
  if (!needLogin()) return;
  const [st, , b] = await call('GET', '/api/playlists', null, null, 200);
  if (st === 200 && b?.items?.length) {
    for (const p of b.items) {
      console.log(`  ${G}${String(p.id).padStart(4)}${X}  ${p.name}  ${p.is_public ? '公开' : '私有'}`);
    }
    state.lastPlaylistId = String(b.items[0].id);
    console.log(`${D}（首个 id=${state.lastPlaylistId} 已记为默认）${X}`);
  }
}

async function actPlaylistCreate() {
  if (!needLogin()) return;
  const name = await ask('歌单名', '测试歌单');
  const isPublic = (await ask('公开？y/N', 'n')).toLowerCase().startsWith('y');
  const [st, , b] = await call('POST', '/api/playlists', { name, is_public: isPublic }, null, 201);
  if (st === 201 && b?.playlist?.id) {
    state.lastPlaylistId = String(b.playlist.id);
    console.log(`${D}（已记为默认歌单）${X}`);
  }
}

async function actPlaylistAdd() {
  if (!needLogin()) return;
  const pl = await ask('歌单 id', state.lastPlaylistId ?? '');
  const song = await ask('歌曲 id', state.lastSongId ?? '');
  if (!pl || !song) return;
  state.lastPlaylistId = pl;
  await call('POST', `/api/playlists/${pl}/items`, { song_id: Number(song) }, null, '201 首次 / 200 重复');
}

async function actPlaylistDetail() {
  if (!needLogin()) return;
  const pl = await ask('歌单 id', state.lastPlaylistId ?? '');
  if (!pl) return;
  state.lastPlaylistId = pl;
  await call('GET', `/api/playlists/${pl}`, null, null, 200);
}

async function actPlaylistDelete() {
  if (!needLogin()) return;
  const pl = await ask('歌单 id', state.lastPlaylistId ?? '');
  if (!pl) return;
  await call('DELETE', `/api/playlists/${pl}`, null, null, 200);
}

async function actHistory() {
  if (!needLogin()) return;
  const id = await ask('要记录的歌曲 id（留空 = 只看历史）', '');
  if (id) await call('POST', '/api/history', { song_id: Number(id), duration_listened_ms: 30000 }, null, 201);
  await call('GET', '/api/history?limit=10', null, null, 200);
}

async function actFavorites() {
  if (!needLogin()) return;
  const id = await ask('歌曲 id（留空 = 只看收藏列表）', '');
  if (id) {
    await call('POST', `/api/favorites/${id}`, null, null, '201 首次 / 200 重复');
    state.lastSongId = id;
  }
  await call('GET', '/api/favorites', null, null, 200);
}

async function actSettings() {
  if (!needLogin()) return;
  await call('GET', '/api/settings', null, null, 200);
  const key = await ask('再写一个键（留空跳过）', '');
  if (!key) return;
  const val = await ask('值（写 null 即删除该键）', '42000');
  await call('PUT', '/api/settings', { [key]: val === 'null' ? null : val }, null, 200);
}

async function actRequestSubmit() {
  if (!needLogin()) return;
  const title = await ask('歌名', 'Numb');
  const artist = await ask('歌手', 'Linkin Park');
  const [st, , b] = await call('POST', '/api/requests', { title, artist }, null, '201 首次 / 200 合并');
  if (b?.request?.id) state.lastRequestId = String(b.request.id);
  void st;
}

async function actRequestList() {
  if (!needLogin()) return;
  await call('GET', '/api/requests', null, null, 200);
}

async function actRequestPatch() {
  if (!needLogin()) return;
  const rid = await ask('请求 id', state.lastRequestId ?? '');
  if (!rid) return;
  state.lastRequestId = rid;
  const status = await ask('新状态 pending/processing/done/rejected', 'processing');
  await call('PATCH', `/api/requests/${rid}`, { status }, null, 200);
}

async function actRequestFetch() {
  if (!needLogin()) return;
  const rid = await ask('请求 id', state.lastRequestId ?? '');
  if (!rid) return;
  console.log(`${D}⚠️ 这条恒 503：provider（下载）插件 kind 还没实现 —— 已知缺口，不是 bug${X}\n`);
  await call('POST', `/api/requests/${rid}/fetch`, null, null, 503);
}

// ───────────────────────────── 菜单树 ─────────────────────────────

const TREE = [
  {
    label: '会话',
    hint: () => (state.token ? `已登录 ${state.username}（${state.role}）` : '未登录'),
    children: [
      { label: '健康检查 /healthz', run: actHealth },
      { label: '注册（仅初始化引导）', run: actRegister },
      { label: '登录', run: actLogin },
      { label: '当前用户 /api/auth/me', run: actWhoami },
      { label: '管理员建号', run: actAdminCreate },
      { label: '清除本地令牌', run: actLogout },
      { label: '切换服务地址', hint: () => state.base, run: actSwitchBase },
    ],
  },
  {
    label: '曲库',
    children: [
      { label: '扫描入库', run: actScan },
      { label: '刮削（批量补标签）', run: actScrape },
      { label: '重刮失败项', run: actScrapeFailed },
      { label: '重刮单曲（按 id）', run: actScrapeSong },
      { label: '曲库列表', run: actLibrary },
      { label: '搜索', run: actSearch },
      { label: '歌曲详情', run: actSongDetail },
      { label: '播放 / Range 探测', hint: () => (state.lastSongId ? `id=${state.lastSongId}` : ''), run: actPlay },
      { label: '取封面（存文件）', hint: () => (state.lastSongId ? `id=${state.lastSongId}` : ''), run: actCover },
      { label: '转码为 mp3', hint: () => (state.lastSongId ? `id=${state.lastSongId}` : ''), run: actTranscode },
    ],
  },
  {
    label: '歌单',
    children: [
      { label: '歌单列表', run: actPlaylistList },
      { label: '新建歌单', run: actPlaylistCreate },
      { label: '加歌到歌单', run: actPlaylistAdd },
      { label: '歌单详情', run: actPlaylistDetail },
      { label: '删除歌单', run: actPlaylistDelete },
    ],
  },
  {
    label: '播放周边',
    children: [
      { label: '播放历史（可顺带记录）', run: actHistory },
      { label: '收藏', run: actFavorites },
      { label: '设置 / 断点续播', run: actSettings },
    ],
  },
  {
    label: '点歌请求',
    children: [
      { label: '提交点歌', run: actRequestSubmit },
      { label: '点歌列表', run: actRequestList },
      { label: '改状态（仅 admin）', run: actRequestPatch },
      { label: 'fetch（已知恒 503）', run: actRequestFetch },
    ],
  },
  {
    label: '任务',
    children: [{ label: '任务列表 /api/jobs', run: actJobs }],
  },
];

const resolveHint = (h) => (typeof h === 'function' ? h() : h);

// ───────────────────────────── 主循环 ─────────────────────────────

async function main() {
  if (argvHelp(process.argv.slice(2))) {
    console.log('用法: node scripts/api_cli.mjs [--base http://127.0.0.1:8080]');
    console.log('需要交互式终端（用方向键操作）。');
    return 0;
  }
  const bi = process.argv.indexOf('--base');
  if (bi >= 0 && process.argv[bi + 1]) state.base = process.argv[bi + 1].replace(/\/+$/, '');

  if (!TTY) {
    console.error('这个客户端需要交互式终端（方向键菜单）。');
    console.error('如果你要的是可脚本化的端到端验证，用：node scripts/api_test.mjs');
    return 2;
  }

  // 起手探一下服务在不在
  let online = false;
  try {
    const r = await fetch(`${state.base}/healthz`, { signal: AbortSignal.timeout(3000) });
    online = r.ok;
  } catch {
    online = false;
  }
  if (!online) {
    clearScreen();
    console.log(`${R}✗ 连不上 ${state.base}${X}`);
    console.log(`${D}  先起服务：./target/debug/music-robot serve --host 127.0.0.1 --port 8080${X}`);
    await waitKey('按任意键仍然进入');
  }

  for (;;) {
    const cats = TREE.map((c) => ({
      label: c.label,
      hint: resolveHint(c.hint),
      value: c,
    }));
    const crumb =
      (state.token ? `${G}${state.username}${X}` : `${Y}未登录${X}`) + `  ${D}${state.base}${X}`;
    const chosen = await select(cats, crumb);
    if (chosen === EXIT) break;
    if (chosen === null) break;

    // 二级菜单
    for (;;) {
      const items = chosen.children.map((a) => ({
        label: a.label,
        hint: resolveHint(a.hint),
        value: a,
      }));
      const act = await select(items, `${crumb}  ${D}›  ${chosen.label}${X}`);
      if (act === EXIT) {
        setRaw(false);
        console.log(`${D}再见。${X}`);
        return 0;
      }
      if (act === null) break;

      clearScreen();
      console.log(`${B}${chosen.label} › ${act.label}${X}\n`);
      try {
        await act.run();
      } catch (e) {
        console.log(`${R}✗ ${e.message}${X}`);
      }
      await waitKey();
    }
  }

  setRaw(false);
  console.log(`${D}再见。${X}`);
  return 0;
}

function argvHelp(argv) {
  return argv.includes('-h') || argv.includes('--help');
}

try {
  // stdin 结束（Ctrl-D / 上游关掉）时菜单的 Promise 永远不会 settle，
  // 顶层 await 会一直悬着并打印 "unsettled top-level await"。直接干净退出。
  process.stdin.on('end', () => {
    setRaw(false);
    process.stdout.write(`\n${D}输入结束，退出。${X}\n`);
    process.exit(0);
  });
  process.exit(await main());
} finally {
  setRaw(false);
}
