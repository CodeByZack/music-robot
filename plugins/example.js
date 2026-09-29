#!/usr/bin/env node
// @music-robot
// {
//   "name": "example-js",
//   "kind": "scraper",
//   "protocol": 1,
//   "capabilities": ["metadata", "lyrics"],
//   "timeout_ms": 10000,
//   "max_concurrency": 1
// }
// @end
//
// 示例插件（Node）—— 单文件、零外部依赖、daemon 模式。
//
// 协议：stdin 一行一个 JSON 请求，stdout 一行一个 JSON 响应。
//   1. 响应必须**原样回显**请求的 id 与 action —— 上层只靠 action 判别响应类型，
//      不看字段有没有，所以 action 漏了或写错会被直接判为协议错误。
//   2. 进程要一直活着处理下一行，不要处理完一个请求就退出（否则池会不停地重启进程）。
//   3. 诊断信息写 stderr，stdout 只允许出现协议 JSON。
//
// 写自己的插件：把 handle() 里的假数据换成真实刮削即可，其余照抄。

'use strict';

const readline = require('readline');

function respond(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
}

function handle(req) {
  // action 必须回显
  const base = { id: req.id, protocol: 1, action: req.action };

  if (req.action === 'scrape') {
    const title = (req.song && req.song.title) || '未知标题';
    respond(Object.assign(base, {
      ok: true,
      confidence: 0.95,
      source: 'example-js',
      matched: { id: 'ex-js-1', title: title },
      tags: { title: title, artist: '示例歌手', album: '示例专辑', year: '2024' },
      lyrics: '[00:00.00]示例歌词',
    }));
    return;
  }

  // 不支持的 action 也要回显 action，并给出明确错误码
  respond(Object.assign(base, {
    ok: false,
    error: { code: 'NOT_FOUND', message: 'example-js 不支持 action: ' + req.action },
  }));
}

const rl = readline.createInterface({ input: process.stdin });

rl.on('line', (line) => {
  const trimmed = line.trim();
  if (!trimmed) {
    return; // 空行忽略，不当成错误
  }
  let req;
  try {
    req = JSON.parse(trimmed);
  } catch (e) {
    respond({
      id: '', protocol: 1, action: 'scrape', ok: false,
      error: { code: 'BAD_REQUEST', message: '请求不是合法 JSON: ' + e.message },
    });
    return;
  }
  handle(req);
});
