import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  createQueue,
  currentId,
  isFinished,
  jumpTo,
  next,
  prev,
  setMode,
} from './player-queue.ts';

/** 固定种子，测试才可复现。 */
function seeded(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

const IDS = [10, 20, 30, 40, 50];

test('顺序模式：order 是自然序，光标在起点', () => {
  const q = createQueue(IDS);
  assert.deepEqual(q.order, [0, 1, 2, 3, 4]);
  assert.equal(q.cursor, 0);
  assert.equal(currentId(q), 10);
  assert.equal(isFinished(q), false);
});

test('可以从指定曲目开始', () => {
  const q = createQueue(IDS, 'order', 30);
  assert.equal(q.cursor, 2);
  assert.equal(currentId(q), 30);
});

test('next 走到末尾 → 光标等于长度、currentId 为 null、isFinished', () => {
  let q = createQueue(IDS);
  for (let i = 0; i < 4; i++) q = next(q);
  assert.equal(currentId(q), 50, '第 5 首应该是 50');
  q = next(q);
  assert.equal(q.cursor, q.order.length);
  assert.equal(currentId(q), null);
  assert.equal(isFinished(q), true);
  // 到底之后再按 next 不变（不会越界）
  assert.deepEqual(next(q), q);
});

test('列表循环：到底回绕到第一首', () => {
  let q = createQueue(IDS, 'repeat-all');
  for (let i = 0; i < 5; i++) q = next(q);
  assert.equal(currentId(q), 10);
  assert.equal(isFinished(q), false, '循环模式永远不算放完');
});

test('单曲循环：next 不动光标', () => {
  const q = createQueue(IDS, 'repeat-one');
  assert.deepEqual(next(q), q);
  assert.equal(currentId(q), 10);
  assert.equal(isFinished(q), false);
});

test('prev 在开头不回绕（回绕只属于「下一首到底」）', () => {
  const q = createQueue(IDS);
  assert.deepEqual(prev(q), q);
  const at2 = next(next(q));
  assert.equal(currentId(at2), 30);
  assert.equal(currentId(prev(at2)), 20);
});

test('随机模式：起始曲目固定在第 0 位，其余是同一批下标的排列', () => {
  const q = createQueue(IDS, 'shuffle', 30, seeded(7));
  assert.equal(currentId(q), 30, '起始曲目必须还在原地');
  assert.deepEqual(
    [...q.order].sort((a, b) => a - b),
    [0, 1, 2, 3, 4],
    '不能丢下标也不能重复',
  );
});

test('切到随机：**当前这首歌不能跳**（天真实现最容易在这里出错）', () => {
  let q = createQueue(IDS);
  q = next(next(q));
  const before = currentId(q);
  assert.equal(before, 30);

  const shuffled = setMode(q, 'shuffle', seeded(3));
  assert.equal(currentId(shuffled), before, '按随机键不该把正在放的歌换掉');
  assert.equal(shuffled.cursor, 0, '当前曲被挪到 order 第 0 位');
});

test('切回顺序：光标落在当前曲目的自然位置上，不是回到 0', () => {
  let q = createQueue(IDS);
  q = next(next(next(q))); // 40
  const shuffled = setMode(q, 'shuffle', seeded(11));
  const back = setMode(shuffled, 'order');
  assert.equal(currentId(back), 40);
  assert.deepEqual(back.order, [0, 1, 2, 3, 4]);
  assert.equal(back.cursor, 3);
});

test('重复设置同一个模式是 no-op（返回同一个对象）', () => {
  const q = createQueue(IDS, 'shuffle', undefined, seeded(1));
  assert.equal(setMode(q, 'shuffle'), q);
});

test('jumpTo 能在随机序里正确定位', () => {
  const q = createQueue(IDS, 'shuffle', 10, seeded(5));
  const jumped = jumpTo(q, 40);
  assert.equal(currentId(jumped), 40);
  // 跳到已经在放的那首 → 状态不变
  assert.equal(jumpTo(jumped, 40), jumped);
  // 不在队列里 → 原样返回
  assert.equal(jumpTo(jumped, 999), jumped);
});

test('空队列不炸', () => {
  const q = createQueue([]);
  assert.equal(currentId(q), null);
  assert.deepEqual(next(q), q);
  assert.deepEqual(prev(q), q);
  assert.equal(isFinished(q), true, '空队列没有任何可放的，算放完');
});

test('随机源真的被用了（换种子结果不同）', () => {
  const a = createQueue(IDS, 'shuffle', 10, seeded(1)).order;
  const b = createQueue(IDS, 'shuffle', 10, seeded(2)).order;
  assert.notDeepEqual(a, b, '两个种子给出同样的顺序，说明 rand 没接上');
});
