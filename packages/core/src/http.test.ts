import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ApiError, createHttp } from './http.ts';
import { createMemoryTokenStore } from './token-store.ts';

/** 假 fetch：记录收到的请求，按脚本返回响应。 */
function fakeFetch(script: {
  status?: number;
  body?: string;
  headers?: Record<string, string>;
}): { fetch: typeof globalThis.fetch; calls: Array<{ url: string; init: RequestInit }> } {
  const calls: Array<{ url: string; init: RequestInit }> = [];
  const fetchImpl = (async (url: string | URL | Request, init?: RequestInit) => {
    calls.push({ url: String(url), init: init ?? {} });
    const status = script.status ?? 200;
    // ⚠️ 204 不允许带 body，Response 构造器会直接抛
    const payload = status === 204 ? null : (script.body ?? '');
    return new Response(payload, {
      status,
      headers: { 'content-type': 'application/json', ...(script.headers ?? {}) },
    });
  }) as unknown as typeof globalThis.fetch;
  return { fetch: fetchImpl, calls };
}

function build(script: Parameters<typeof fakeFetch>[0], token: string | null = null) {
  const tokens = createMemoryTokenStore(token);
  const { fetch, calls } = fakeFetch(script);
  const http = createHttp({ baseUrl: 'http://x/api/', tokens, fetch });
  return { http, calls, tokens };
}

test('有令牌时带 Authorization，没有时不带', async () => {
  const withToken = build({ body: '{"ok":true}' }, 'tok-123');
  await withToken.http.get('/songs');
  const h1 = withToken.calls[0]!.init.headers as Record<string, string>;
  assert.equal(h1.Authorization, 'Bearer tok-123');

  const noToken = build({ body: '{"ok":true}' });
  await noToken.http.get('/songs');
  const h2 = noToken.calls[0]!.init.headers as Record<string, string>;
  assert.equal(h2.Authorization, undefined, '没登录就不该带空 Bearer');
});

test('baseUrl 尾斜杠与 path 前斜杠不会拼出双斜杠', async () => {
  const { http, calls } = build({ body: '{}' });
  await http.get('/songs');
  assert.equal(calls[0]!.url, 'http://x/api/songs');
  await http.get('songs');
  assert.equal(calls[1]!.url, 'http://x/api/songs');
});

test('只有带 body 时才设 Content-Type（GET 不带）', async () => {
  const { http, calls } = build({ body: '{}' });
  await http.get('/songs');
  assert.equal((calls[0]!.init.headers as Record<string, string>)['Content-Type'], undefined);
  await http.post('/songs', { title: 'x' });
  assert.equal((calls[1]!.init.headers as Record<string, string>)['Content-Type'], 'application/json');
  assert.equal(calls[1]!.init.body, '{"title":"x"}');
  assert.equal(calls[0]!.init.body, undefined, 'GET 不该有 body');
});

test('204 与空响应体返回 undefined（不抛 JSON 解析错）', async () => {
  const a = build({ status: 204 });
  assert.equal(await a.http.del('/playlists/1/items/2'), undefined);

  const b = build({ status: 200, body: '' });
  assert.equal(await b.http.get('/jobs'), undefined);
});

test('成功响应解析成对象', async () => {
  const { http } = build({ body: '{"total":3,"items":[]}' });
  const r = await http.get<{ total: number }>('/library');
  assert.equal(r.total, 3);
});

test('后端错误体 → ApiError 带 code 与中文文案', async () => {
  const { http } = build({
    status: 409,
    body: '{"error":{"code":"CONFLICT","message":"已有扫描任务在跑"}}',
  });
  await assert.rejects(
    () => http.post('/scan'),
    (e: unknown) => {
      assert.ok(e instanceof ApiError);
      assert.equal(e.status, 409);
      assert.equal(e.code, 'CONFLICT');
      assert.equal(e.message, '已有扫描任务在跑');
      assert.equal(e.isUnauthorized, false);
      return true;
    },
  );
});

test('401 → 回调一次，且 isUnauthorized 为真', async () => {
  let hits = 0;
  const tokens = createMemoryTokenStore('expired');
  const { fetch } = fakeFetch({ status: 401, body: '{"error":{"code":"UNAUTHORIZED","message":"令牌过期"}}' });
  const http = createHttp({
    baseUrl: 'http://x',
    tokens,
    fetch,
    onUnauthorized: () => {
      hits += 1;
    },
  });
  await assert.rejects(() => http.get('/auth/me'), (e: unknown) => {
    assert.ok(e instanceof ApiError);
    assert.equal(e.isUnauthorized, true);
    return true;
  });
  assert.equal(hits, 1, '401 回调必须恰好一次（core 不重试：后端没有 refresh）');
});

test('错误体不是 JSON（反代返回 HTML）也不吞掉状态码', async () => {
  const { http } = build({ status: 502, body: '<html>bad gateway</html>' });
  await assert.rejects(
    () => http.get('/library'),
    (e: unknown) => {
      assert.ok(e instanceof ApiError);
      assert.equal(e.status, 502);
      assert.equal(e.code, 'HTTP_502', '拿不到 error.code 时按状态码兜底');
      assert.match(e.message, /502/);
      return true;
    },
  );
});

test('网络异常原样抛出（不是 ApiError）', async () => {
  const tokens = createMemoryTokenStore(null);
  const boom = (async () => {
    throw new TypeError('fetch failed');
  }) as unknown as typeof globalThis.fetch;
  const http = createHttp({ baseUrl: 'http://x', tokens, fetch: boom });
  await assert.rejects(() => http.get('/library'), TypeError);
});

test('登录后把令牌写进 store 就能复用（token-store 是唯一注入点）', async () => {
  const tokens = createMemoryTokenStore(null);
  const { fetch, calls } = fakeFetch({ body: '{}' });
  const http = createHttp({ baseUrl: 'http://x', tokens, fetch });

  await http.get('/library');
  assert.equal((calls[0]!.init.headers as Record<string, string>).Authorization, undefined);

  tokens.set('fresh');
  await http.get('/library');
  assert.equal(
    (calls[1]!.init.headers as Record<string, string>).Authorization,
    'Bearer fresh',
  );
});
