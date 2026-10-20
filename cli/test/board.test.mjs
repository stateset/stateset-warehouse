import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough, Writable } from 'node:stream';
import { OperationsBoard, refreshInterval, laneFor, attentionLines } from '../lib/board.mjs';
import { Terminal } from '../lib/terminal.mjs';
import { tableLines, inspectRows } from '../lib/views.mjs';
import { width } from '../lib/format.mjs';

const scope = { origin: 'https://example.com', tenant: 'Original-Tenant', actor: 'Original-Actor', warehouse: 'Original-Facility' };
const session = { identity: { tenant: scope.tenant, actor: scope.actor, role: 'admin' }, demo: true };
const fixture = () => ({ warehouse: { code: 'PDX', name: 'Original facility' }, version: 7, stocks: [{ sku: 'Original', location: 'Dock', lot: 'Original', on_hand: 123456, reserved: 23 }], locations: [{ code: 'Dock', kind: 'receiving' }], orders: [{ id: 'Original-Order', reference: 'Original urgent order', status: 'pending', priority: 'urgent', lines: [], created_at: '2026-10-20T10:00:00Z' }], receipts: [], exceptions: [{ id: 'Original-Issue', category: 'Observed shortage', status: 'open', created_at: '2026-10-20T09:00:00Z' }], lot_controls: [] });
function model() {
  let now = Date.parse('2026-10-20T12:00:00Z'); const calls = []; let fail; let other = false; let version = 7;
  const client = { origin: scope.origin, warehousePath: id => `/v1/warehouses/${id}`, async session() { calls.push('session'); if (fail === 'session') throw new Error('Synthetic identity read failed'); return other ? { identity: { ...session.identity, actor: 'Other' } } : session; }, async read(path) { calls.push(path); if (fail === 'snapshot') throw new Error('Synthetic snapshot read failed'); return { ...fixture(), version }; } };
  const journal = { async listForIdentity(identity) { assert.deepEqual(identity, scope); calls.push('local journals'); return [{ stage: 'uncertain' }, { stage: 'committed' }]; } };
  const board = new OperationsBoard(client, journal, scope, session, { now: () => now, intervalMs: 5000 });
  return { board, calls, advance(ms) { now += ms; }, fail(value) { fail = value; }, other(value) { other = value; }, version(value) { version = value; } };
}
function device(columns = 49, rows = 20) {
  const input = new PassThrough(); input.isTTY = true; input.isRaw = false; input.setRawMode = value => { input.isRaw = value; };
  let text = ''; const output = new Writable({ write(chunk, _encoding, done) { text += chunk; done(); } }); output.isTTY = true; output.columns = columns; output.rows = rows;
  const ui = new Terminal(input, output, { TERM: 'xterm-ghostty', NO_COLOR: '1' });
  return { ui, input, frames: () => text.split('\x1b[2J\x1b[H').slice(1).map(frame => frame.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '')) };
}
const settle = () => new Promise(setImmediate);

test('automatic board reads respect interval, typing and pause, and coalesce concurrent refresh calls', async () => {
  const m = model(); const first = m.board.refresh(); assert.equal(m.board.refresh(), first); await first;
  assert.equal(m.board.pending.count, 1); const original = m.board.snapshot;
  m.advance(5000); await m.board.tick({ editing: true }); assert.equal(m.board.snapshot, original);
  m.board.togglePause(); await m.board.tick(); assert.equal(m.board.snapshot, original);
  m.board.togglePause(); m.version(8); await m.board.tick(); assert.equal(m.board.snapshot.version, 8);
  await m.board.tick(); assert.equal(m.calls.filter(call => call.startsWith('/v1/')).length, 2);
  assert.ok(m.board.context().detailsIntro.some(line => line.includes('1 pending original request')));
});
test('failed refreshes retain the last snapshot, mark stale, expose unknown counts and recover explicitly', async () => {
  const m = model(); await m.board.refresh(); const original = m.board.snapshot;
  m.fail('snapshot'); m.advance(5000); await m.board.tick();
  assert.equal(m.board.snapshot, original); assert.ok(m.board.context().intro.some(line => line.startsWith('STALE:')));
  assert.equal(laneFor('a', m.board.queueSnapshot).count, null);
  m.fail(undefined); m.version(9); await m.board.refresh(); assert.equal(m.board.snapshot.version, 9);
  assert.ok(!m.board.context().intro.some(line => line.startsWith('STALE:')));
  m.board.togglePause(); m.advance(31000); assert.ok(m.board.context().intro.some(line => line.includes('PAUSED') && line.includes('STALE')));
});
test('identity changes stop automatic reads and keep local originals scoped to the verified operator', async () => {
  const m = model(); await m.board.refresh(); m.other(true); m.advance(5000); await m.board.tick();
  const calls = m.calls.length; assert.ok(m.board.sessionError); assert.equal(m.board.session.identity.actor, scope.actor);
  m.advance(100000); await m.board.tick(); assert.equal(m.calls.length, calls, 'held sessions require manual retry');
  assert.ok(m.board.context().intro.some(line => line.startsWith('ACTION: Session held')));
  m.other(false); await m.board.refresh(); assert.equal(m.board.sessionError, '');
});
test('board lane shortcuts and attention age use original status, location types and timestamps', () => {
  const snapshot = fixture();
  assert.equal(laneFor('a', snapshot).count, 1); assert.equal(laneFor('b', snapshot).count, 1);
  assert.equal(laneFor('e', snapshot).kind, 'exceptions'); assert.equal(laneFor('u', snapshot).count, 1);
  const lines = attentionLines(snapshot, Date.parse('2026-10-20T12:00:00Z'));
  assert.ok(lines[0].includes('Original-Issue') && lines[0].includes('age 3h'));
  assert.ok(lines[1].includes('Original urgent order') && lines[1].includes('age 2h'));
  snapshot.orders[0].created_at = 'invalid'; assert.ok(attentionLines(snapshot, 0).some(line => line.includes('unknown')));
  for (const value of ['1', '4', '301', '-1', '1.5', '01', 'no']) assert.throws(() => refreshInterval(value));
  assert.equal(refreshInterval(), 15000); assert.equal(refreshInterval('0'), 0); assert.equal(refreshInterval('5'), 5000);
});

