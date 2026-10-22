import test from 'node:test';
import assert from 'node:assert/strict';
import { PLANS, planTasks, withCount, fetchPlan } from '../lib/planning.mjs';

const move = { type: 'move_stock', sku: 'SKU-A', lot: 'L1', from: 'RECV-01', to: 'S-2', quantity: 5 };

test('putaway and replenishment tasks keep the exact planned command', () => {
  const tasks = planTasks('putaway', { warehouse_id: 'wh', version: 3, tasks: [{ command: move, reason: 'consolidate_same_lot' }] }, 'wh');
  assert.equal(tasks.length, 1);
  assert.equal(tasks[0].command, move);
  assert.match(tasks[0].label, /RECV-01 -> S-2 \| 5 each \| consolidate same lot/);
  assert.equal(tasks[0].needsCount, false);
});

test('batch picks flatten stops in order with their totes', () => {
  const pick = n => ({ tote: n, quantity: 1, command: { type: 'pick', order_id: `o${n}`, line_id: 'l', location: 'A-1', lot: 'L', quantity: 1 } });
  const tasks = planTasks('batch', { warehouse_id: 'wh', version: 1, stops: [{ sequence: 1, location: 'A-1', sku: 'S', lot: 'L', picks: [pick(1), pick(2)] }] }, 'wh');
  assert.deepEqual(tasks.map(t => t.command.order_id), ['o1', 'o2']);
  assert.match(tasks[1].label, /tote 2/);
});

test('cycle counts need the observed quantity before they become a command', () => {
  const command = { type: 'count_stock', sku: 'S', lot: 'L', location: 'B-1', counted: null, reason: 'Scheduled class A cycle count' };
  const [task] = planTasks('counts', { warehouse_id: 'wh', version: 1, tasks: [{ location: 'B-1', sku: 'S', lot: 'L', class: 'A', last_counted_at: null, command }] }, 'wh');
  assert.equal(task.needsCount, true);
  assert.match(task.label, /never counted/);
  assert.equal(withCount(command, ' 12 ').counted, 12);
  assert.equal(withCount(command, '0').counted, 0);
  for (const bad of ['', '-1', '1.5', '012', 'x', '1000001']) assert.throws(() => withCount(command, bad));
  assert.equal(command.counted, null, 'the planned command is not mutated');
});

test('plans from another facility or without tasks are refused', () => {
  assert.throws(() => planTasks('putaway', { warehouse_id: 'other', version: 1, tasks: [] }, 'wh'), /verified/);
  assert.throws(() => planTasks('counts', { warehouse_id: 'wh', version: 1 }, 'wh'), /task list/);
  assert.throws(() => planTasks('nope', { warehouse_id: 'wh', version: 1 }, 'wh'), /Unknown/);
});

test('fetchPlan uses each plan route and surfaces server errors', async () => {
  const calls = [];
  const client = {
    warehousePath: (w, suffix) => `/v1/warehouses/${w}${suffix}`,
    call: async (method, path, body) => { calls.push([method, path, body]); return path.endsWith('/due') ? { ok: false, status: 503, data: { error: 'held' } } : { ok: true, status: 200, data: { ok: 1 } }; },
  };
  assert.deepEqual(await fetchPlan(client, 'wh', 'counts'), { ok: 1 });
  await assert.rejects(fetchPlan(client, 'wh', 'replenishment'), /HTTP 503: held/);
  assert.deepEqual(calls.map(c => c[0]), ['POST', 'GET']);
  assert.equal(Object.keys(PLANS).length, 4);
});
