import test from 'node:test';
import assert from 'node:assert/strict';
import { orderProgress, remainingOrderLines, fulfillmentSummary } from '../lib/fulfillment.mjs';
import { readFulfillmentReview } from '../lib/reviews.mjs';
import { rowWork, selectionFor, selectedWork } from '../lib/workflows.mjs';
import { commandForm } from '../lib/commands.mjs';
import { tableView, workQueues } from '../lib/queues.mjs';
import { laneFor } from '../lib/board.mjs';

const scope = { origin: 'https://example.com', tenant: 'Original-Tenant', actor: 'Original-Actor', warehouse: 'Original-Facility' };
const fixture = () => ({ id: 'Original-Order', reference: 'Original / Order', status: 'partially_shipped', shipment_mode: 'packages', priority: 'normal', lines: [{ id: 'Original-LINE', sku: 'Original-SKU', quantity: 10 }, { id: 'original-line', sku: 'Original-SKU', quantity: 5 }], allocations: [{ line_id: 'Original-LINE', sku: 'Original-SKU', location: 'Original-Bin', lot: 'Original-Lot', quantity: 8, picked: 5, shipped: 3, released: 1 }, { line_id: 'original-line', sku: 'Original-SKU', location: 'Other-Bin', lot: 'Original-Lot', quantity: 2, picked: 0, shipped: 0, released: 0 }] });
const packages = () => [{ id: 'Original-Package', reference: 'Original actual pack', order_id: 'Original-Order', closure: null, lines: [{ line_id: 'Original-LINE', location: 'Original-Bin', lot: 'Original-Lot', quantity: 2 }] }];

test('remaining work keeps same-SKU original lines distinct and excludes released reservations', () => {
  const order = fixture(); const before = structuredClone(order); const progress = orderProgress(order);
  assert.deepEqual(progress.totals, { ordered: '15', allocated: '9', to_pick: '4', staged: '2', shipped: '3', cancelled: '0', unallocated: '6', remaining: '12' });
  assert.deepEqual(progress.lines.map(line => [line.id, line.to_pick, line.unallocated]), [['Original-LINE', '2', '3'], ['original-line', '2', '3']]);
  assert.deepEqual(remainingOrderLines(order, 'unpick'), [order.lines[0]]);
  assert.deepEqual(order, before);
  const huge = { ...order, status: 'pending', lines: order.lines.map(line => ({ ...line, quantity: Number.MAX_SAFE_INTEGER })), allocations: [] };
  assert.equal(orderProgress(huge).totals.ordered, '18014398509481982');
});

test('malformed, substituted, duplicated or overallocated facts hold remaining-work calculations', () => {
  for (const change of [{ picked: 9 }, { shipped: 6 }, { released: -1 }, { quantity: Number.MAX_SAFE_INTEGER + 1 }, { sku: 'original-sku' }, { line_id: 'Other-Line' }]) {
    const order = fixture(); Object.assign(order.allocations[0], change);
    assert.ok(orderProgress(order).error);
    assert.ok(!rowWork('orders', order, 'operator').some(spec => ['pick', 'unpick', 'allocate_available', 'release_unpicked', 'pack_shipment', 'cancel_remainder'].includes(spec.type)));
  }
  const duplicate = fixture(); duplicate.allocations.push({ ...duplicate.allocations[0] }); assert.ok(orderProgress(duplicate).error);
  const excess = fixture(); excess.allocations[0].quantity = 12; assert.ok(orderProgress(excess).error);
  assert.ok(orderProgress({ ...fixture(), lines: [] }).error);
});

test('closed original remainder is cancelled demand rather than new backordered work', () => {
  const order = fixture(); order.status = 'closed_partial';
  order.allocations = order.allocations.map(row => ({ ...row, picked: row.shipped, released: row.quantity - row.shipped }));
  order.remainder_cancellation = { lines: { 'Original-LINE': { sku: 'Original-SKU', ordered: 10, shipped: 3, cancelled: 7 }, 'original-line': { sku: 'Original-SKU', ordered: 5, shipped: 0, cancelled: 5 } } };
  const progress = orderProgress(order);
  assert.equal(progress.totals.cancelled, '12'); assert.equal(progress.totals.remaining, '0'); assert.equal(progress.totals.unallocated, '0');
  assert.deepEqual(rowWork('orders', order, 'operator').map(spec => spec.type), ['create_return', 'report_exception']);
  order.remainder_cancellation.lines['Original-LINE'].cancelled = 6; assert.ok(orderProgress(order).error);
});