test('home clock ticks retain unfinished input, selection and context; refresh callbacks stop at exit', async () => {
  const io = device(); let ticks = 0; let paused = false;
  try {
    const menu = io.ui.menu('Operations', [['Other', 'other'], ['Original inventory', 'inventory']], {
      tickMs: 10, onTick: ({ editing }) => { ticks++; assert.ok(editing); },
      context: () => ({ intro: ['Operator: Original', paused ? 'Paused' : 'Current'], compactIntro: ['Original'], detailsIntro: ['Complete original context'] }),
      localActions: { z: () => { paused = true; } },
    });
    io.input.write('/Orig'); await new Promise(resolve => setTimeout(resolve, 25));
    io.input.write('inal\n\n'); assert.equal(await menu, 'inventory');
    const atExit = ticks; await new Promise(resolve => setTimeout(resolve, 25)); assert.equal(ticks, atExit);
    assert.ok(ticks > 0);
    io.input.write('z\nq\n'); assert.equal(await io.ui.menu('Pause', [['Original', 'original']], { localActions: { z: () => { paused = true; } } }), null); assert.equal(paused, true);
  } finally { io.ui.close(); }
  assert.equal(io.input.isRaw, false);
});

test('compact stock and lot tables retain exact quantities and availability without numeric ellipsis', async () => {
  const stock = fixture().stocks[0];
  const lot = { sku: 'Original', lot: 'Original lot', availability: 'quarantined', physical: 123456, reserved: 23, usable: 0 };
  for (const columns of [20, 34, 48, 69, 119]) for (const large of [false, true]) {
    const quantity = large ? Number.MAX_SAFE_INTEGER : 123456;
    const stocks = tableLines('stocks', [{ ...stock, on_hand: quantity }], columns);
    const lots = tableLines('lots', [{ ...lot, physical: quantity }], columns);
    for (const lines of [stocks, lots]) {
      assert.ok(lines.every(line => width(line) <= columns)); assert.ok(lines.some(line => line.includes(String(quantity))));
      assert.ok(lines.some(line => line.includes('23')));
    }
    assert.ok(lots.some(line => line.includes('quarantined')));
  }
  for (const kind of ['stocks', 'lots']) {
    const io = device(); io.input.write('q\n');
    try { await inspectRows(io.ui, kind, async () => ({ title: 'Original', version: 7, warehouseId: 'Original-Facility', rows: [kind === 'stocks' ? stock : lot] })); }
    finally { io.ui.close(); }
    assert.ok(io.frames().every(frame => frame.trimEnd().split('\n').length <= 20));
    assert.ok(io.frames()[0].includes('123456') && io.frames()[0].includes('23'));
  }
});

test('home board bounds compact and wide layouts while retaining recovery and full original context', async () => {
  const m = model(); await m.board.refresh();
  for (const [columns, rows] of [[49, 20], [70, 24], [120, 32], [70, 44]]) {
    const io = device(columns, rows); io.input.write('v\n' + 'n\n'.repeat(30) + '\nq\n');
    try { await io.ui.menu('WMS operations center', Array.from({ length: 20 }, (_, n) => [`Original action ${n+1}`, n+1]), { context: () => m.board.context({ columns: io.ui.columns }), compactHint: ['r refresh · z pause · v info · q'] }); }
    finally { io.ui.close(); }
    assert.ok(io.frames().every(frame => frame.trimEnd().split('\n').length <= rows), `${columns}x${rows} overflow`);
    assert.ok(io.frames()[0].includes('ACTION:')); assert.ok(io.frames().some(frame => frame.includes('Original-Tenant')));
  }
});

test('row selection is visible without colors and retains a three-digit original row number', async () => {
  const io = device(); const rows = Array.from({ length: 100 }, (_, n) => ({ ...fixture().stocks[0], sku: `Original-${n}` }));
  try {
    const view = inspectRows(io.ui, 'stocks', async () => ({ title: 'Original inventory', version: 7, rows }));
    await settle(); io.input.write('\x1b[F'); await settle();
    assert.ok(io.frames().at(-1).includes('› 100'), 'selection marker cannot replace a digit of the original row number');
    io.input.write('q\n'); await view;
  } finally { io.ui.close(); }
});
