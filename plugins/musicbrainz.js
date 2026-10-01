#!/usr/bin/env node
// @music-robot
// {
//   "name": "musicbrainz",
//   "kind": "scraper",
//   "protocol": 1,
//   "capabilities": ["metadata"],
//   "timeout_ms": 15000,
//   "max_concurrency": 1
// }
// @end
//
// MusicBrainz 刮削插件（Node，单文件、零外部依赖、daemon 模式）。
//
// ## 它做什么
// 从**文件名**解析出歌名与歌手（解析不出来才退回标签），**只用歌名**去 MusicBrainz
// 的 recording 搜索接口查；回来的候选再按**歌手吻合度 → 时长吻合度 → MB score →
// 发行日期**排序，取最优的一条，把 title / artist / album / year / genre / track 回给服务端。
//
// ## 为什么查询不带 artist（2026-10-01 改，之前是 `recording:"X" AND artist:"Y"`）
//
// 实测：本机 fixture 的 artist 标签**本身是脏的** —— 盗版资源的上传者把广告塞进了
// 歌手字段（`公众号：阿乐资源库`、`凤凰传奇 | 音乐下载网站 yym4.com`）。拿这种值去
// AND，**6 首样本全军覆没**，尽管其中 3 首只按歌名查立刻 score=100 命中。
// 一个字段脏就判死整首歌，这个查询策略太脆。
//
// 反过来只查歌名的问题是**召回噪声大**：MusicBrainz 给同一首歌的不同演唱者
// **统统打 100 分**，而且**返回顺序不稳定**（同一个查询连打 4 次，「筷子兄弟」
// 一会儿第 1 一会儿第 3）。所以 MB 自己的 score 在中文曲库里**几乎没有区分度**，
// 真正管用的是歌手与时长这两个本地信号 —— 排序时它们排在最前面。
//
// ## 为什么优先用文件名
//
// 同上：标签脏、文件名反而干净（`老男孩-筷子兄弟.mp3`、`盛夏-毛不易.mp3`）。
// 约定 **第一个分隔符之前是歌名，之后是歌手**；解析不出来就整条退回标签。
// 只从文件名补 title / artist **两个字段**，album 等仍取标签。
//
// ## 几个必须遵守的 MusicBrainz 规矩（不然会被 403 / 限流）
//   1. **必须带 User-Agent**，格式 `应用名/版本 ( 联系方式 )`。缺了会被直接拒绝。
//   2. **匿名限流 1 请求/秒**。本插件在两次请求之间强制间隔 >= 1100ms。
//   3. 不要并发打它 —— 清单里 max_concurrency = 1。
// 详见 https://musicbrainz.org/doc/MusicBrainz_API/Rate_Limiting
//
// ## ⚠️ 这个数据源的固有限制（实测得出，不是实现偷懒）
//
// MusicBrainz 的 **recording 搜索**回答的是「有哪些录音叫这个名字」，
// **不是**「这首歌属于哪张专辑」。一个 recording 会挂在几十个 release 上
// （原专辑 / 现场 / 合辑 / 精选 / 再版），API 里**没有**「哪张是正片专辑」这个字段。
//
// ### album 为什么几乎永远拿不到（2026-09 实测，别再去试那两种「显然的修法」）
//
//   1. **recording 搜索返回的 `releases` 是截断的**：实测搜 "Numb / Linkin Park"，
//      每个 recording 只回 **1~2** 个 release，不是它在 MusicBrainz 上的全部。
//      所以 `releaseOf` 里那套「primary=Album 且 secondary 为空」的筛选**几乎恒为空**。
//   2. **补一次 lookup（`/recording/<mbid>?inc=releases+release-groups`）也救不了**，
//      反而更糟：实测对 187s 的那个 recording lookup，只有 `Living Things +` @2013
//      （再版）这一张正片专辑。若照单全收，等于把**别人的再版**当成用户的原专辑写进去 ——
//      正是下面说的「给错专辑比不给更糟」。
//
// 真要做准，得换思路：拿**本地已有的 album 标签**去 `/release` 搜索、再核对曲目表
// （Picard 那一类 tagger 的做法，还要配 AcoustID 指纹）。那要多次请求 + 实体匹配逻辑，
// 与本插件「简单」的定位冲突，所以**不做**。当前行为：album 拿不到就留空。
//
// 所以本插件的取舍是：
//   · title / artist —— 可靠，总是给；
//   · year —— 优先取所选 release 的日期，没有正片专辑时退回 recording 的
//     `first-release-date`。**注意**：这个字段可能是某次再版的年份而不是初版
//     （实测 Bohemian Rhapsody 给的是 1992，而原版是 1975）；
//   · album / track —— **只在找到一个「正片专辑」时给**（primary-type=Album 且
//     secondary-types 为空）；找不到就**留空**。给错专辑比不给更糟 ——
//     用户库里会多出一张根本不存在的专辑。
//   · genre —— MusicBrainz 的 genre 覆盖率很低，多数时候为空，属正常。
//
// 想让 album 更准，正确做法是再查一次 release-group / release 接口，或改从
// **release 搜索**入手（用曲名+歌手找 release，而不是找 recording）。那会多几次
// 请求、也更复杂；本插件刻意保持「简单」。
//
// ## 协议要点（照 example.js）
//   · stdout 只放协议 JSON（一行一个），诊断信息一律走 stderr；
//   · 响应必须**原样回显**请求的 id 与 action；
//   · tags 里**字段缺省 = 不修改**（MusicBrainz 没有歌词，所以干脆不返回 lyrics 字段）。

