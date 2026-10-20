import test from 'node:test';
import assert from 'node:assert/strict';
import { commandForm } from '../lib/commands.mjs';
import { rowWork, selectedWork, selectionFor } from '../lib/workflows.mjs';
import { readOrderReview } from '../lib/reviews.mjs';

const scope = { origin: 'https://example.com', tenant: 'tenant', actor: 'operator', warehouse: 'wh-original' };
const stock = { sku: 'SKU-Case', lot: 'Lot-É', location: 'Original-Bin', on_hand: 20, reserved: 2, serials: ['never-use-custody'] };
const receipt = { id: 'original-receipt', reference: 'RCV-1', status: 'expected', lines: [{ id: 'Original-LINE', sku: stock.sku, lot: stock.lot, quantity: 8, received: 0 }] };
const snapshot = { warehouse: { id: scope.warehouse }, version: 7, stocks: [stock], receipts: [receipt], orders: [] };

test('selected stock carries only exact source identities; physical destination, quantity and scans stay explicit', async () => {
  const selection = selectionFor(scope, 'stocks', stock, snapshot.version, 'move_stock');
  const { spec, identities } = selectedWork(snapshot, scope, selection, 'operator');
  assert.deepEqual(identities, { sku: 'SKU-Case', lot: 'Lot-É', from: 'Original-Bin' });
  const prompts = []; const values = ['Observed-Destination', '3'];
  const command = await commandForm(spec, async prompt => { prompts.push(prompt); return values.shift(); }, undefined, identities);
  assert.deepEqual(command, { type: 'move_stock', sku: stock.sku, lot: stock.lot, from: stock.location, to: 'Observed-Destination', quantity: 3 });
  assert.deepEqual(prompts, ['to: ', 'quantity (whole each): ']);
  assert.ok(!JSON.stringify(command).includes('never-use-custody'));
  for (const extra of [{ to: 'assumed' }, { quantity: 20 }, { serial_inputs: [] }]) {
    await assert.rejects(commandForm(spec, async () => assert.fail('must reject before asking'), undefined, { ...identities, ...extra }), /cannot supply quantities/);
  }
});

test('selected work is fenced by exact scope, reviewed version, original identity and current role', () => {
  const selection = selectionFor(scope, 'stocks', stock, 7, 'count_stock');
  for (const key of ['origin', 'tenant', 'actor', 'warehouse']) {
    assert.throws(() => selectedWork(snapshot, { ...scope, [key]: 'other' }, selection, 'admin'), /different server, tenant, actor or facility/);
  }
  assert.throws(() => selectedWork({ ...snapshot, version: 8 }, scope, selection, 'admin'), /view is stale/);
  assert.throws(() => selectedWork({ ...snapshot, stocks: [{ ...stock, sku: 'sku-case' }] }, scope, selection, 'admin'), /record is missing/);
  assert.throws(() => selectedWork(snapshot, scope, selection, 'operator'), /current role/);
  assert.throws(() => selectedWork(snapshot, scope, { ...selection, commandType: 'create_product' }, 'admin'), /unavailable/);
  assert.throws(() => selectedWork(snapshot, scope, { ...selection, kind: 'untrusted' }, 'admin'), /Unsupported/);
});

test('receiving requires an explicitly selected original line and never assumes observed units or receiving bin', async () => {
  const selection = selectionFor(scope, 'receipts', receipt, 7, 'receive', 'Original-LINE');
  const { spec, identities } = selectedWork(snapshot, scope, selection, 'operator');
  assert.deepEqual(identities, { receipt_id: 'original-receipt', line_id: 'Original-LINE' });
  const prompts = []; const values = ['2', 'Observed-Receiving-Bin'];
  const command = await commandForm(spec, async prompt => { prompts.push(prompt); return values.shift(); }, undefined, identities);
  assert.equal(command.quantity, 2); assert.equal(command.location, 'Observed-Receiving-Bin');
  assert.deepEqual(prompts, ['quantity (whole each): ', 'location: ']);
  for (const lineId of [undefined, 'original-line', 'other']) {
    assert.throws(() => selectedWork(snapshot, scope, { ...selection, lineId }, 'operator'), /exact original line/);
  }
  await assert.rejects(commandForm(spec, async () => assert.fail('no entry accepted'), undefined, { ...identities, location: 'assumed-bin' }), /cannot supply/);
});

test('row action suggestions respect role and terminal states without claiming server admission', () => {
  assert.deepEqual(rowWork('stocks', stock, 'oms'), []);
  assert.deepEqual(rowWork('receipts', { ...receipt, status: 'received' }, 'admin'), []);
  assert.deepEqual(rowWork('orders', { status: 'shipped' }, 'admin').map(spec => spec.type), ['report_exception']);
  assert.deepEqual(rowWork('orders', { status: 'pending' }, 'oms').map(spec => spec.type), ['cancel_order']);
  assert.ok(!rowWork('orders', { status: 'packed', shipment_mode: 'split' }, 'admin').some(spec => spec.type === 'ship'));
});