test('partial package orders retain fresh role/version/line guards for remaining actions', async () => {
  const order = fixture(); const snapshot = { warehouse: { id: scope.warehouse }, version: 7, orders: [order] };
  const types = rowWork('orders', order, 'operator').map(spec => spec.type);
  for (const type of ['allocate_available', 'pick', 'unpick', 'release_unpicked', 'pack_shipment']) assert.ok(types.includes(type));
  assert.ok(!types.includes('cancel_remainder'), 'staged goods must be recovered before remainder closure');
  const selection = selectionFor(scope, 'orders', order, 7, 'pick', 'Original-LINE');
  const chosen = selectedWork(snapshot, scope, selection, 'operator');
  assert.deepEqual(chosen.identities, { order_id: order.id, line_id: 'Original-LINE' });
  const answers = ['Original-Bin', 'Original-Lot', '1'];
  const command = await commandForm(chosen.spec, async () => answers.shift(), undefined, chosen.identities);
  assert.deepEqual(command, { type: 'pick', order_id: order.id, line_id: 'Original-LINE', location: 'Original-Bin', lot: 'Original-Lot', quantity: 1 });
  assert.throws(() => selectedWork({ ...snapshot, version: 8 }, scope, selection, 'operator'), /stale/);
  assert.throws(() => selectedWork(snapshot, { ...scope, actor: 'Other' }, selection, 'operator'), /different/);
  assert.throws(() => selectedWork(snapshot, scope, selection, 'oms'), /current role/);
  const changed = structuredClone(order); changed.allocations[0].released = 3;
  assert.throws(() => selectedWork({ ...snapshot, orders: [changed] }, scope, selection, 'operator'), /exact original line/);
  order.allocations[0].picked = order.allocations[0].shipped;
  const closure = selectedWork(snapshot, scope, selectionFor(scope, 'orders', order, 7, 'cancel_remainder'), 'oms');
  const prompts = [];
  assert.deepEqual(await commandForm(closure.spec, async prompt => { prompts.push(prompt); return 'Actual decision to close remaining demand'; }, undefined, closure.identities), { type: 'cancel_remainder', order_id: order.id, reason: 'Actual decision to close remaining demand' });
  assert.deepEqual(prompts, ['reason: ']);
  await assert.rejects(commandForm(closure.spec, async () => assert.fail('no assumed reason'), undefined, { ...closure.identities, reason: 'Assumed' }), /cannot supply/);
});

test('partial queue includes active package work before and after handoff and preserves old queue positions', () => {
  const rows = [fixture(), { ...fixture(), id: 'Before-Handoff', status: 'allocated' }, ...['shipped', 'cancelled', 'closed_partial'].map(status => ({ ...fixture(), id: status, status })), { ...fixture(), id: 'Legacy-Order', shipment_mode: null, status: 'picked' }];
  const view = tableView('orders', { rows }, { filter: 'partial', sort: 'priority' });
  assert.deepEqual(view.rows.map(row => row.id).sort(), ['Before-Handoff', 'Original-Order']);
  const queues = workQueues({ orders: rows }); assert.equal(queues[8].kind, 'exceptions'); assert.equal(queues[9].filter, 'partial'); assert.equal(queues[9].count, 2);
  assert.equal(laneFor('l', { orders: rows }).count, 2);
  assert.equal(laneFor('l', { error: 'Unavailable' }).count, null);
});

test('remaining-work review binds live identity, original order, facility, version and package source', async () => {
  const order = fixture(); const original = structuredClone(order); const page = { warehouse_id: scope.warehouse, order_id: order.id, version: 7, ok: true, packages: packages() };
  const reads = [];
  const client = { origin: scope.origin, session: async () => ({ identity: { tenant: scope.tenant, actor: scope.actor } }), warehousePath: (_id, suffix = '') => `/v1/warehouses/Original${suffix}`, async read(path) { reads.push(path); return path.endsWith('/shipments') ? page : { warehouse: { id: scope.warehouse }, version: 7, orders: [order] }; } };
  const review = await readFulfillmentReview(client, scope, order.id, 7);
  const summary = fulfillmentSummary(scope, review);
  for (const value of ['Remaining active demand: 12 each', 'Unallocated: 6 each', 'To pick: 4 each', 'Original-LINE', 'original-line', 'Original-Package', 'already be packed']) assert.ok(summary.includes(value));
  assert.deepEqual(order, original); assert.equal(reads.length, 2);
  await assert.rejects(readFulfillmentReview(client, { ...scope, actor: 'Other' }, order.id, 7), /different/);
  await assert.rejects(readFulfillmentReview(client, scope, order.id, 8), /stale/);
  await assert.rejects(readFulfillmentReview(client, scope, 'original-order', 7), /missing/);
  for (const change of [{ ok: false }, { version: 8 }, { order_id: 'Other' }, { warehouse_id: 'Other' }]) await assert.rejects(readFulfillmentReview({ ...client, read: async path => path.endsWith('/shipments') ? { ...page, ...change } : { warehouse: { id: scope.warehouse }, version: 7, orders: [order] } }, scope, order.id, 7), /source review/);
});