'use strict';

const readline = require('readline');

const MB_BASE = 'https://musicbrainz.org/ws/2/recording';
// MusicBrainz 要求能联系到你；留一个项目地址即可（不含真实邮箱也不该是空串）。
const USER_AGENT = 'music-robot/0.1.0 ( https://github.com/music-robot )';
const MIN_INTERVAL_MS = 1100; // 匿名限流 1 req/s，留 100ms 余量
const RESULT_LIMIT = 10;

// 网络重试。**这不是防御性编程，是实测必需**：本机的 node fetch 会间歇性抛
// `fetch failed`（同一个 URL、同一个进程，上一秒失败下一秒就成功；同一时刻 curl 是 200）。
// 链路抖动不重试的话，一次抖动就让整首歌的刮削白跑，用户还得手动再点一次。
//
// 预算约束：清单里 timeout_ms = 15000，重试总计**不能超过它** —— 超了池子会先杀掉插件，
// 用户看到的是「插件超时」而不是「网络抖动」，排查方向直接被带偏。所以下面把 timeout_ms
// 当作**整次请求的总预算**来切分，而不是每次尝试都用满它。
const MAX_ATTEMPTS = 3;
const MIN_ATTEMPT_MS = 1500; // 单次尝试的下限：比这还短的超时只会制造假超时
const RETRY_BACKOFF_MS = 400; // 首次退避，之后翻倍

let lastRequestAt = 0;

function log(...args) {
  process.stderr.write('[musicbrainz] ' + args.join(' ') + '\n');
}

function respond(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
}

