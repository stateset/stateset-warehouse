import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough, Writable } from 'node:stream';
import { Terminal, fit, wrap, width } from '../lib/terminal.mjs';
import { inspectJson, inspectRows, overviewLines, scanProgress, tableLines } from '../lib/views.mjs';

function scripted(answers, columns = 48, rows = 20) {
  const screens = []; let current;
  return {
    columns, rows, screens,
    screen(title) { current = { title, lines: [] }; screens.push(current); },
    line(text) { current.lines.push(...wrap(text, columns)); },
    print(text) { current.lines.push(text); },
    async ask() { assert.ok(answers.length, 'unexpected extra prompt'); return answers.shift(); },
  };
}

test('narrow tables preserve graphemes, align wide labels and escape terminal controls', () => {
  for (const value of ['倉庫 / café e\u0301', '👩🏽‍💻📦 / 123', 'bad\x1b[2J\n\u202eSKU']) {
    assert.equal(width(fit(value, 12)), 12);
    assert.equal(width(fit(value, 12, true)), 12);
    for (const line of wrap(value, 12)) assert.ok(width(line) <= 12);
  }
  assert.equal(fit('e\u0301e\u0301e\u0301', 2), 'e\u0301…');
  assert.equal(width('倉庫'), 4);
  const row = { sku: '倉庫\x1b[2J' + 'x'.repeat(100), location: 'A', lot: 'exact-lot', on_hand: 123, reserved: 2 };
  for (const columns of [20, 35, 69, 120]) {
    const lines = tableLines('stocks', [row], columns);
    assert.ok(lines.every(line => width(line) <= columns));
    assert.ok(lines.every(line => !/[\x00-\x1f\x7f-\x9f\u202e]/.test(line)));
  }
});

test('table search always uses the full snapshot, clears, pages, refreshes and opens complete original details', async () => {
  const rows = Array.from({ length: 15 }, (_, n) => ({ sku: n % 2 ? `Beta-${n}` : `Alpha-${n}`, location: 'A', lot: `original-${n}`, on_hand: n, reserved: 0, id: `full-original-${n}` }));
  let loads = 0;
  const ui = scripted(['n', 'p', '/BETA', '/alpha', '/', '1', '', 'r', '/missing', '/', '']);
  await inspectRows(ui, 'stocks', async () => { loads++; return { title: `snapshot v${loads}`, rows }; });
  const bodies = ui.screens.map(screen => screen.lines.join('\n'));
  assert.ok(bodies[0].includes('1–7 of 15 rows'));
  assert.ok(bodies[1].includes('8–14 of 15 rows'));
  assert.ok(bodies[3].includes('1–7 of 7 rows'));
  assert.ok(bodies[4].includes('1–7 of 8 rows'), 'replacing search is not cumulative');
  assert.ok(bodies[5].includes('1–7 of 15 rows'));
  assert.ok(bodies[6].includes('"id": "full-original-0"'), 'detail retains full original identity');
  assert.equal(loads, 2);
  assert.ok(ui.screens.some(screen => screen.title === 'snapshot v2'));
  assert.ok(bodies.some(body => body.includes('0–0 of 0 rows') && body.includes('No matches.')));
  assert.ok(ui.screens.every(screen => screen.lines.every(line => width(line) <= ui.columns)));
});

test('empty views stay usable and invalid row choices do not leave the table', async () => {
  const ui = scripted(['1', '/any', '/', '']);
  await inspectRows(ui, 'stocks', async () => ({ title: 'Empty inventory', rows: [] }));
  assert.equal(ui.screens.length, 4);
  assert.ok(ui.screens[0].lines.includes('0–0 of 0 rows'));
  assert.ok(ui.screens[0].lines.includes('No records in this snapshot.'));
  assert.ok(ui.screens[1].lines.join(' ').includes('Choose a row number'));
});

