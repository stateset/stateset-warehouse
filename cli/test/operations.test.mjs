import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough, Writable } from 'node:stream';
import { Terminal } from '../lib/terminal.mjs';
import { inspectRows, inspectText, overviewLines } from '../lib/views.mjs';
import { workSummary } from '../lib/summaries.mjs';
import { collectWork, lineSources } from '../lib/entry.mjs';
import { commands } from '../lib/commands.mjs';

function device(columns, rows) {
  const input = new PassThrough(); input.isTTY = true; input.isRaw = false; input.setRawMode = value => { input.isRaw = value; }; let text = '';
  const output = new Writable({ write(chunk, _encoding, done) { text += chunk; done(); } }); output.isTTY = true; output.columns = columns; output.rows = rows;
  const ui = new Terminal(input, output, { TERM: 'xterm-ghostty', TERM_PROGRAM: 'ghostty', NO_COLOR: '1' });
  return { ui, input, frames: () => text.split('\x1b[2J\x1b[H').slice(1).map(frame => frame.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '')) };
}
test('Ghostty operations menus keep context and prompts within compact and wide panes', async () => {
  const snapshot = { warehouse: { code: 'PDX', name: 'Portland' }, version: 17, stocks: [], orders: [], receipts: [], exceptions: [], readAt: '2026-10-20T12:00:00.000Z' };
  const intro = overviewLines(snapshot, { count: 2 }, { identity: { actor: 'Original-' + 'x'.repeat(150), role: 'admin', tenant: 'Original-Tenant' }, demo: true }, 'Original-Facility');
  for (const [columns, rows] of [[49, 20], [70, 24], [70, 44], [120, 32], [30, 12]]) {
    const io = device(columns, rows); io.input.write('/missing\ninvalid\n/\nv\n' + 'n\n'.repeat(30) + '\nq\n');
    try { await io.ui.menu('WMS operations center', Array.from({ length: 20 }, (_, index) => [`Work ${index + 1}`, index]), { intro, sections: { 1: 'WORK', 9: 'CONTROL', 16: 'OPERATIONS' }, compactIntro: ['Facility: Original-Facility', 'Allocate 0 · Pick 0 · Pack 0 · Ship 0'], compactHint: ['t queues · i stock · o orders · w work', '↑↓ Enter · / find · r refresh · v info · q'] }); }
    finally { io.ui.close(); }
    assert.ok(io.frames().every(frame => frame.trimEnd().split('\n').length <= rows), `${columns}×${rows} frame overflow`);
    assert.ok(io.frames()[0].includes('ACTION:'), 'pending original work remains visible');
    assert.ok(io.frames().some(frame => frame.includes('Original-Tenant')), 'full original context remains reachable');
  }
});
test('compact lot tables retain full context and keep prompts on screen', async () => {
  const io = device(49, 20); io.input.write('/missing\nv\n\n/\nq\n');
  const snapshot = { title: 'Lots | Original-Facility | v7', warehouseId: 'Original-Facility', version: 7, rows: [{ sku: 'SKU', lot: 'Original-Lot', availability: 'quarantined', physical: 4, reserved: 2, usable: 0 }], note: 'Availability checked at 2026-10-20T12:00:00.123456Z; the original complete time must remain readable.', nextCursor: null };
  try { await inspectRows(io.ui, 'lots', async () => snapshot, { serverPaged: true, filter: 'quarantined', onAction: async () => {} }); } finally { io.ui.close(); }
  assert.ok(io.frames().every(frame => frame.trimEnd().split('\n').length <= 20));
  assert.ok(io.frames().some(frame => frame.replace(/[\r\n]/g, '').includes('2026-10-20T12:00:00.123456Z')));
});
test('readable work reviews retain complete literal values and original JSON remains inspectable', async () => {
  const scope = { origin: 'https://example.com', warehouse: 'Original-Facility', actor: 'Original-Actor', tenant: 'Original-Tenant' };
  const request = { request_id: 'original-request', expected_version: 7, command: { type: 'move_stock', sku: 'Original-SKU', lot: 'original-lot', from: 'From', to: 'To', quantity: 2 }, serial_inputs: [{ target: [], sku: 'Original-SKU', serials: ['Original', 'original'] }] };
  const original = structuredClone(request); const summary = workSummary(scope, request);
  for (const value of ['Original-Facility', 'Original-Actor', 'Original-Tenant', 'Original-SKU', 'original-lot', 'Original', 'original', 'Quantity (each): 2']) assert.ok(summary.includes(value));
  const io = device(49, 20); io.input.write('j\n\n\n');
  try { assert.equal(await inspectText(io.ui, 'Review actual work', summary, { details: { scope, request } }), true); } finally { io.ui.close(); }
  assert.deepEqual(request, original);
  assert.ok(io.frames().some(frame => frame.includes('Complete original data')));
});

