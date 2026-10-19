import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough, Writable } from 'node:stream';
import { Terminal, fit, wrap, width } from '../lib/terminal.mjs';
import { inspectRows, overviewLines, tableLines } from '../lib/views.mjs';

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
