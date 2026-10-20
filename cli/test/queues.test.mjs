import test from 'node:test';
import assert from 'node:assert/strict';
import { tableView, workQueues } from '../lib/queues.mjs';

test('open and attention queues exclude terminal orders and priority sorting preserves exact original records', () => {
  const rows = [
    { id: 'Original-normal', reference: 'SO-1', priority: 'normal', status: 'pending' },
    { id: 'original-high', reference: 'SO-2', priority: 'high', status: 'allocated' },
    { id: 'Original-urgent', reference: 'SO-3', priority: 'urgent', status: 'packed' },
    { id: 'Original-split', reference: 'SO-4', priority: 'high', status: 'partially_shipped' },
    ...['shipped', 'cancelled', 'closed_partial'].map(status => ({ id: status, reference: status, status, priority: 'urgent' })),
  ];
  const before = structuredClone(rows);
  const open = tableView('orders', { rows }, { filter: 'open', sort: 'priority' });
  assert.deepEqual(open.rows.map(row => row.id), ['Original-urgent', 'original-high', 'Original-split', 'Original-normal']);
  assert.equal(open.rows[0], rows[2], 'sort keeps the exact original row object');
  assert.deepEqual(rows, before, 'source snapshot order and values are unchanged');
  assert.equal(tableView('orders', { rows }, { filter: 'attention' }).rows.length, 3);
});

test('receiving-bin queues use actual location kind and positive stock without inferring availability', () => {
  const rows = [
    { sku: 'SKU', location: 'DOCK', lot: 'Original', on_hand: 4, reserved: 4 },
    { sku: 'SKU', location: 'EMPTY-DOCK', lot: 'Original', on_hand: 0, reserved: 0 },
    { sku: 'SKU', location: 'RECV-NAME-ONLY', lot: 'Original', on_hand: 9, reserved: 0 },
  ];
  const snapshot = { rows, locations: [{ code: 'DOCK', kind: 'receiving' }, { code: 'EMPTY-DOCK', kind: 'receiving' }, { code: 'RECV-NAME-ONLY', kind: 'storage' }] };
  assert.deepEqual(tableView('stocks', snapshot, { filter: 'receiving' }).rows, [rows[0]]);
  assert.equal(tableView('stocks', snapshot, { filter: 'reserved' }).rows[0].reserved, 4);
  assert.deepEqual(tableView('stocks', snapshot, { filter: 'zero' }).rows, [rows[1]]);
});

test('search intersects filters while numeric sorting keeps exact safe integer order', () => {
  const rows = [
    { sku: 'SKU-Case', location: 'A', lot: 'Lot', on_hand: Number.MAX_SAFE_INTEGER - 1, reserved: 1 },
    { sku: 'sku-case', location: 'B', lot: 'Lot', on_hand: Number.MAX_SAFE_INTEGER, reserved: 2 },
    { sku: 'SKU-Case', location: 'C', lot: 'Lot', on_hand: 5, reserved: 0 },
  ];
  const view = tableView('stocks', { rows }, { filter: 'reserved', sort: 'on_hand', query: 'SKU-CASE' });
  assert.deepEqual(view.rows.map(row => row.location), ['B', 'A']);
  assert.deepEqual(view.rows.map(row => row.sku), ['sku-case', 'SKU-Case']);
  assert.equal(tableView('stocks', { rows }, { filter: 'reserved', query: 'missing' }).rows.length, 0);
});

test('queue preview counts reflect the snapshot and distinguish unavailable data from zero records', () => {
  const snapshot = { orders: [{ status: 'pending', priority: 'urgent' }, { status: 'shipped', priority: 'urgent' }], receipts: [{ status: 'expected' }, { status: 'received' }], stocks: [], locations: [], exceptions: [{ status: 'open' }, { status: 'resolved' }] };
  const queues = workQueues(snapshot);
  assert.equal(queues.find(queue => queue.filter === 'open' && queue.kind === 'orders').count, 1);
  assert.equal(queues.find(queue => queue.kind === 'receipts').count, 1);
  assert.equal(queues.find(queue => queue.kind === 'stocks').count, 0);
  assert.equal(queues.find(queue => queue.kind === 'exceptions').count, 1);
  assert.ok(workQueues({ error: 'read unavailable' }).every(queue => queue.count === null));
});

test('unsupported view modes are held instead of silently changing the chosen filter', () => {
  assert.throws(() => tableView('orders', { rows: [] }, { filter: 'unknown' }), /Unknown table/);
  assert.throws(() => tableView('stocks', { rows: [] }, { sort: 'unknown' }), /Unknown table/);
});