test('overview distinguishes quantities, terminal orders, pending recovery and read failures', () => {
  const snapshot = {
    warehouse: { code: 'PDX', name: 'Portland' }, version: 9, readAt: '2026-10-19T22:00:00.000Z',
    stocks: [{ on_hand: Number.MAX_SAFE_INTEGER, reserved: 3 }, { on_hand: 10, reserved: 2 }],
    orders: ['pending', 'partially_shipped', 'shipped', 'cancelled', 'closed_partial'].map(status => ({ status })),
    receipts: ['expected', 'receiving', 'received', 'cancelled'].map(status => ({ status })),
    exceptions: [{ status: 'open' }, { status: 'resolved' }],
  };
  const session = { identity: { actor: 'operator', tenant: 't', role: 'admin' }, demo: true };
  const body = overviewLines(snapshot, { count: 1 }, session, 'wh-pdx').join('\n');
  assert.ok(body.includes('On hand 9007199254741001 ea · Reserved 5 ea'), 'aggregates retain integer precision');
  assert.ok(body.includes('Open orders 2 · Incoming 2 · Exceptions 1'));
  assert.ok(body.includes('SYNTHETIC DEMO'));
  assert.ok(body.includes('ACTION: 1 pending original request'));
  const held = overviewLines({ error: 'read\x1b[2Jfailed' }, { error: 'journal held' }, session, 'wh-pdx').join('\n');
  assert.ok(held.includes('Snapshot unavailable:'));
  assert.ok(held.includes('Recovery check held:'));
  assert.ok(!held.includes('no pending'));
  assert.ok(!held.includes('\x1b'));
});

test('menu search keeps original command numbers and a short pane keeps its prompt visible', async () => {
  const input = new PassThrough(); let outputText = '';
  const output = new Writable({ write(chunk, _encoding, done) { outputText += chunk; done(); } });
  output.columns = 49; output.rows = 20;
  const ui = new Terminal(input, output);
  input.end('/command 24\n24\n');
  try {
    const items = Array.from({ length: 36 }, (_, n) => [`Command ${n + 1}`, `value-${n + 1}`]);
    assert.equal(await ui.menu('Work', items), 'value-24');
    const screens = outputText.split('STATESET / WAREHOUSE\n').slice(1);
    assert.ok(screens[0].includes('Page 1/'));
    assert.ok(screens[1].includes('24. Command 24'));
    for (const screen of screens) {
      assert.ok(screen.split('\n').length <= 20);
      assert.ok(screen.split('\n').every(line => width(line) <= 48));
    }
  } finally { ui.close(); }
});

test('row actions require a highlighted exact record and reload the snapshot after returning', async () => {
  const original = { sku: 'Original-SKU', location: 'A', lot: 'original', on_hand: 5, reserved: 0 };
  const ui = scripted(['a', { key: 'down' }, 'a', '']);
  const calls = []; let loads = 0;
  await inspectRows(ui, 'stocks', async () => ({ title: `snapshot v${++loads}`, version: loads, rows: [original] }), { onAction: async (row, version) => { calls.push({ row, version }); } });
  assert.deepEqual(calls, [{ row: original, version: 1 }]);
  assert.equal(loads, 2);
  assert.ok(ui.screens[1].lines.join(' ').includes('Highlight a row'));
  assert.equal(ui.screens.at(-1).title, 'snapshot v2');
});

test('correction is enabled only in an unsent review and scanning keeps physical source context visible', async () => {
  const review = scripted([{ key: 'f2' }]);
  assert.equal(await inspectJson(review, 'Unsent review', { serials: ['Observed'] }, { allowUndo: true }), 'undo');
  const outcome = scripted([{ key: 'f2' }, '']);
  assert.equal(await inspectJson(outcome, 'Committed original outcome', { serials: ['Observed'] }), true);
  const ui = scripted([]);
  scanProgress(ui, { observed: 1, required: 2, targetIndex: 0, targetCount: 1, target: { each_quantity: 2 }, latest: [{ sku: 'SKU', serial: 'Original' }], message: 'Duplicate; scan again' }, { warehouse: 'wh' }, 7, { type: 'move_stock', from: 'A', to: 'B', lot: 'Original-Lot' });
  assert.ok(ui.screens[0].lines.includes('Move: A → B | lot Original-Lot'));
  assert.ok(ui.screens[0].lines.includes('SKU | Original'));
  assert.ok(ui.screens[0].lines.includes('Duplicate; scan again'));
});

