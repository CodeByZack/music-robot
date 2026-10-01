import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readResumeMs, resumeKey, writeResumeMs } from './resume.ts';

test('键前缀与后端 RESUME_KEY_PREFIX 一致', () => {
  // 后端 src/server/routes/playback.rs: pub const RESUME_KEY_PREFIX: &str = "resume:";
  assert.equal(resumeKey(42), 'resume:42');
});

test('没听过 → 读不到', () => {
  assert.equal(readResumeMs({}, 42), null);
  assert.equal(readResumeMs({ volume: '30' }, 42), null);
});

test('写过 → 原样读回', () => {
  assert.equal(readResumeMs({ 'resume:42': '12345' }, 42), 12345);
});

test('太靠前的位置不算断点（否则每次进来都白跳一下）', () => {
  assert.equal(readResumeMs({ 'resume:42': '4999' }, 42), null);
  assert.equal(readResumeMs({ 'resume:42': '5000' }, 42), 5000);
});

test('脏值当没有，不 seek 到荒唐位置', () => {
  for (const bad of ['', 'abc', 'NaN', 'Infinity', '-1', '{}']) {
    assert.equal(readResumeMs({ 'resume:42': bad }, 42), null, `值 ${bad}`);
  }
});

test('只认自己那首歌的键，别人的不串台', () => {
  const s = { 'resume:42': '60000' };
  assert.equal(readResumeMs(s, 42), 60000);
  assert.equal(readResumeMs(s, 43), null);
  // 'resume:4' 与 'resume:42' 不能互相命中
  assert.equal(readResumeMs({ 'resume:4': '60000' }, 42), null);
});

test('写：刚开始 → 删键', () => {
  assert.equal(writeResumeMs(0, 200_000), null);
  assert.equal(writeResumeMs(4999, 200_000), null);
  assert.equal(writeResumeMs(5000, 200_000), '5000');
});

test('写：播到尾部 → 删键（播完自动下一首时顺手清掉上一首）', () => {
  assert.equal(writeResumeMs(195_000, 200_000), null);
  assert.equal(writeResumeMs(189_999, 200_000), '189999');
  // 恰好播完
  assert.equal(writeResumeMs(200_000, 200_000), null);
});

test('写：时长未知时不删（不确定就留着）', () => {
  assert.equal(writeResumeMs(123_456, 0), '123456');
  assert.equal(writeResumeMs(123_456, Number.NaN), '123456');
});

test('写：位置本身不是数字 → 删键，别把 NaN 写进库里', () => {
  assert.equal(writeResumeMs(Number.NaN, 200_000), null);
  assert.equal(writeResumeMs(Number.POSITIVE_INFINITY, 200_000), null);
});

test('写进去的值的能原样读回来（往返）', () => {
  const v = writeResumeMs(123_456.7, 200_000);
  assert.equal(v, '123456'); // 取整，别写小数点
  assert.equal(readResumeMs({ 'resume:7': v as string }, 7), 123_456);
});
