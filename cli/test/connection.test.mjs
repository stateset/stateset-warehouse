import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough, Writable } from 'node:stream';
import { Client } from '../lib/client.mjs';
import { connectSession } from '../lib/connection.mjs';
import { Terminal } from '../lib/terminal.mjs';
import { inspectRows } from '../lib/views.mjs';

const original = { identity: { tenant: 'Original-Tenant', actor: 'Original-Actor', role: 'admin' }, default_warehouse_id: 'Original-Facility' };
function device(columns = 49, rows = 20) {
  const input = new PassThrough(); input.isTTY = true; input.setRawMode = () => {};
  let text = '';
  const output = new Writable({ write(chunk, _encoding, done) { text += chunk; done(); } }); output.isTTY = true; output.columns = columns; output.rows = rows;
  const ui = new Terminal(input, output, { TERM: 'xterm-ghostty', NO_COLOR: '1' });
  return { ui, input, frames: () => text.split('\x1b[2J\x1b[H').slice(1).map(frame => frame.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, '')) };
}
function scripted(answers) {
  const screens = []; let current;
  return { columns: 100, rows: 28, screens,
    screen(title) { current = { title, lines: [] }; screens.push(current); },
    line(text) { current.lines.push(text); }, print(text) { current.lines.push(text); },
    async ask() { assert.ok(answers.length, 'unexpected prompt'); return answers.shift(); },
    async menu() { assert.ok(answers.length, 'unexpected menu'); return answers.shift(); },
  };
}

test('startup outages and expired authentication wait for explicit retry inside the console', async () => {
  for (const status of [401, 503, 'network']) {
    let calls = 0;
    const client = new Client('http://127.0.0.1:8088', 'synthetic-test-token', { fetchImpl: async (_url, options) => {
      assert.equal(options.method, 'GET'); calls++;
      if (calls === 1 && status === 'network') throw new Error('Synthetic disconnected service');
      return new Response(JSON.stringify(calls === 1 ? { error: 'Synthetic session unavailable' } : original), { status: calls === 1 ? status : 200 });
    } });
    const io = device(); io.input.write('r\n');
    try { assert.deepEqual(await connectSession(client, io.ui), original); }
    finally { io.ui.close(); }
    assert.equal(calls, 2);
    assert.ok(io.frames().some(frame => frame.includes('Connection held')));
    assert.ok(io.frames().every(frame => !frame.includes('Inspect local')), 'unknown identities cannot select local requests');
    assert.ok(io.frames().every(frame => frame.trimEnd().split('\n').length <= 20));
  }
});

test('reconnection preserves the original actor and tenant while accepting fresh role changes', async () => {
  for (const key of ['actor', 'tenant']) {
    let calls = 0; let inspected = 0;
    const changed = structuredClone(original); changed.identity[key] = 'Other-Identity';
    const restored = structuredClone(original); restored.identity.role = 'operator';
    const io = device(); io.input.write('c\nr\n');
    try {
      const session = await connectSession({ origin: 'https://example.com', async session() { return ++calls === 1 ? changed : restored; } }, io.ui, { previous: original, inspectRecovery: async () => { inspected++; } });
      assert.deepEqual(session, restored);
    } finally { io.ui.close(); }
    assert.equal(inspected, 1); assert.equal(calls, 2);
    assert.ok(io.frames().some(frame => frame.includes('Connection held')));
    assert.deepEqual(original.identity, { tenant: 'Original-Tenant', actor: 'Original-Actor', role: 'admin' });
  }
});

test('local recovery errors remain held and quitting never retries or sends work', async () => {
  let calls = 0;
  const io = device(); io.input.write('c\nq\n');
  try {
    assert.equal(await connectSession({ origin: 'https://example.com', async session() { calls++; throw new Error('Synthetic offline'); } }, io.ui, { previous: original, inspectRecovery: async () => { throw new Error('Original journal held'); } }), null);
  } finally { io.ui.close(); }
  assert.equal(calls, 1);
  assert.ok(io.frames().at(-1).includes('Recovery check held:'));
});

const rows = Array.from({ length: 20 }, (_, n) => ({ sku: `Original-${String(n).padStart(2, '0')}`, location: 'A', lot: 'Lot', on_hand: n + 1, reserved: 0 }));
test('failed snapshot refresh retains search, filter, sort and page but holds stale actions', async () => {
  const ui = scripted(['/Original', 'f', 'on_hand', 's', 'identity', 'n', 'r', { key: 'down' }, 'a', 'v', '', 'r', 'a', { key: 'down' }, 'a', 'q']);
  let loads = 0; const actions = [];
  await inspectRows(ui, 'stocks', async () => {
    if (++loads === 2) throw new Error('Synthetic refresh outage');
    return { title: `Inventory v${loads}`, version: loads, rows };
  }, { onAction: async (row, version) => { actions.push({ row, version }); } });
  assert.equal(loads, 4);
  assert.equal(actions.length, 1); assert.equal(actions[0].version, 3);
  assert.equal(actions[0].row, rows[0]);
  const bodies = ui.screens.map(screen => screen.lines.join('\n'));
  const held = bodies.findIndex(body => body.includes('STALE'));
  assert.ok(held >= 0);
  assert.ok(bodies[held - 1].match(/(\d+)–\d+ of 20 rows/)[1] !== '1', 'failed refresh occurred beyond the first page');
  assert.ok(bodies[held].match(/(\d+)–\d+ of 20 rows/)[1] !== '1', 'held view does not reset to the first page');
  assert.ok(bodies[held].includes('/Original'));
  assert.ok(bodies[held].includes('Filter:') && bodies[held].includes('Sort:'));
  assert.ok(bodies.some(body => body.includes('Refresh successfully before opening work')));
  assert.ok(bodies.some(body => body.includes('Last successful read:') && body.includes('Synthetic refresh outage')));
  assert.ok(!bodies.at(-1).includes('STALE'));
});

test('a failed post-action read retains the table without repeating the action', async () => {
  const io = device(); io.input.write('\x1b[B' + 'a\na\nr\nq\n');
  let loads = 0; let actions = 0;
  try {
    await inspectRows(io.ui, 'stocks', async () => {
      if (++loads === 2) throw new Error('Synthetic response read failed');
      return { title: 'Original inventory', version: loads, rows };
    }, { onAction: async () => { actions++; } });
  } finally { io.ui.close(); }
  assert.equal(actions, 1); assert.equal(loads, 3);
  assert.ok(io.frames().some(frame => frame.includes('STALE')));
  assert.ok(io.frames().every(frame => frame.trimEnd().split('\n').length <= 20));
});