test('exception resolution carries only its exact original identity and requires open status, scope and current role', async () => {
  const exception = { id: 'Original-Exception', order_id: 'Original-Order', category: 'Damage', status: 'open', detail: 'Observed original issue', resolution: '' };
  const state = { ...snapshot, exceptions: [exception] };
  const selection = selectionFor(scope, 'exceptions', exception, 7, 'resolve_exception');
  const { spec, identities } = selectedWork(state, scope, selection, 'operator');
  assert.deepEqual(identities, { exception_id: exception.id });
  const prompts = [];
  assert.deepEqual(await commandForm(spec, async prompt => { prompts.push(prompt); return 'Actual investigation and disposition recorded'; }, undefined, identities), { type: 'resolve_exception', exception_id: exception.id, resolution: 'Actual investigation and disposition recorded' });
  assert.deepEqual(prompts, ['resolution: ']);
  assert.throws(() => selectedWork({ ...state, version: 8 }, scope, selection, 'operator'), /stale/);
  assert.throws(() => selectedWork(state, { ...scope, warehouse: 'other' }, selection, 'operator'), /different/);
  assert.throws(() => selectedWork(state, scope, selection, 'oms'), /current role/);
  assert.throws(() => selectedWork({ ...state, exceptions: [{ ...exception, id: 'original-exception' }] }, scope, selection, 'operator'), /missing/);
  assert.throws(() => selectedWork({ ...state, exceptions: [{ ...exception, status: 'resolved' }] }, scope, selection, 'operator'), /unavailable/);
  assert.throws(() => selectedWork(state, scope, { ...selection, lineId: 'unrelated' }, 'operator'), /does not take/);
  await assert.rejects(commandForm(spec, async () => assert.fail('must reject before asking'), undefined, { ...identities, resolution: 'Assumed resolution' }), /cannot supply/);
});

test('reporting an exception from an order preserves its optional source identity and requires actual issue details', async () => {
  const order = { id: 'Original-Order', status: 'shipped' };
  const state = { ...snapshot, orders: [order] };
  const selection = selectionFor(scope, 'orders', order, 7, 'report_exception');
  const { spec, identities } = selectedWork(state, scope, selection, 'operator');
  assert.deepEqual(identities, { order_id: 'Original-Order' });
  const prompts = []; const answers = ['Actual damage', 'Actual observed issue'];
  const command = await commandForm(spec, async prompt => { prompts.push(prompt); return answers.shift(); }, undefined, identities);
  assert.deepEqual(prompts, ['category: ', 'detail: ']);
  assert.deepEqual(command, { type: 'report_exception', order_id: 'Original-Order', category: 'Actual damage', detail: 'Actual observed issue' });
  const unlinked = ['', 'Actual unlinked issue', 'Actual facility observation'];
  assert.deepEqual(await commandForm(spec, async () => unlinked.shift()), { type: 'report_exception', order_id: null, category: 'Actual unlinked issue', detail: 'Actual facility observation' }, 'manual reports can still omit an order');
  assert.throws(() => selectedWork(state, scope, selection, 'oms'), /current role/);
  await assert.rejects(commandForm(spec, async () => assert.fail('must reject before asking'), undefined, { ...identities, category: 'Assumed category' }), /cannot supply/);
});

test('an order without a returnable physical handoff remains reviewable with the original server reason', async () => {
  const record = { id: 'Original / Order', status: 'allocated' }, shipments = { items: [] };
  const reason = { error: 'Returns require an original physical handoff' };
  const reads = [];
  const client = {
    warehousePath: (_warehouse, suffix) => `/v1/warehouses/Facility${suffix}`,
    async read(path) { reads.push(path); return path.endsWith('/shipments') ? shipments : record; },
    async call(method, path) { assert.equal(method, 'GET'); reads.push(path); return { ok: false, status: 409, data: reason }; },
  };
  assert.deepEqual(await readOrderReview(client, 'Facility', record.id), { record, shipments, returnEligibility: { unavailable: true, http_status: 409, response: reason } });
  assert.ok(reads.every(path => path.startsWith('/v1/warehouses/Facility/orders/Original%20%2F%20Order')));
});

test('order review retains successful eligibility and holds authentication or service failures', async () => {
  const client = { warehousePath: () => '/v1/warehouses/Facility/orders/Original', read: async () => ({}), call: async () => ({ ok: true, status: 200, data: { original: 'returnable handoff' } }) };
  assert.deepEqual((await readOrderReview(client, 'Facility', 'Original')).returnEligibility, { original: 'returnable handoff' });
  for (const status of [401, 403, 500, 503]) await assert.rejects(readOrderReview({ ...client, call: async () => ({ ok: false, status, data: { error: 'Original read failure' } }) }, 'Facility', 'Original'), new RegExp(`HTTP ${status}: Original read failure`));
  await assert.rejects(readOrderReview({ ...client, read: async () => { throw new Error('Original record unavailable'); } }, 'Facility', 'Original'), /Original record unavailable/);
});