const scope = { origin: 'https://example.com', warehouse: 'Facility', actor: 'Actor', tenant: 'Tenant' };
const snapshot = { version: 7, stocks: [], orders: [], receipts: [] };
function scripted(values) { return { lines: [], async ask() { assert.ok(values.length, 'unexpected prompt'); return values.shift(); }, line(text) { this.lines.push(text); } }; }
function packs(change = {}) { return { warehousePath: (_warehouse, suffix) => `/v1/warehouses/Facility${suffix}`, async read(path) { const q = new URL(path, scope.origin).searchParams; assert.equal(q.get('review'), '7'); return { warehouse_id: 'Facility', version: 7, sku: 'SKU', code: q.get('code'), page_source_ok: true, next_cursor: null, items: [{ id: 'Original-Definition', code: 'BOX', enabled: true, source_ok: true, each_per_pack: 6 }], ...change }; } }; }
test('actual pack scans preserve original definitions and exact integer conversion in the request', async () => {
  const ui = scripted(['SKU', 'Lot', 'From', 'To', 'pack', 'box', 'BOX', '1.5', '2']);
  const result = await collectWork(packs(), ui, scope, snapshot, commands.find(command => command.type === 'move_stock'));
  assert.equal(result.command.quantity, 12);
  assert.deepEqual(result.unit_inputs, [{ target: [], sku: 'SKU', pack_code: 'BOX', definition_id: 'Original-Definition', pack_quantity: 2, each_per_pack: 6 }]);
  assert.ok(ui.lines.some(line => line.includes('Observed 2 BOX')));
});
test('pack source failures hold entry and replacing an oversized pack observation clears its proof', async () => {
  await assert.rejects(collectWork(packs({ version: 8 }), scripted(['SKU', 'Lot', 'From', 'To', 'pack', 'BOX']), scope, snapshot, commands.find(command => command.type === 'move_stock')), /changed or are held/);
  const stock = { sku: 'SKU', lot: 'Lot', location: 'From', on_hand: 2, reserved: 0 };
  const ui = scripted(['Dispatch', 'Carrier', 'Actual-Handoff', 'Original-Line', 'pack', 'BOX', '1', '2', 'no']);
  const result = await collectWork(packs(), ui, scope, { ...snapshot, stocks: [stock] }, commands.find(command => command.type === 'dispatch_transfer'), { source: stock, identities: {} }, async () => 'Original-Destination');
  assert.equal(result.command.lines[0].quantity, 2);
  assert.deepEqual(result.unit_inputs, [], 'rejected conversion is absent from the final original request');
});
test('guided package and return sources subtract existing claims and preserve original tuples', () => {
  const chosen = { source: { allocations: [{ line_id: 'Original-Line', sku: 'SKU', location: 'From', lot: 'Lot', picked: 5, shipped: 1 }] }, shipments: { packages: [{ closure: null, lines: [{ line_id: 'Original-Line', location: 'From', lot: 'Lot', quantity: 2 }] }] }, returns: { lines: [{ order_line_id: 'Original-Line', sku: 'SKU', lot: 'Lot', remaining: 2 }] } };
  const packed = lineSources(snapshot, chosen, { type: 'pack_shipment', lines: [{ line_id: 'Original-Line', location: 'From', lot: 'Lot', quantity: 1 }] });
  assert.equal(packed[0].maximum, 1);
  assert.deepEqual(packed[0].identities, { line_id: 'Original-Line', location: 'From', lot: 'Lot' });
  assert.deepEqual(lineSources(snapshot, chosen, { type: 'create_return', lines: [{ order_line_id: 'Original-Line', lot: 'Lot', quantity: 2 }] }), []);
});