test('sort changes clear selection before an action can target a reordered record', async () => {
  const rows = [{ sku: 'Original', location: 'A', lot: 'lot', on_hand: 1, reserved: 0 }, { sku: 'original', location: 'B', lot: 'lot', on_hand: 9, reserved: 0 }];
  const ui = scripted([{ key: 'down' }, { key: 'f4' }, 'a', { key: 'down' }, 'a', '']);
  ui.menu = async (_title, choices) => { assert.ok(choices.some(([, value]) => value === 'on_hand')); return 'on_hand'; };
  const called = [];
  await inspectRows(ui, 'stocks', async () => ({ title: 'Original snapshot', version: 7, rows }), { onAction: async row => called.push(row) });
  assert.deepEqual(called, [rows[1]], 'only the explicitly highlighted row after sorting is used');
  assert.ok(ui.screens.some(screen => screen.lines.join(' ').includes('Highlight a row')));
});

test('filter changes clear selection while refresh retains filter, sort and search', async () => {
  let loads = 0;
  const rows = [{ sku: 'Alpha', location: 'A', lot: 'lot', on_hand: 2, reserved: 0 }, { sku: 'Beta', location: 'B', lot: 'lot', on_hand: 9, reserved: 1 }];
  const ui = scripted(['/BETA', { key: 'down' }, { key: 'f3' }, 'a', 'r', ''], 100);
  ui.menu = async () => 'reserved';
  await inspectRows(ui, 'stocks', async () => ({ title: `snapshot v${++loads}`, version: loads, rows }), { sort: 'on_hand', onAction: async () => assert.fail('a filter change must clear the old selection') });
  assert.equal(loads, 2);
  const last = ui.screens.at(-1).lines.join(' ');
  assert.ok(last.includes('/BETA'));
  assert.ok(last.includes('Stock with reservations'));
  assert.ok(last.includes('On hand: highest first'));
  assert.ok(last.includes('Beta'));
  assert.ok(!last.includes('Alpha'));
});

test('server batches cross both boundaries, preserve search and refresh from the beginning', async () => {
  const requests = [];
  const ui = scripted(['/Original', 'n', '1', '', 'p', 'n', 'r', ''], 100, 40);
  const first = { sku: 'SKU', lot: 'Original-First', physical: 1, reserved: 0, usable: 1, availability: 'available' };
  const last = { ...first, lot: 'Original-Last' };
  const cursor = '["SKU","Original / & ?"]';
  await inspectRows(ui, 'lots', async after => {
    requests.push(after);
    return { title: `batch ${after === null ? 'one' : 'two'}`, rows: [after === null ? first : last], nextCursor: after === null ? cursor : null };
  }, { serverPaged: true });
  assert.deepEqual(requests, [null, cursor, null, cursor, null]);
  assert.ok(ui.screens.some(screen => screen.lines.join(' ').includes('"lot": "Original-Last"')));
  assert.ok(ui.screens.at(-1).lines.join(' ').includes('/Original'));
  assert.ok(ui.screens.at(-1).lines.join(' ').includes('Batch 1'));
  assert.ok(ui.screens.some(screen => screen.lines.join(' ').includes('Batch 2 · last batch')));
});

test('an empty filtered batch can advance and failed reads retain its original position for retry', async () => {
  const requests = []; let failed = false;
  const ui = scripted(['/needle', 'n', 'n', 'p', 'n', ''], 100, 40);
  await inspectRows(ui, 'lots', async cursor => {
    requests.push(cursor);
    if (cursor === 'next' && !failed) { failed = true; throw new Error('Synthetic read interruption'); }
    return { title: cursor === null ? 'first original batch' : 'second original batch', rows: [{ sku: 'SKU', lot: cursor === null ? 'not-a-match' : 'needle', physical: 1 }], nextCursor: cursor === null ? 'next' : null };
  }, { serverPaged: true });
  assert.deepEqual(requests, [null, 'next', 'next', null, 'next']);
  const held = ui.screens.find(screen => screen.lines.join(' ').includes('Synthetic read interruption'));
  assert.equal(held.title, 'first original batch');
  assert.ok(held.lines.join(' ').includes('Batch 1'));
  assert.ok(ui.screens.at(-1).lines.join(' ').includes('needle'));
});

test('repeated server cursors cannot silently loop through the same records', async () => {
  let loads = 0;
  const ui = scripted(['n', 'n', ''], 100, 40);
  await inspectRows(ui, 'lots', async () => { loads++; return { title: 'Original batch', rows: [], nextCursor: 'repeat' }; }, { serverPaged: true });
  assert.equal(loads, 2);
  assert.ok(ui.screens.at(-1).lines.join(' ').includes('Repeated server cursor'));
});
