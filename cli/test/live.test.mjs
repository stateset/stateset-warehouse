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
import { rowWork } from '../lib/workflows.mjs';
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
  async function driveTerminal(steps, filename, { expectCommit = true, warehouse = scope.warehouse } = {}) {
    const terminal = spawn('script', ['-qefc', `stty cols 70 rows 44; exec ${shell(process.execPath)} ${shell(path.join(repo, 'cli/warehouse-tui.mjs'))}`, '/dev/null'], { env: { ...env, WMS_URL: origin, WMS_TOKEN: token, WMS_WAREHOUSE: warehouse, WMS_TUI_STATE_DIR: journal.root, TERM: 'xterm' }, detached: true, stdio: ['pipe', 'pipe', 'pipe'] });
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
  const moveTranscript = await driveTerminal([['Select: ', '1'], ['Select: ', '2'], ['sku: ', 'TUI-PART'], ['lot: ', 'UNTRACKED'], ['from: ', 'A'], ['to: ', 'B'], ['quantity (whole each): ', '1.5'], ['quantity (whole each): ', '1'], ['Scan actual TUI-PART [] label 1/1: ', 'Replace-before-save'], ['Enter to confirm: ', { keys: '\x1bOQ' }], ['Scan actual TUI-PART [] label 1/1: ', 'original'], ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['Select: ', 'q']], 'terminal.txt');
  assert.ok(moveTranscript.includes('Enter a whole each quantity from 1 to 1000000.'));
  assert.ok(moveTranscript.includes('F2 correct last observation before submission'));
  const correctedMove = (await journal.listForIdentity(scope)).find(row => row.request.serial_inputs?.some(input => input.serials.includes('original')));
  assert.ok(correctedMove);
  assert.ok(!correctedMove.original.body.includes('Replace-before-save'), 'only corrected actual observations enter the original durable request');
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
    ['quantity (whole each): ', '2'], ['location: ', 'RECV-01'],
    ['Scan actual TUI-PART [] label 1/2: ', 'Guided-Serial'],
    ['Scan actual TUI-PART [] label 2/2: ', 'Guided-Serial'],
    ['Scan actual TUI-PART [] label 2/2: ', 'Guided-Spare'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'],
    ['Enter to continue: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'guided-receiving-terminal.txt');
  assert.ok(receivingTranscript.includes(`Selected receipt_id: ${selectedReceipt.id}`));
  assert.ok(receivingTranscript.includes('Selected line_id: Original-LINE'));
  assert.ok(receivingTranscript.includes('Duplicate literal serial observation for this SKU'));
  assert.ok(receivingTranscript.includes('Captured 1/2 labels'));
  const receivingRecord = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'receive');
  assert.equal(receivingRecord.stage, 'committed');
  assert.equal(receivingRecord.request.expected_version, selectedSnapshot.version);
  assert.deepEqual(receivingRecord.request.command, { type: 'receive', receipt_id: selectedReceipt.id, line_id: 'Original-LINE', quantity: 2, location: 'RECV-01' });
  assert.deepEqual(receivingRecord.request.serial_inputs, [{ target: [], sku: 'TUI-PART', serials: ['Guided-Serial', 'Guided-Spare'] }]);
  const afterReceiving = await client.read(client.warehousePath(scope.warehouse));
  assert.equal(afterReceiving.version, selectedSnapshot.version + 1);
  assert.equal(afterReceiving.receipts.find(row => row.id === selectedReceipt.id).lines[0].received, 2);
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
  assert.equal(afterGuided.stocks.find(row => row.location === 'RECV-01').on_hand, 1);
  const guidedCustody = await client.read('/v1/serials?sku=TUI-PART&warehouse_id=wh-main&limit=100');
  assert.equal(guidedCustody.items.find(row => row.serial === 'Guided-Serial').place.location, 'B');
  assert.equal(guidedCustody.items.find(row => row.serial === 'Guided-Spare').place.location, 'RECV-01');
  assert.equal(guidedCustody.items.find(row => row.serial === 'Third').place.location, 'A');
  assert.equal((await client.read(client.warehousePath(scope.warehouse, '/reconciliation'))).ok, true);
  // Real queues retain original identities across filtering, refresh and sorting.
  for (const priority of ['normal', 'high', 'urgent']) {
    const state = await client.read(client.warehousePath(scope.warehouse));
    const result = await client.call('POST', client.warehousePath(scope.warehouse, '/commands'), {
      request_id: randomUUID(), expected_version: state.version,
      command: { type: 'create_order', reference: `QUEUE-${priority.toUpperCase()}`, source_order_id: `synthetic:queue:${priority}`, customer: 'Synthetic customer', destination: 'Synthetic destination', priority, lines: [{ id: `Original-${priority}`, sku: 'TUI-PART', quantity: 1 }] },
    });
    assert.equal(result.status, 200, JSON.stringify(result.data));
  }
  const queueSnapshot = await client.read(client.warehousePath(scope.warehouse));
  const highOrder = queueSnapshot.orders.find(row => row.reference === 'QUEUE-HIGH');
  const urgentOrder = queueSnapshot.orders.find(row => row.reference === 'QUEUE-URGENT');
  const queueRecords = (await journal.listForIdentity(scope)).length;
  const queueTranscript = await driveTerminal([
    ['Select: ', 't'], ['Select: ', '1'], ['View: ', '1'],
    ['n next | p previous | Enter back: ', ''],
    ['View: ', { keys: '\x1bOR' }], ['Select: ', '7'],
    ['View: ', '/URGENT'], ['View: ', 'r'], ['View: ', '/'],
    ['View: ', { keys: '\x1bOS' }], ['Select: ', '3'],
    ['View: ', '1'], ['n next | p previous | Enter back: ', ''],
    ['View: ', ''], ['Select: ', 'q'],
  ], 'queues-read-only-terminal.txt', { expectCommit: false });
  const queueDetails = queueTranscript.split('"reference": ').slice(1).map(body => body.split('\r')[0]);
  assert.ok(queueDetails[0].startsWith('"QUEUE-URGENT"'), 'open queue starts with the most urgent order');
  assert.ok(queueDetails.at(-1).startsWith('"QUEUE-HIGH"'), 'reference sorting changes the displayed first record');
  assert.ok(queueTranscript.includes('of 2 rows · 3 total'));
  assert.equal((await client.read(client.warehousePath(scope.warehouse))).version, queueSnapshot.version);
  assert.equal((await journal.listForIdentity(scope)).length, queueRecords);
  const queuedAction = await driveTerminal([
    ['Select: ', 't'], ['Select: ', '6'], ['View: ', { keys: '\x1b[B' }],
    ['View: ', { keys: '\x1bOS' }], ['Select: ', '3'], ['View: ', 'a'],
    ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', '2'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'],
    ['Enter to continue: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'queue-sorted-action-terminal.txt');
  assert.ok(queuedAction.includes('Highlight a row with the arrow keys'));
  assert.ok(queuedAction.includes(`Selected order_id: ${highOrder.id}`));
  const allocated = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'allocate_available');
  assert.equal(allocated.stage, 'committed');
  assert.equal(allocated.request.expected_version, queueSnapshot.version);
  assert.deepEqual(allocated.request.command, { type: 'allocate_available', order_id: highOrder.id });
  const afterQueue = await client.read(client.warehousePath(scope.warehouse));
  assert.equal(afterQueue.version, queueSnapshot.version + 1);
  assert.equal(afterQueue.orders.find(row => row.id === highOrder.id).status, 'allocated');
  assert.equal(afterQueue.orders.find(row => row.id === urgentOrder.id).status, 'pending');
  assert.deepEqual(afterQueue.stocks.map(({ reserved, ...row }) => row), queueSnapshot.stocks.map(({ reserved, ...row }) => row), 'allocation does not move physical stock');
  assert.deepEqual(await client.read('/v1/serials?sku=TUI-PART&warehouse_id=wh-main&limit=100'), guidedCustody, 'allocation does not change physical serial custody');
  assert.equal((await client.read(client.warehousePath(scope.warehouse, '/reconciliation'))).ok, true);
  // Exercise a real cursor boundary containing quotes, non-ASCII and URL
  // delimiters. These fixtures are isolated in the new synthetic facility.
  async function seed(warehouse, command, serial_inputs = []) {
    const state = await client.read(client.warehousePath(warehouse));
    const result = await client.call('POST', client.warehousePath(warehouse, '/commands'), { request_id: randomUUID(), expected_version: state.version, command, ...(serial_inputs.length ? { serial_inputs } : {}) });
    assert.equal(result.status, 200, JSON.stringify(result.data));
  }
  const pagedProduct = { type: 'create_product', sku: 'PAGED', name: 'Synthetic paged stock', category: 'Synthetic', reorder_point: 0, serial_controlled: false };
  await seed(newId, pagedProduct); await seed(scope.warehouse, pagedProduct);
  const bin = 'PAGED-STORAGE';
  await seed(newId, { type: 'create_location', code: bin, zone: 'Synthetic pagination', kind: 'storage' });
  const lotAt = index => `Lot / " É & ? #${String(index).padStart(3, '0')}`;
  for (let index = 0; index < 101; index++) await seed(newId, { type: 'count_stock', sku: 'PAGED', lot: lotAt(index), location: bin, counted: 1, reason: 'Synthetic pagination boundary fixture' });
  await seed(newId, { type: 'quarantine_lot', sku: 'PAGED', lot: lotAt(100), reason: 'Synthetic held lot in second batch' });
  await seed(newId, { type: 'dispatch_transfer', reference: 'PAGED-SETTLED', destination_warehouse_id: scope.warehouse, carrier: 'Synthetic carrier', handoff_reference: 'Synthetic original handoff', lines: [{ id: 'Original-Transfer-Line', sku: 'PAGED', lot: lotAt(0), location: bin, quantity: 1 }] });
  const dispatched = await client.read(client.warehousePath(newId, '/transfers?limit=100'));
  const transfer = dispatched.items.find(row => row.data.reference === 'PAGED-SETTLED').data;
  await seed(scope.warehouse, { type: 'receive_transfer', transfer_id: transfer.id, line_id: 'Original-Transfer-Line', quantity: 1, location: 'RECV-01', arrival_reference: 'Synthetic actual arrival' });
  assert.equal((await client.read(client.warehousePath(newId, '/transfers?limit=100'))).items.length, 0, 'settled transfer is excluded by the server default');
  const beforeBrowsing = await client.read(client.warehousePath(newId));
  const mainBeforeBrowsing = await client.read(client.warehousePath(scope.warehouse));
  const recordsBeforeBrowsing = (await journal.listForIdentity(scope)).length;
  const pagedTranscript = await driveTerminal([
    ['Select: ', '9'], ['View: ', '/100'], ['View: ', 'n'],
    ['View: ', { keys: '\x1bOR' }], ['Select: ', '3'],
    ['View: ', 'r'], ['View: ', 'n'], ['View: ', '/'], ['View: ', '1'],
    ['n next | p previous | Enter back: ', ''], ['View: ', 'p'],
    ['View: ', { keys: '\x1bOR' }], ['Select: ', '1'], ['View: ', 'r'],
    ['View: ', ''], ['Select: ', '10'],
    ['View: ', 'f'], ['Select: ', '2'], ['View: ', 'f'], ['Select: ', '3'],
    ['View: ', '1'], ['n next | p previous | Enter back: ', ''],
    ['View: ', ''], ['Select: ', 'q'],
  ], 'paged-lots-and-settled-transfer-terminal.txt', { expectCommit: false, warehouse: newId });
  assert.ok(pagedTranscript.includes('Batch 2 · last batch'));
  assert.ok(pagedTranscript.includes(JSON.stringify(lotAt(100))), 'complete original lot label beyond the first 100 records');
  assert.ok(pagedTranscript.includes('"availability": "quarantined"'));
  assert.ok(pagedTranscript.includes('PAGED-SETTLED'), 'settled transfer remains visible');
  assert.ok(pagedTranscript.includes(`"id": "${transfer.id}"`));
  assert.ok(pagedTranscript.includes('Inbound to this facility') && pagedTranscript.includes('Outbound from this facility'));
  assert.equal((await client.read(client.warehousePath(newId))).version, beforeBrowsing.version, 'paged browsing does not commit source work');
  assert.equal((await client.read(client.warehousePath(scope.warehouse))).version, mainBeforeBrowsing.version, 'paged browsing does not commit destination work');
  assert.equal((await journal.listForIdentity(scope)).length, recordsBeforeBrowsing);
  assert.equal((await client.read(client.warehousePath(newId, '/reconciliation'))).ok, true);
  assert.equal((await client.read(client.warehousePath(scope.warehouse, '/reconciliation'))).ok, true);
  // Report from the exact selected order, then investigate and resolve from
  // the open-exceptions queue. Declining confirmation creates no request.
  const beforeIssue = await client.read(client.warehousePath(scope.warehouse));
  const issueOrder = beforeIssue.orders.find(row => row.id === highOrder.id);
  const reportIndex = rowWork('orders', issueOrder, 'admin').findIndex(spec => spec.type === 'report_exception') + 2;
  assert.ok(reportIndex > 1);
  const reportTranscript = await driveTerminal([
    ['Select: ', 'o'], ['View: ', '/QUEUE-HIGH'], ['View: ', { keys: '\x1b[B' }],
    ['View: ', 'a'], ['Select: ', String(reportIndex)],
    ['category: ', 'TUI-LINKED'], ['detail: ', 'Synthetic observed issue requiring original-order review'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'],
    ['Enter to continue: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'order-exception-report-terminal.txt');
  assert.ok(reportTranscript.includes(`Selected order_id: ${issueOrder.id}`));
  const reported = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'report_exception');
  assert.equal(reported.stage, 'committed');
  assert.equal(reported.request.expected_version, beforeIssue.version);
  assert.deepEqual(reported.request.command, { type: 'report_exception', order_id: issueOrder.id, category: 'TUI-LINKED', detail: 'Synthetic observed issue requiring original-order review' });
  const afterIssue = await client.read(client.warehousePath(scope.warehouse));
  const issue = afterIssue.exceptions.find(row => row.category === 'TUI-LINKED');
  assert.ok(issue); assert.equal(issue.order_id, issueOrder.id); assert.equal(issue.status, 'open');
  assert.equal(afterIssue.version, beforeIssue.version + 1);
  const issueRecords = (await journal.listForIdentity(scope)).length;
  const resolvedTranscript = await driveTerminal([
    ['Select: ', 't'], ['Select: ', '9'], ['View: ', { keys: '\x1b[B' }],
    ['View: ', 'a'], ['Select: ', '1'], ['n next | p previous | Enter back: ', ''],
    ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', '3'],
    ['n next | p previous | Enter back: ', ''],
    ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', '2'],
    ['resolution: ', 'Do not send this draft'], ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'no'],
    ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', '2'],
    ['resolution: ', 'Synthetic investigation completed against original order'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'],
    ['Enter to continue: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'exception-linked-review-and-resolution-terminal.txt');
  assert.ok(resolvedTranscript.includes(`"id": "${issue.id}"`));
  assert.ok(resolvedTranscript.includes(`"id": "${issueOrder.id}"`), 'linked review opens the original order');
  assert.ok(resolvedTranscript.includes(`Selected exception_id: ${issue.id}`));
  assert.ok(resolvedTranscript.includes('No rows in this filter.'), 'resolved issue leaves the open queue');
  const issueJournal = await journal.listForIdentity(scope);
  const resolution = issueJournal.find(row => row.request.command.type === 'resolve_exception');
  assert.equal(issueJournal.length, issueRecords + 1, 'declined draft and linked reviews create no journal');
  assert.equal(resolution.stage, 'committed');
  assert.equal(resolution.request.expected_version, afterIssue.version);
  assert.deepEqual(resolution.request.command, { type: 'resolve_exception', exception_id: issue.id, resolution: 'Synthetic investigation completed against original order' });
  assert.ok(!issueJournal.some(row => row.original.body.includes('Do not send this draft')));
  const afterResolution = await client.read(client.warehousePath(scope.warehouse));
  assert.equal(afterResolution.version, afterIssue.version + 1);
  assert.equal(afterResolution.exceptions.find(row => row.id === issue.id).status, 'resolved');
  assert.deepEqual(afterResolution.stocks, beforeIssue.stocks, 'reporting and resolving do not change physical stock or reservations');
  assert.deepEqual(afterResolution.orders, beforeIssue.orders, 'investigation does not rewrite original fulfillment work');
  assert.equal((await client.read(client.warehousePath(scope.warehouse, '/reconciliation'))).ok, true);
  // Receive an actual serial-controlled transfer through its selected original
  // row/line. Source-side inspection cannot expose destination receiving.
  await seed(newId, { type: 'create_product', sku: 'TUI-PART', name: 'Synthetic terminal part', category: 'Synthetic', reorder_point: 0, serial_controlled: true });
  const sourceBeforeDispatch = await client.read(client.warehousePath(scope.warehouse));
  const sourceBin = sourceBeforeDispatch.stocks.find(row => row.sku === 'TUI-PART' && row.location === 'B');
  assert.ok(sourceBin.on_hand - sourceBin.reserved >= 1);
  await seed(scope.warehouse, { type: 'dispatch_transfer', reference: 'TUI-SERIAL-ARRIVAL', destination_warehouse_id: newId, carrier: 'Synthetic carrier', handoff_reference: 'Synthetic actual departure', lines: [{ id: 'Original-ARRIVAL', sku: 'TUI-PART', lot: 'UNTRACKED', location: 'B', quantity: 1 }] }, [{ target: ['Original-ARRIVAL'], sku: 'TUI-PART', serials: ['Guided-Serial'] }]);
  const arrivalPage = await client.read(client.warehousePath(newId, '/transfers?limit=100&open_only=false'));
  const arriving = arrivalPage.items.find(row => row.data.reference === 'TUI-SERIAL-ARRIVAL').data;
  const beforeArrival = await client.read(client.warehousePath(newId));
  const sourceAfterDispatch = await client.read(client.warehousePath(scope.warehouse));
  const beforeArrivalRecords = (await journal.listForIdentity(scope)).length;
  const sourceReview = await driveTerminal([
    ['Select: ', '10'], ['View: ', '/TUI-SERIAL-ARRIVAL'], ['View: ', { keys: '\x1b[B' }],
    ['View: ', 'a'], ['Select: ', '1'], ['n next | p previous | Enter back: ', ''],
    ['View: ', ''], ['Select: ', 'q'],
  ], 'transfer-source-inspection-terminal.txt', { expectCommit: false });
  assert.ok(!sourceReview.includes('Receive an arrived facility transfer'), 'source facility offers original inspection only');
  assert.equal((await client.read(client.warehousePath(newId))).version, beforeArrival.version);
  assert.equal((await journal.listForIdentity(scope)).length, beforeArrivalRecords);
  const arrivalTranscript = await driveTerminal([
    ['Select: ', '10'], ['View: ', '/TUI-SERIAL-ARRIVAL'], ['View: ', { keys: '\x1b[B' }],
    ['View: ', 'a'], ['Select: ', '2'], ['Select: ', '1'], ['quantity (whole each): ', '1'],
    ['location: ', 'RECV-01'], ['arrival_reference: ', 'Actual synthetic arrival observation'],
    ['Scan actual TUI-PART [] label 1/1: ', 'Guided-Serial'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'],
    ['Enter to continue: ', ''], ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'],
    ['Select: ', '1'], ['n next | p previous | Enter back: ', ''],
    ['View: ', ''], ['Select: ', 'q'],
  ], 'selected-transfer-arrival-terminal.txt', { warehouse: newId });
  assert.ok(arrivalTranscript.includes(`Selected transfer_id: ${arriving.id}`));
  assert.ok(arrivalTranscript.includes('Selected line_id: Original-ARRIVAL'));
  assert.ok(arrivalTranscript.includes('in transit 1 each'));
  const receivedArrival = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'receive_transfer');
  assert.equal(receivedArrival.stage, 'committed');
  assert.equal(receivedArrival.original.scope.warehouse, newId);
  assert.equal(receivedArrival.request.expected_version, beforeArrival.version);
  assert.deepEqual(receivedArrival.request.command, { type: 'receive_transfer', transfer_id: arriving.id, line_id: 'Original-ARRIVAL', quantity: 1, location: 'RECV-01', arrival_reference: 'Actual synthetic arrival observation' });
  assert.deepEqual(receivedArrival.request.serial_inputs, [{ target: [], sku: 'TUI-PART', serials: ['Guided-Serial'] }]);
  assert.equal((await journal.listForIdentity(scope)).length, beforeArrivalRecords + 1);
  const afterArrival = await client.read(client.warehousePath(newId));
  assert.equal(afterArrival.version, beforeArrival.version + 1);
  assert.equal(afterArrival.stocks.find(row => row.sku === 'TUI-PART' && row.location === 'RECV-01').on_hand, 1);
  const { availability_checked_at: ignoredSourceTime, ...sourceProof } = sourceAfterDispatch;
  const { availability_checked_at: newSourceTime, ...sourceAfterArrival } = await client.read(client.warehousePath(scope.warehouse));
  assert.deepEqual(sourceAfterArrival, sourceProof, 'destination receiving does not rewrite source physical stock or fulfillment');
  const arrivalCustody = await client.read(`/v1/serials?sku=TUI-PART&warehouse_id=${newId}&limit=100`);
  assert.equal(arrivalCustody.items.find(row => row.serial === 'Guided-Serial').place.location, 'RECV-01');
  const settled = (await client.read(client.warehousePath(newId, '/transfers?limit=100&open_only=false'))).items.find(row => row.data.id === arriving.id).data;
  assert.equal(settled.lines[0].received, 1);
  assert.deepEqual(rowWork('transfers', settled, 'admin', newId), [], 'settled transfer exposes no further receive action');
  assert.equal((await client.read(client.warehousePath(newId, '/reconciliation'))).ok, true);
  assert.equal((await client.read(client.warehousePath(scope.warehouse, '/reconciliation'))).ok, true);
  t.diagnostic(`Private synthetic TUI evidence retained: ${root}`);
});
