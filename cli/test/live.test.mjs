import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import * as fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { randomBytes, randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { Client } from '../lib/client.mjs';
import { Journal, sendOriginal } from '../lib/journal.mjs';
import { visibleCommands } from '../lib/commands.mjs';
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const database = process.env.WMS_TEST_DATABASE_URL;
const shell = value => `'${value.replaceAll("'", "'\\''")}'`;

// This test starts only an owned API with synthetic identities on an explicitly
// isolated existing test DB; it never configures native/external integrations.
test('real API/PG terminal work, lost-success recovery, actor fencing and PTY navigation', { skip: !database, timeout: 90000 }, async t => {
  assert.ok(new URL(database).pathname.includes('test'), 'isolated test database name');
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'wms-tui-live-'));
  const token = randomBytes(32).toString('hex'), otherToken = randomBytes(32).toString('hex');
  const tenant = `tui-live-${randomUUID()}`;
  const actor = 'synthetic:tui-original';
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('WMS_') && !k.startsWith('WORKOS_') && k !== 'DATABASE_URL'));
  Object.assign(env, { DATABASE_URL: database, WMS_DEMO: 'false', WMS_BIND: '127.0.0.1:0', WMS_API_KEYS_JSON: JSON.stringify({ [token]: { tenant, actor, role: 'admin' }, [otherToken]: { tenant, actor: 'synthetic:tui-other', role: 'admin' } }), RUST_LOG: 'stateset_warehouse=info', NO_COLOR: '1' });
  const server = spawn(path.join(repo, 'target/debug/stateset-warehouse'), [], { cwd: root, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let log = ''; const capture = data => { log += data.toString(); }; server.stdout.on('data', capture); server.stderr.on('data', capture);
  t.after(async () => {
    if (server.exitCode === null && server.signalCode === null) { server.kill('SIGTERM'); await Promise.race([once(server, 'exit'), new Promise(r => setTimeout(r, 5000).unref())]); }
    if (server.exitCode === null && server.signalCode === null) { server.kill('SIGKILL'); await once(server, 'exit'); }
    // Preserve original requests and server output as private development evidence.
    await fs.writeFile(path.join(root, 'server.log'), log, { mode: 0o600 });
  });
  let origin;
  for (let n = 0; n < 200; n++) {
    const match = log.replace(/\x1b\[[0-9;]*m/g, '').match(/127\.0\.0\.1:(\d+)/);
    if (match) { origin = `http://127.0.0.1:${match[1]}`; break; }
    if (server.exitCode !== null) throw new Error(`Owned API failed to start. Private evidence: ${root}`);
    await new Promise(r => setTimeout(r, 50));
  }
  assert.ok(origin, `Owned API did not become ready. Evidence: ${root}`);
  const client = new Client(origin, token); const journal = new Journal(path.join(root, 'recovery'));
  const session = await client.session(); assert.equal(session.identity.tenant, tenant);
  const scope = { origin, tenant, actor, warehouse: 'wh-main' };
  async function prepare(command, serial_inputs = []) {
    const state = await client.read(client.warehousePath('wh-main'));
    return journal.prepare(scope, { request_id: randomUUID(), expected_version: state.version, command, ...(serial_inputs.length ? { serial_inputs } : {}) });
  }
  for (const command of [
    { type: 'create_product', sku: 'TUI-PART', name: 'Synthetic terminal part', category: 'Synthetic', reorder_point: 0, serial_controlled: true },
    { type: 'create_location', code: 'A', zone: 'Synthetic', kind: 'storage' },
    { type: 'create_location', code: 'B', zone: 'Synthetic', kind: 'storage' },
    { type: 'count_stock', sku: 'TUI-PART', lot: 'UNTRACKED', location: 'A', counted: 3, reason: 'Synthetic observed opening units' },
  ]) {
    // Seed synthetic fixtures through REST; durable terminal recovery is
    // exercised below for the actual interrupted command and PTY action.
    const snapshot = await client.read(client.warehousePath('wh-main'));
    const serial_inputs = command.type === 'count_stock' ? [{ target: [], sku: 'TUI-PART', serials: ['Original', 'original', 'Third'] }] : [];
    const response = await client.call('POST', client.warehousePath('wh-main', '/commands'), { request_id: randomUUID(), expected_version: snapshot.version, command, ...(serial_inputs.length ? { serial_inputs } : {}) });
    assert.equal(response.status, 200, JSON.stringify(response.data));
  }
  const original = await prepare({ type: 'move_stock', sku: 'TUI-PART', lot: 'UNTRACKED', from: 'A', to: 'B', quantity: 1 }, [{ target: [], sku: 'TUI-PART', serials: ['Original'] }]);
  const lossy = new Client(origin, token, { fetchImpl: async (url, options) => {
    const response = await fetch(url, options);
    if (options.method === 'POST' && url.pathname.endsWith('/commands')) { assert.ok(response.ok); await response.text(); throw new Error('Synthetic response lost after real commit'); }
    return response;
  } });
  const uncertain = await sendOriginal(lossy, journal, original); assert.equal(uncertain.stage, 'uncertain');
  const committedVersion = (await client.read(client.warehousePath('wh-main'))).version;
  await assert.rejects(sendOriginal(new Client(origin, otherToken), journal, uncertain), /different server, tenant or actor/);
  const recovered = await sendOriginal(client, new Journal(journal.root), uncertain);
  assert.equal(recovered.stage, 'committed'); assert.equal(recovered.events.at(-1).details.response.replayed, true);
  assert.equal((await client.read(client.warehousePath('wh-main'))).version, committedVersion);
  const onceResult = execFileSync(process.execPath, [path.join(repo, 'cli/warehouse-tui.mjs'), '--once', 'inventory'], { env: { ...env, WMS_URL: origin, WMS_TOKEN: token, WMS_TUI_STATE_DIR: journal.root }, encoding: 'utf8' });
  assert.equal(JSON.parse(onceResult).version, committedVersion); assert.ok(!onceResult.includes(token));

  // The script utility supplies a real pseudoterminal. Drive actual prompts,
  // including a typed physical move; the server verifies the resulting state.
  async function driveTerminal(steps, filename, { expectCommit = true } = {}) {
    const terminal = spawn('script', ['-qefc', `stty cols 70 rows 44; exec ${shell(process.execPath)} ${shell(path.join(repo, 'cli/warehouse-tui.mjs'))}`, '/dev/null'], { env: { ...env, WMS_URL: origin, WMS_TOKEN: token, WMS_TUI_STATE_DIR: journal.root, TERM: 'xterm' }, detached: true, stdio: ['pipe', 'pipe', 'pipe'] });
    let transcript = ''; let position = 0; let handled = 0;
    const drive = chunk => {
      transcript += chunk.toString();
      while (handled < steps.length) {
        const [prompt, answer] = steps[handled]; const index = transcript.indexOf(prompt, position); if (index < 0) break;
        position = index + prompt.length; handled++; terminal.stdin.write(typeof answer === 'object' ? answer.keys : `${answer}\n`);
      }
    };
    terminal.stdout.on('data', drive); terminal.stderr.on('data', drive);
    const timer = setTimeout(() => { try { process.kill(-terminal.pid, 'SIGTERM'); } catch {} }, 30000);
    let code;
    try { [code] = await once(terminal, 'exit'); } finally { clearTimeout(timer); terminal.stdin.end(); }
    await fs.writeFile(path.join(root, filename), transcript, { mode: 0o600 });
    assert.equal(code, 0, `Terminal failed; evidence: ${root}`); assert.equal(handled, steps.length, `Incomplete PTY interaction; evidence: ${root}`);
    if (expectCommit) assert.ok(transcript.includes('Recovery status: committed'));
    assert.ok(!transcript.includes(token));
    return transcript;
  }
  const readTranscript = await driveTerminal([
    ['Select: ', 'i'], ['View: ', '/tui-part'], ['View: ', '1'],
    ['n next | p previous | Enter back: ', ''], ['View: ', '/missing'],
    ['View: ', '/'], ['View: ', 'r'], ['View: ', ''],
    ['Select: ', 'o'], ['View: ', ''],
    ['Select: ', '11'], ['Exact SKU: ', 'TUI-PART'], ['Exact serial (empty for page): ', ''], ['n next | p previous | Enter back: ', ''],
    ['Select: ', '12'], ['Exact SKU: ', 'TUI-PART'], ['Exact serial (empty for page): ', 'Original'], ['n next | p previous | Enter back: ', ''],
    ['Select: ', '14'], ['Select: ', '3'], ['n next | p previous | Enter back: ', ''],
    ['Select: ', '15'], ['n next | p previous | Enter back: ', ''],
    ['Select: ', '?'], ['n next | p previous | Enter back: ', ''],
    ['Select: ', 'r'], ['Select: ', 'q'],
  ], 'read-only-terminal.txt', { expectCommit: false });
  assert.ok(readTranscript.includes('On hand 3 ea · Reserved 0 ea'));
  assert.ok(readTranscript.includes('No matches. Enter / to clear the search.'));
  assert.ok(readTranscript.includes('"sku": "TUI-PART"'));
  assert.ok(readTranscript.includes('No records in this snapshot.'));
  assert.equal((await client.read(client.warehousePath('wh-main'))).version, committedVersion, 'overview, search, detail and refresh are read-only');
  const arrowTranscript = await driveTerminal([
    ['Select: ', { keys: '\x1b[B\n' }], ['View: ', { keys: '\x1b[B\n' }],
    ['n next | p previous | Enter back: ', { keys: '\x1b' }],
    ['View: ', { keys: '\x1b' }], ['Select: ', 'q'],
  ], 'arrow-terminal.txt', { expectCommit: false });
  assert.ok(arrowTranscript.includes('Inventory | wh-main | snapshot'));
  assert.ok(arrowTranscript.includes('"sku": "TUI-PART"'));
  assert.ok(arrowTranscript.includes('\x1b[?1049h') && arrowTranscript.includes('\x1b[?1049l'), 'terminal screen restored');
  assert.equal((await client.read(client.warehousePath('wh-main'))).version, committedVersion);
  const moveTranscript = await driveTerminal([['Select: ', '1'], ['Select: ', '2'], ['sku: ', 'TUI-PART'], ['lot: ', 'UNTRACKED'], ['from: ', 'A'], ['to: ', 'B'], ['quantity (whole each): ', '1.5'], ['quantity (whole each): ', '1'], ['Scan actual TUI-PART [] label 1/1: ', 'original'], ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['Select: ', 'q']], 'terminal.txt');
  assert.ok(moveTranscript.includes('Enter a whole each quantity from 1 to 1000000.'));
  const final = await client.read(client.warehousePath('wh-main'));
  assert.equal(final.version, committedVersion + 1);
  assert.equal(final.stocks.find(s => s.location === 'A').on_hand, 1);
  assert.equal(final.stocks.find(s => s.location === 'B').on_hand, 2);
  const custody = await client.read('/v1/serials?sku=TUI-PART&warehouse_id=wh-main&limit=100');
  assert.equal(custody.items.length, 3);
  for (const label of ['Original', 'original']) assert.equal(custody.items.find(row => row.serial === label).place.location, 'B');
  assert.equal(custody.items.find(row => row.serial === 'Third').place.location, 'A');
  assert.equal((await client.read(client.warehousePath('wh-main', '/reconciliation'))).ok, true);
  const recordsBeforeCancel = (await journal.listForIdentity(scope)).length;
  const cancelled = await driveTerminal([
    ['Select: ', '1'], ['Select: ', '2'], ['sku: ', { keys: '\x1b' }],
    ['Select: ', '1'], ['Select: ', '2'], ['sku: ', 'TUI-PART'], ['lot: ', 'UNTRACKED'],
    ['from: ', 'A'], ['to: ', 'B'], ['quantity (whole each): ', '1'],
    ['Scan actual TUI-PART [] label 1/1: ', 'Third'],
    ['Enter to confirm: ', { keys: '\x1b' }], ['Select: ', 'q'],
  ], 'cancel-terminal.txt', { expectCommit: false });
  assert.ok(!cancelled.includes('Record this observed work? Type yes: '), 'Escape from review does not enter confirmation');
  assert.equal((await client.read(client.warehousePath('wh-main'))).version, final.version, 'cancelled form and observed review do not commit');
  assert.equal((await journal.listForIdentity(scope)).length, recordsBeforeCancel, 'cancelled review does not fabricate an original request');
  // Facility creation must target a new identity at version zero. Exercise
  // the actual terminal form, automatic selection and durable original record.
  const createIndex = visibleCommands('admin').findIndex(c => c.type === 'create_warehouse') + 1;
  assert.ok(createIndex > 0);
  const facilityTranscript = await driveTerminal([['Select: ', '1'], ['Select: ', String(createIndex)], ['code: ', 'TUI-NEW'], ['name: ', 'Synthetic new facility'], ['city: ', 'Synthetic City'], ['timezone: ', 'America/Vancouver'], ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['Select: ', 'q']], 'facility-terminal.txt');
  const createdRecord = (await journal.listForIdentity(scope)).find(r => r.request.command.type === 'create_warehouse');
  assert.ok(createdRecord); assert.equal(createdRecord.stage, 'committed');
  assert.equal(createdRecord.request.expected_version, 0);
  const newId = createdRecord.original.scope.warehouse;
  assert.match(newId, /^wh-[0-9a-f-]{36}$/); assert.notEqual(newId, scope.warehouse);
  assert.ok(facilityTranscript.includes(`Facility: ${newId}`), 'successful creation selects its new facility');
  const created = await client.read(client.warehousePath(newId));
  assert.equal(created.version, 1); assert.equal(created.warehouse.id, newId);
  assert.equal(created.warehouse.code, 'TUI-NEW'); assert.equal(created.warehouse.timezone, 'America/Vancouver');
  assert.equal(created.locations.length, 2); assert.equal(created.stocks.length, 0);
  const { availability_checked_at: previousCheck, ...beforeCreation } = final;
  const { availability_checked_at: currentCheck, ...afterCreation } = await client.read(client.warehousePath('wh-main'));
  assert.ok(previousCheck && currentCheck);
  assert.deepEqual(afterCreation, beforeCreation, 'original facility unchanged');
  // Select the original receiving row/line in the terminal. Only identities
  // carry through; actual received quantity, bin and serial are still observed.
  const beforeReceiving = await client.read(client.warehousePath(scope.warehouse));
  const receivingFixture = await client.call('POST', client.warehousePath(scope.warehouse, '/commands'), {
    request_id: randomUUID(), expected_version: beforeReceiving.version,
    command: { type: 'create_receipt', reference: 'TUI-GUIDED', supplier: 'Synthetic supplier', lines: [{ id: 'Original-LINE', sku: 'TUI-PART', lot: 'UNTRACKED', quantity: 2 }] },
  });
  assert.equal(receivingFixture.status, 200, JSON.stringify(receivingFixture.data));
  const selectedSnapshot = await client.read(client.warehousePath(scope.warehouse));
  const selectedReceipt = selectedSnapshot.receipts.find(row => row.reference === 'TUI-GUIDED');
  const receivingTranscript = await driveTerminal([
    ['Select: ', '6'], ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', '1'],
    ['n next | p previous | Enter back: ', ''],
    ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', '2'], ['Select: ', '1'],
    ['quantity (whole each): ', '1'], ['location: ', 'RECV-01'],
    ['Scan actual TUI-PART [] label 1/1: ', 'Guided-Serial'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'],
    ['Enter to continue: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'guided-receiving-terminal.txt');
  assert.ok(receivingTranscript.includes(`Selected receipt_id: ${selectedReceipt.id}`));
  assert.ok(receivingTranscript.includes('Selected line_id: Original-LINE'));
  const receivingRecord = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'receive');
  assert.equal(receivingRecord.stage, 'committed');
  assert.equal(receivingRecord.request.expected_version, selectedSnapshot.version);
  assert.deepEqual(receivingRecord.request.command, { type: 'receive', receipt_id: selectedReceipt.id, line_id: 'Original-LINE', quantity: 1, location: 'RECV-01' });
  assert.deepEqual(receivingRecord.request.serial_inputs, [{ target: [], sku: 'TUI-PART', serials: ['Guided-Serial'] }]);
  const afterReceiving = await client.read(client.warehousePath(scope.warehouse));
  assert.equal(afterReceiving.version, selectedSnapshot.version + 1);
  assert.equal(afterReceiving.receipts.find(row => row.id === selectedReceipt.id).lines[0].received, 1);
  const selectedMove = await driveTerminal([
    ['Select: ', 'i'], ['View: ', '/RECV-01'], ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'],
    ['Select: ', '2'], ['to: ', 'B'], ['quantity (whole each): ', '1'],
    ['Scan actual TUI-PART [] label 1/1: ', 'Guided-Serial'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'],
    ['Enter to continue: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'guided-move-terminal.txt');
  assert.ok(selectedMove.includes('Selected sku: TUI-PART') && selectedMove.includes('Selected from: RECV-01'));
  const afterGuided = await client.read(client.warehousePath(scope.warehouse));
  assert.equal(afterGuided.version, afterReceiving.version + 1);
  assert.equal(afterGuided.stocks.find(row => row.location === 'A').on_hand, 1);
  assert.equal(afterGuided.stocks.find(row => row.location === 'B').on_hand, 3);
  assert.equal(afterGuided.stocks.find(row => row.location === 'RECV-01').on_hand, 0);
  const guidedCustody = await client.read('/v1/serials?sku=TUI-PART&warehouse_id=wh-main&limit=100');
  assert.equal(guidedCustody.items.find(row => row.serial === 'Guided-Serial').place.location, 'B');
  assert.equal(guidedCustody.items.find(row => row.serial === 'Third').place.location, 'A');
  assert.equal((await client.read(client.warehousePath(scope.warehouse, '/reconciliation'))).ok, true);
  t.diagnostic(`Private synthetic TUI evidence retained: ${root}`);
});
