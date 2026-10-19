import test from 'node:test';
import assert from 'node:assert/strict';
import { commandForm } from '../lib/commands.mjs';
import { rowWork, selectedWork, selectionFor } from '../lib/workflows.mjs';

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
  assert.deepEqual(rowWork('orders', { status: 'shipped' }, 'admin'), []);
  assert.deepEqual(rowWork('orders', { status: 'pending' }, 'oms').map(spec => spec.type), ['cancel_order']);
  assert.ok(!rowWork('orders', { status: 'packed', shipment_mode: 'split' }, 'admin').some(spec => spec.type === 'ship'));
});