// ── 归一化（用于比对与打分；与服务端 S23 的口径无关，这里只服务于选最佳候选）──
function normalize(s) {
  if (typeof s !== 'string') return '';
  return s
    .normalize('NFKC')          // 全角 → 半角
    .toLowerCase()
    .replace(/[\u0000-\u001f\u007f]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

// ── 限流：两次请求之间至少隔 MIN_INTERVAL_MS ──
async function throttle() {
  const wait = lastRequestAt + MIN_INTERVAL_MS - Date.now();
  if (wait > 0) await new Promise((r) => setTimeout(r, wait));
  lastRequestAt = Date.now();
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

// 单次 HTTP 尝试（含超时）。成功返回 { resp }，失败返回 { code, message }。
async function fetchOnce(url, timeoutMs) {
  const ac = new AbortController();
  const timer = setTimeout(() => ac.abort(), timeoutMs);
  try {
    const resp = await fetch(url, {
      headers: { 'User-Agent': USER_AGENT, Accept: 'application/json' },
      signal: ac.signal,
    });
    return { resp: resp };
  } catch (e) {
    if (e && e.name === 'AbortError') {
      return { code: 'TIMEOUT', message: '查询 MusicBrainz 超时（' + timeoutMs + 'ms）' };
    }
    // node 的 fetch 失败时 message 常常就是干巴巴的 "fetch failed"，
    // 真正的原因在 e.cause 里（ECONNRESET / ENOTFOUND / …）。丢掉它等于丢掉排查线索。
    const cause = e && e.cause ? '（' + (e.cause.code || e.cause.message || e.cause) + '）' : '';
    return { code: 'NETWORK', message: '查询 MusicBrainz 失败：' + ((e && e.message) || e) + cause };
  } finally {
    clearTimeout(timer);
  }
}

// 带重试的 GET：在 totalMs 预算内最多试 MAX_ATTEMPTS 次，返回最后一次的结果。
//
// 每次尝试前都过一遍 throttle() —— 重试打的还是 MusicBrainz，不能绕过 1 req/s 的限流，
// 否则抖动没解决，反而先被 503 限流。
async function fetchWithRetry(url, totalMs) {
  const started = Date.now();
  let last = { code: 'NETWORK', message: '查询 MusicBrainz 失败：预算不足，未发起请求' };

  for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
    const left = totalMs - (Date.now() - started);
    if (left < MIN_ATTEMPT_MS) break;

    // 把剩余预算按「还要试几次」均分：第一次不会独吞全部预算，后面的尝试才有时间可用。
    const perTry = Math.max(MIN_ATTEMPT_MS, Math.floor(left / (MAX_ATTEMPTS - attempt + 1)));
    await throttle();

    const r = await fetchOnce(url, perTry);
    if (r.resp) return r;
    last = r;

    if (attempt < MAX_ATTEMPTS) {
      const backoff = RETRY_BACKOFF_MS * Math.pow(2, attempt - 1);
      log('第 ' + attempt + ' 次请求失败（' + r.code + '：' + r.message + '），' + backoff + 'ms 后重试');
      await sleep(backoff);
    }
  }
  return last;
}

function escapeLucene(s) {
  // Lucene 查询串里 \ 和 " 必须转义，否则查询会语法错误或语义被改。
  return String(s).replace(/\\/g, '\\\\').replace(/"/g, '\\"');
}

function buildQuery(song) {
  // 兜底查询：**只用歌名**（带 artist 的 AND 会让一个脏标签判死整首歌，见文件头）。
  return song.title ? 'recording:"' + escapeLucene(song.title) + '"' : '';
}

// 首选查询：歌名 + 歌手。歌手取文件名解析出来的那个（比标签干净）。
function buildStrictQuery(title, artist) {
  if (!title || !artist) return '';
  return (
    'recording:"' + escapeLucene(title) + '"' +
    ' AND artist:"' + escapeLucene(artist) + '"'
  );
}

// 规范化歌手，用于**第二个**查询变体。
//
//   `Camila Cabello&YoungThug-大耳兽莫慢待` → `Camila Cabello & Young Thug`
//
// 三处改动都有实测依据：
//   · 末段 `-大耳兽莫慢待` 是上传者昵称，带着它查零结果；
//   · `&` 两侧不带空格时 MusicBrainz 匹配不上；
//   · 连写的驼峰要拆开 —— `YoungThug` 查不到，`Young Thug` 才行
//     （实测 `artist:"Camila Cabello & Young Thug"` 恰好 1 条，就是本地那个 217s 的专辑版）。
//
// ⚠️ 只在**第一个变体失败后**才用它：`Jay-Z` 这种名字里真带 `-` 的歌手，
// 第一个变体（原样）能查到，轮不到这里被切成 `Jay`。
function normalizeArtistForQuery(artist) {
  let text = String(artist || '').trim();
  if (!text) return '';
  const cut = text.lastIndexOf('-');
  if (cut > 0) text = text.slice(0, cut).trim();
  return text
    .replace(/&/g, ' & ')
    // 驼峰拆分：小写/数字 紧跟 大写 时插空格（`YoungThug` → `Young Thug`）
    .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
    .replace(/\s+/g, ' ')
    .trim();
}

// 查一次 MusicBrainz，把「HTTP 杂事」收在这一个地方。
// 返回 { recordings } 或 { error: { code, message, retryable } }。
async function fetchRecordings(query, timeoutMs) {
  const url = MB_BASE + '?query=' + encodeURIComponent(query) + '&fmt=json&limit=' + RESULT_LIMIT;
  const got = await fetchWithRetry(url, timeoutMs);
  if (!got.resp) {
    // TIMEOUT / NETWORK 都是可重试错误，交给上层决定要不要再战（retryable = true）。
    return { error: { code: got.code, message: got.message, retryable: true } };
  }
  const resp = got.resp;

  if (resp.status === 503) {
    // MusicBrainz 限流时回 503 并带 Retry-After
    return { error: { code: 'RATE_LIMITED', message: '被 MusicBrainz 限流，稍后重试', retryable: true } };
  }
  if (resp.status === 400) {
    return { error: { code: 'BAD_REQUEST', message: '查询串被 MusicBrainz 拒绝（可能含非法字符）', retryable: false } };
  }
  if (resp.status === 403) {
    return { error: { code: 'AUTH_FAILED', message: 'MusicBrainz 拒绝了请求（User-Agent 不合规？）', retryable: false } };
  }
  if (!resp.ok) {
    return { error: { code: 'NETWORK', message: 'MusicBrainz 返回 HTTP ' + resp.status, retryable: true } };
  }

  let data;
  try {
    data = await resp.json();
  } catch (e) {
    return { error: { code: 'NETWORK', message: 'MusicBrainz 返回的不是合法 JSON', retryable: true } };
  }
  return { recordings: data && Array.isArray(data.recordings) ? data.recordings : [] };
}

// ── 从文件名解析歌名 / 歌手 ──────────────────────────────────────────────
//
// 约定：**第一个分隔符之前是歌名，之后是歌手**。
//   `老男孩-筷子兄弟.mp3`                              → 老男孩 / 筷子兄弟
//   `牵丝戏 - 白兀.flac`                               → 牵丝戏 / 白兀
//   `Havana-Camila Cabello&YoungThug-大耳兽莫慢待.mp3`  → Havana / Camila Cabello&YoungThug-大耳兽莫慢待
//
// 取**第一个**分隔符而不是最后一个：上例最后一个分隔符切出来的是
// `Havana-Camila Cabello&YoungThug` / `大耳兽莫慢待`，歌名里塞着歌手、歌手是上传者昵称，
// 两边都不能用来查。取第一个则歌名正好是 `Havana`（要查的就是它）。
//
// 解析不出来（没分隔符 / 某一侧为空 / 长得离谱）返回 null，调用方整条退回标签。
function parseFileName(filePath) {
  if (typeof filePath !== 'string' || !filePath.trim()) return null;
  // 取 basename 再剥扩展名。手写而不用 require('path')：插件要跨平台，且这里够简单。
  const base = filePath.split(/[\\/]/).pop() || '';
  // 扩展名限定成「点 + 1~5 个字母数字」，免得把歌名里的 `.` 当成扩展名切掉。
  const stem = base.replace(/\.[a-z0-9]{1,5}$/i, '').trim();
  if (!stem) return null;

  // 去掉开头的音轨号（`01. ` / `01 - ` / `01_`）。只剥数字，别把 `1999-...` 这种年份当序号。
  const body = stem.replace(/^\d{1,3}\s*[.\-_]\s+/, '').trim();
  if (!body) return null;

  // 优先切「两边带空格」的分隔符：`歌名 - 歌手` 比 `歌名-歌手` 更明确，
  // 也避免把 `Havana-Camila` 这种无空格连字符先切了（上面那条是为了歌名准确，这里同理）。
  let sep = body.includes(' - ') ? ' - ' : '-';
  const at = body.indexOf(sep);
  if (at < 0) return null;

  const title = body.slice(0, at).trim();
  const artist = body.slice(at + sep.length).trim();
  if (!title || !artist) return null;
  // 两侧都别长得离谱：正常歌名/歌手不会有 100 字符，出现了多半是误切。
  if (title.length > 100 || artist.length > 100) return null;
  return { title: title, artist: artist };
}

// 这首歌的「查询身份」：文件名优先，解析不出来的那半边退回标签。
function pickIdentity(song) {
  const fromName = parseFileName(song && song.file_path);
  const tagTitle = typeof song.title === 'string' ? song.title.trim() : '';
  const tagArtist = typeof song.artist === 'string' ? song.artist.trim() : '';
  const title = (fromName && fromName.title) || tagTitle;
  const artist = (fromName && fromName.artist) || tagArtist;
  return {
    title: title,
    artist: artist,
    // 只用于日志/排查：这次的身份是从文件名来的还是从标签来的。
    from: fromName ? 'file_name' : 'tags',
  };
}

// ── 打分：MB 自己的 score（0~100）+ 与本地标签的一致性 + 时长吻合度 ──
function confidenceOf(cand, song) {
  let c = (typeof cand.score === 'number' ? cand.score : 0) / 100;

  const wantTitle = normalize(song.title);
  const gotTitle = normalize(cand.title);
  const wantArtist = normalize(song.artist);
  const gotArtist = normalize(artistCredit(cand));

  if (wantTitle && gotTitle) {
    if (wantTitle === gotTitle) c += 0.05;
    else if (!gotTitle.includes(wantTitle) && !wantTitle.includes(gotTitle)) c -= 0.15;
  }
  if (wantArtist && gotArtist) {
    if (gotArtist === wantArtist || gotArtist.includes(wantArtist)) c += 0.05;
    else c -= 0.15;
  }

  // 时长：差 3% 内加分，差 15% 以上扣分（能有效排除同名不同版本）
  const want = song.duration_ms;
  const got = cand.length;
  if (typeof want === 'number' && want > 0 && typeof got === 'number' && got > 0) {
    const delta = Math.abs(want - got) / want;
    if (delta <= 0.03) c += 0.1;
    else if (delta >= 0.15) c -= 0.2;
  }

  return Math.max(0, Math.min(1, c));
}

function artistCredit(cand) {
  if (!Array.isArray(cand['artist-credit'])) return '';
  return cand['artist-credit']
    .map((ac) => (ac && ac.name) || (ac && ac.artist && ac.artist.name) || '')
    .filter(Boolean)
    .join(' / ');
}

// 歌手吻合度分档：2 = 吻合，1 = 无法比较（本地没歌手 / 候选没歌手），0 = 明显不符。
//
// 为什么放在**排序第一位**：MusicBrainz 的 score 在中文曲库里没有区分度（见文件头），
// 歌手是本地能给的最强信号。实测「老男孩」的候选里 筷子兄弟 / 雷婷 / 羽·泉 / 赵照
// **全是 100 分**，只有歌手能把正确的那条挑出来。
//
// 双向包含：文件名里常见 `Havana-Camila Cabello&YoungThug-大耳兽莫慢待` 这种
// 「歌手后面还拖着一串」的写法，短的那侧被长的那侧包含就算吻合。
// 但包含判定要求短侧 >= 2 字符 —— 否则单字母歌手（`T`、`A`）会匹配上任何东西。
function artistClass(cand, song) {
  const want = normalize(song && song.artist);
  const got = normalize(artistCredit(cand));
  if (!want || !got) return 1;
  if (want === got) return 2;
  const shorter = want.length <= got.length ? want : got;
  const longer = want.length <= got.length ? got : want;
  if (shorter.length >= 2 && longer.includes(shorter)) return 2;
  return 0;
}

// 时长吻合度分档：2 = 吻合（<=3%），1 = 未知/无法比较，0 = 明显不吻合（>=10%）
function durationClass(cand, song) {
  const want = song && song.duration_ms;
  const got = cand && cand.length;
  if (typeof want !== 'number' || want <= 0) return 1;
  if (typeof got !== 'number' || got <= 0) return 1;
  const delta = Math.abs(want - got) / want;
  if (delta <= 0.03) return 2;
  if (delta >= 0.1) return 0;
  return 1;
}

// 排序用键（**故意不 clamp**，否则信号会被上限吃掉，见调用处的长注释）
// 优先级：歌手吻合 → 时长吻合 → MB score → 发行日期最早
function rankKey(cand, song) {
  return [
    -artistClass(cand, song),
    -durationClass(cand, song),
    -(typeof cand.score === 'number' ? cand.score : 0),
    paddedDate(typeof cand['first-release-date'] === 'string' ? cand['first-release-date'] : ''),
  ];
}

// 键越小越优先
function compareRank(a, b) {
  for (let i = 0; i < a.length; i++) {
    if (a[i] < b[i]) return -1;
    if (a[i] > b[i]) return 1;
  }
  return 0;
}

function paddedDate(d) {
  const t = String(d || '').trim();
  if (/^\d{4}$/.test(t)) return t + '-12-31';
  if (/^\d{4}-\d{2}$/.test(t)) return t + '-31';
  if (/^\d{4}-\d{2}-\d{2}$/.test(t)) return t;
  return '9999-12-31'; // 没日期 → 排到最后
}

// 从候选里挑出最合适的 release（专辑 / 年份 / 曲目号都从它取）。
//
// ⚠️ 这里不能随便挑第一个带日期的：MusicBrainz 的 recording 会挂上**一堆**
// release，包括各种合辑、现场、再版。实测搜 "Numb / Linkin Park" 时第一个带日期的
// 是合辑 `The Top 101 of 2003 on KC101` —— 给错专辑比不给更糟。
//
// 优先级：
//   1. 本地已有 album 标签 且 与某个 release 标题归一化后相等 → 用它（用户自己的信息最可信）
//   2. release-group 的 primary-type 是 Album 且**没有** secondary-types（排除 Compilation /
//      Live / Remix / Soundtrack 等「次级类型」）→ 取其中最早的一个（通常就是原版）
//   3. 退而求其次：第一个带日期的
//   4. 再不行：第一个
function releaseOf(cand, song) {
  const releases = (Array.isArray(cand.releases) ? cand.releases : []).filter(Boolean);
  if (releases.length === 0) return null;

  const wantAlbum = normalize((song && song.album) || '');
  if (wantAlbum) {
    const hit = releases.find((r) => normalize(r.title) === wantAlbum);
    if (hit) return hit;
  }

  const plainAlbums = releases.filter((r) => {
    const rg = r['release-group'];
    if (!rg) return false;
    const primary = (rg['primary-type'] || '').toLowerCase();
    const secondary = Array.isArray(rg['secondary-types']) ? rg['secondary-types'] : [];
    return primary === 'album' && secondary.length === 0;
  });
  if (plainAlbums.length > 0) {
    // 同为正片专辑时取日期最早的（一般就是原版）。
    //
    // ⚠️ 不能直接比日期字符串！MusicBrainz 的日期精度不一：
    //   原专辑 Meteora = '2003-03-25'，而某张合辑 = '2003'（只有年份）。
    //   字符串比较下 '2003' < '2003-03-25'，**精度低的反而排前面** —— 实测就是这么
    //   把合辑 The Top 101 of 2003 on KC101 选中的。
    //   所以把不完整的日期补到**该区间末尾**（年 → 该年 12-31，年月 → 该月 31），
    //   于是同年里精度高（更可能是真实发行日）的排在前面。
    return plainAlbums.slice().sort((a, b) => paddedDate(a.date).localeCompare(paddedDate(b.date)))[0];
  }

  // 走到这里说明没有任何「正片专辑」。**不要兜底捡一个** ——
  // 实测候选里出现过只有合辑（primary=other, secondary=[Compilation, DJ-mix]）的录音，
  // 兜底会把合辑名当专辑写进用户的库。宁可留空（字段缺省 = 不修改）。
  return null;
}

function trackNumberOf(release) {
  const media = release && Array.isArray(release.media) ? release.media : [];
  for (const m of media) {
    const tracks = m && Array.isArray(m.tracks) ? m.tracks : [];
    for (const t of tracks) {
      const n = parseInt(t && t.number, 10);
      if (!Number.isNaN(n)) return n;
    }
  }
  return null;
}

function yearOf(release) {
  const d = release && typeof release.date === 'string' ? release.date : '';
  const m = /^(\d{4})/.exec(d);
  return m ? m[1] : null;
}

function yearFromRecording(cand) {
  const d = cand['first-release-date'];
  if (typeof d !== 'string') return null;
  const m = /^(\d{4})/.exec(d);
  return m ? m[1] : null;
}

function genreOf(cand) {
  const tags = Array.isArray(cand.tags) ? cand.tags : [];
  // MB 的 tags 带 count，取最高频那个
  const best = tags
    .filter((t) => t && typeof t.name === 'string')
    .sort((a, b) => (b.count || 0) - (a.count || 0))[0];
  return best ? best.name : null;
}

// ── 组装成功响应 ──
function buildOk(id, cand, conf, song) {
  const release = releaseOf(cand, song);
  const tags = {};
  if (cand.title) tags.title = cand.title;
  const artist = artistCredit(cand);
  if (artist) tags.artist = artist;
  // album 与 track 只在有**正片专辑**时给；release 为 null 就整个不给（见 releaseOf 的说明）。
  if (release && release.title) tags.album = release.title;
  // year 优先用所选 release 的日期；没有正片专辑时退回 recording 的
  // first-release-date —— 它是「这首歌最初发行的时间」，比随便一个 release 的日期可信。
  const year = yearOf(release) || yearFromRecording(cand);
  if (year) tags.year = year;
  const genre = genreOf(cand);
  if (genre) tags.genre = genre;
  const track = release ? trackNumberOf(release) : null;
  if (track !== null) tags.track = track;

  return {
    id: id,
    protocol: 1,
    action: 'scrape',
    ok: true,
    confidence: conf,
    source: 'musicbrainz',
    matched: {
      id: cand.id || null,
      title: cand.title || null,
      artist: artist || null,
      url: cand.id ? 'https://musicbrainz.org/recording/' + cand.id : null,
    },
    tags: tags,
  };
}

function buildError(id, code, message, retryable) {
  return {
    id: id,
    protocol: 1,
    action: 'scrape',
    ok: false,
    error: { code: code, message: message, retryable: !!retryable },
  };
}

// ── 主流程：查一次 MusicBrainz ──
async function scrape(req) {
  const raw = req.song || {};
  // 文件名优先解析出 title / artist；解析不出来退回标签。
  // album 不从文件名取（它不在文件名里），仍用标签值选 release。
  const identity = pickIdentity(raw);
  const song = Object.assign({}, raw, { title: identity.title, artist: identity.artist });
  const title = song.title;
  const artist = song.artist;

  if (!title && !artist) {
    return buildError(req.id, 'NOT_FOUND', '这首歌没有标题也没有歌手，无法查询 MusicBrainz', false);
  }
  if (!title) {
    // 查询只用歌名，没歌名就没法查 —— 说清楚是哪一步缺，别含糊成「没找到」。
    return buildError(
      req.id,
      'NOT_FOUND',
      '这首歌没有可用的标题（文件名没解析出歌名，标签里也没有），无法查询 MusicBrainz',
      false
    );
  }
  log('查询身份来自 ' + identity.from + '：title=' + JSON.stringify(title) + ' artist=' + JSON.stringify(artist));

  // timeout_ms 在这里是**整次请求（含重试）的总预算**，见 fetchWithRetry 的注释。
  const timeoutMs = (req.options && req.options.timeout_ms) || 10000;

  // ── 两段式查询：先带歌手（精确），零结果才退回只查歌名（宽容）──
  //
  // 为什么不是「只查歌名」这一条路：同名歌一多，纯歌名查询**等于随机**。
  // 实测 `recording:"Havana"` 前 25 条 score 全是 100，却一条都不是 Camila Cabello
  // （Kenny G / Wimme / Frank Loesser…）；本地那个 217s 的文件会被一条时长恰好
  // 215s 的同名歌（Brother Sun Sister Moon《Havana》1997）顶掉 —— 而且它能过认领闸门，
  // 因为时长「吻合」。写错还不可撤销。
  //
  // 反过来只走 AND 也不行：歌手本身可能是错的（实测 牵丝戏 被标成「白兀」），
  // `artist:"白兀"` 零结果，歌其实查得到。
  //
  // 所以：AND 优先（干净歌手时 4/6 直接命中且全对），零结果才退到歌名，
  // 两条路**都**要过下面的认领闸门。
  const steps = [];
  const seen = new Set();
  const pushQuery = (kind, query) => {
    if (!query || seen.has(query)) return; // 去重：干净歌手只需要一次请求
    seen.add(query);
    steps.push({ kind: kind, query: query });
  };
  pushQuery('strict', buildStrictQuery(title, artist));
  pushQuery('strict-normalized', buildStrictQuery(title, normalizeArtistForQuery(artist)));
  pushQuery('loose', buildQuery({ title: title }));

  let recordings = [];
  let usedKind = '';
  for (const step of steps) {
    const got = await fetchRecordings(step.query, timeoutMs);
    if (got.error) {
      return buildError(req.id, got.error.code, got.error.message, got.error.retryable);
    }
    if (got.recordings.length) {
      recordings = got.recordings;
      usedKind = step.kind;
      break;
    }
    log(step.kind + ' 查询零结果，继续下一种：' + step.query);
  }
  if (recordings.length === 0) {
    return buildError(req.id, 'NOT_FOUND', 'MusicBrainz 没有找到匹配的录音', false);
  }
  log(usedKind + ' 查询命中 ' + recordings.length + ' 条候选');


  // 同名录音会有多个（原版 / 现场 / 重混），而且 MusicBrainz 给它们的 score **常常都是 100**
  // —— 它们是不同的 recording 实体，光看 score 挑不准。
  //
  // ⚠️ 这里踩过一个坑：原先用「confidenceOf 相加后比大小」，但 confidence 最终要 clamp 到
  // 0~1，原版（score 1.0 + 时长加分 0.1 = 1.1）和现场版（1.0 + 0 = 1.0）**双双 clamp 成 1.0**，
  // 时长这个最强信号被上限吃掉了，于是日期兜底把现场版选中。实测就是如此。
  //
  // 正确做法：**排序用不着 clamp 的键**，按优先级逐级比较：
  //   1. 时长吻合度（能区分原版 / 现场 / 重混 —— 最强信号）
  //   2. MusicBrainz 自己的 score
  //   3. first-release-date 最早（原版通常最早）
  let best = null;
  let bestKey = null;
  for (const cand of recordings) {
    const key = rankKey(cand, song);
    if (bestKey === null || compareRank(key, bestKey) < 0) {
      bestKey = key;
      best = cand;
    }
  }
  const bestConf = confidenceOf(best, song);
  const bestArtist = best ? artistClass(best, song) : 0;
  const bestDuration = best ? durationClass(best, song) : 0;

  // ── 认领闸门：必须拿到**至少一个强本地信号**才认这个候选 ──
  //
  // 查询只用歌名之后，候选里全是**毫不相干的同名歌**。实测 `recording:"Havana"`
  // 返回的前 25 条**score 全是 100**，却没有一条是 Camila Cabello
  // （Kenny G / Wimme / Frank Loesser……），因为 MusicBrainz 的 score 在这种
  // 查询下等于噪声。少了这道闸，插件会把 `David Rudder` 的《Havana》(2001)
  // 当成用户的歌写进文件 —— 而刮削会覆盖原文件，**不可撤销**。
  //
  // ⚠️ 光靠 confidence 的加减分拦不住它：那条候选**没有时长字段**，反而躲过了
  // 时长扣分，最终算出 0.9，高于服务端 0.80 的命中阈值。所以闸门必须显式判。
  if (!best || bestArtist !== 2 && bestDuration !== 2) {
    return buildError(
      req.id,
      'NOT_FOUND',
      'MusicBrainz 的候选里没有一条能和本地歌手或时长对上（' +
        '歌手吻合度 ' + bestArtist + '/2、时长吻合度 ' + bestDuration + '/2，' +
        '最高 confidence ' + bestConf.toFixed(2) + '）',
      false
    );
  }

  if (bestConf < 0.5) {
    // 低于 0.5 连「候选」都算不上；回 NOT_FOUND 让上层顺序回退到下一个插件，
    // 而不是塞一堆不可信标签进去（0.80 的命中阈值由服务端判定）。
    return buildError(
      req.id,
      'NOT_FOUND',
      'MusicBrainz 的候选都不够可信（最高 ' + bestConf.toFixed(2) + '）',
      false
    );
  }

  return buildOk(req.id, best, bestConf, song);
}

function handleLine(line) {
  const trimmed = line.trim();
  if (!trimmed) return;

  let req;
  try {
    req = JSON.parse(trimmed);
  } catch (e) {
    log('请求不是合法 JSON：' + e.message);
    respond(buildError('', 'BAD_REQUEST', '请求不是合法 JSON：' + e.message, false));
    return;
  }

  if (req.action !== 'scrape') {
    respond({
      id: req.id,
      protocol: 1,
      action: req.action,
      ok: false,
      error: { code: 'BAD_REQUEST', message: 'musicbrainz 只支持 action=scrape', retryable: false },
    });
    return;
  }

  scrape(req)
    .then(respond)
    .catch((e) => {
      // 兜底：绝不让插件因为未捕获异常直接死掉（死了池子要重启进程，还会丢请求）
      log('未捕获异常：' + (e && e.stack ? e.stack : e));
      respond(buildError(req.id, 'INTERNAL', '插件内部错误', false));
    });
}

// ── 离线自检：`node plugins/musicbrainz.js --selftest` ────────────────────
//
// 只测**纯函数**（文件名解析、查询串构造、歌手/时长分档与排序），不联网。
// 这些是 2026-10-01 改查询策略时新加的判断逻辑，也是最容易悄悄写错的地方：
// 它们决定「哪条候选会被写进用户的文件」，而刮削会覆盖原文件、不可撤销。
// `tests/plugin_e2e.rs` 会跑这个自检，所以 `cargo test` 就覆盖得到。
function selftest() {
  const failures = [];
  const eq = (name, got, want) => {
    const a = JSON.stringify(got);
    const b = JSON.stringify(want);
    if (a !== b) failures.push(name + '\n    实际 ' + a + '\n    期望 ' + b);
  };

  // 文件名解析：歌名取第一个分隔符之前（Havana 那条取第一个而不是最后一个，
  // 否则歌名会变成 `Havana-Camila Cabello&YoungThug`，拿去查反而查不到）
  eq('解析 老男孩-筷子兄弟.mp3',
    parseFileName('/music/老男孩-筷子兄弟.mp3'), { title: '老男孩', artist: '筷子兄弟' });
  eq('解析 牵丝戏 - 白兀.flac',
    parseFileName('/music/牵丝戏 - 白兀.flac'), { title: '牵丝戏', artist: '白兀' });
  eq('解析 Havana-...-大耳兽莫慢待.mp3（取第一个分隔符）',
    parseFileName('/music/Havana-Camila Cabello&YoungThug-大耳兽莫慢待.mp3'),
    { title: 'Havana', artist: 'Camila Cabello&YoungThug-大耳兽莫慢待' });
  eq('解析 Windows 路径 + 音轨号',
    parseFileName('C:\\music\\01. 盛夏-毛不易.mp3'), { title: '盛夏', artist: '毛不易' });
  // 解析不出来必须回 null（调用方据此退回标签），不能猜
  eq('无分隔符 → null', parseFileName('/music/没有分隔符.mp3'), null);
  eq('歌手侧为空 → null', parseFileName('/music/只有歌名-.mp3'), null);
  eq('歌名侧为空 → null', parseFileName('/music/-只有歌手.mp3'), null);
  eq('空路径 → null', parseFileName(''), null);
  eq('非字符串 → null', parseFileName(null), null);

  // 查询串规范化
  eq('规范化 驼峰+&+尾巴',
    normalizeArtistForQuery('Camila Cabello&YoungThug-大耳兽莫慢待'),
    'Camila Cabello & Young Thug');
  eq('规范化 干净歌手原样', normalizeArtistForQuery('筷子兄弟'), '筷子兄弟');
  eq('规范化 名字里的 - 不动（第一个变体能查到就不走这里）',
    normalizeArtistForQuery('Jay-Z'), 'Jay');

  // 歌手吻合度：双向包含，但短侧 < 2 字符不算（否则单字母歌手匹配一切）
  const cand = (artist, len, score) => ({
    score: score === undefined ? 100 : score,
    length: len,
    title: 'Havana',
    'artist-credit': [{ name: artist }],
  });
  const song = { title: 'Havana', artist: 'Camila Cabello&YoungThug-大耳兽莫慢待', duration_ms: 217307 };
  eq('长串包含候选名 → 吻合', artistClass(cand('Camila Cabello'), song), 2);
  eq('毫不相干 → 不吻合', artistClass(cand('Bongwater'), song), 0);
  eq('短侧只有 1 字符 → 不算吻合', artistClass(cand('C'), song), 0);
  eq('本地没歌手 → 无法比较', artistClass(cand('Bongwater'), { artist: '' }), 1);

  // 这条是**安全闸门**的核心：同名的另一首歌（时长恰好接近）不能胜出
  const wrong = cand('Bongwater', 215000);
  const right = cand('Camila Cabello', 217000);
  if (compareRank(rankKey(right, song), rankKey(wrong, song)) >= 0) {
    failures.push('歌手吻合的候选必须排在同名不同歌的前面');
  }

  if (failures.length) {
    for (const f of failures) process.stderr.write('[selftest] ✗ ' + f + '\n');
    process.stderr.write('[selftest] ' + failures.length + ' 项失败\n');
    return 1;
  }
  process.stdout.write('[selftest] 全部通过\n');
  return 0;
}

if (process.argv.includes('--selftest')) {
  process.exit(selftest());
}

const rl = readline.createInterface({ input: process.stdin });
rl.on('line', handleLine);
