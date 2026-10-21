import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import * as fs from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { randomBytes, randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { Client } from '../lib/client.mjs';
import { readConsole, readProduct, readProductImage } from '../lib/console.mjs';
import { Raster } from '../lib/raster.mjs';
import { Journal, sendOriginal } from '../lib/journal.mjs';
import { visibleCommands } from '../lib/commands.mjs';
import { rowWork } from '../lib/workflows.mjs';
import { orderProgress } from '../lib/fulfillment.mjs';
const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const database = process.env.WMS_TEST_DATABASE_URL;
const shell = value => `'${value.replaceAll("'", "'\\''")}'`;

// This test starts only an owned API with synthetic identities on an explicitly
// isolated existing test DB; it never configures native/external integrations.
test('real API/PG terminal work, lost-success recovery, actor fencing and PTY navigation', { skip: !database, timeout: 180000 }, async t => {
  assert.ok(new URL(database).pathname.includes('test'), 'isolated test database name');
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'wms-tui-live-'));
  const token = randomBytes(32).toString('hex'), otherToken = randomBytes(32).toString('hex');
  const tenant = `tui-live-${randomUUID()}`;
  const actor = 'synthetic:tui-original';
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('WMS_') && !k.startsWith('WORKOS_') && k !== 'DATABASE_URL'));
  Object.assign(env, { DATABASE_URL: database, WMS_DEMO: 'false', WMS_BIND: '127.0.0.1:0', WMS_API_KEYS_JSON: JSON.stringify({ [token]: { tenant, actor, role: 'admin' }, [otherToken]: { tenant, actor: 'synthetic:tui-other', role: 'admin' } }), RUST_LOG: 'stateset_warehouse=info', NO_COLOR: '1' });
  await fs.writeFile(path.join(root, 'product.png'), new Raster(16, 12).png());
  const assetsFile = path.join(root, 'console-assets.json');
  await fs.writeFile(assetsFile, JSON.stringify({ schema_version: 1, facilities: [{ tenant, warehouse_id: 'wh-main', layout: { name: 'Synthetic test plan', unit: 'grid', width: 100, height: 50, bins: [{ code: 'A', x: 0, y: 0, width: 40, height: 40 }, { code: 'B', x: 50, y: 0, width: 40, height: 40 }] }, product_images: { 'TUI-PART': 'product.png' } }] }));
  env.WMS_CONSOLE_ASSETS_FILE = assetsFile;
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

  const visual = await readConsole(client, scope); assert.equal(visual.stock.on_hand_each, '3'); assert.equal(visual.floor_plan.available, true);
  const product = await readProduct(client, scope, 'TUI-PART'); assert.equal(product.label.value, 'TUI-PART');
  assert.deepEqual(await readProductImage(client, scope, product), await fs.readFile(path.join(root, 'product.png')));
  await assert.rejects(readProductImage(new Client(origin, otherToken), scope, product), /409/);
  const consoleOnce = execFileSync(process.execPath, [path.join(repo, 'cli/warehouse-tui.mjs'), '--once', 'console'], { env: { ...env, WMS_URL: origin, WMS_TOKEN: token }, encoding: 'utf8' });
  assert.equal(JSON.parse(consoleOnce).version, String(committedVersion)); assert.ok(!consoleOnce.includes('\x1b_G'));

  // The script utility supplies a real pseudoterminal. Drive actual prompts,
  // including a typed physical move; the server verifies the resulting state.
  async function driveTerminal(steps, filename, { expectCommit = true, warehouse = scope.warehouse, columns = 70, rows = 44, url = origin, resizable = false, refreshSeconds = 15, timeoutMs = 30000, graphics = false } = {}) {
    assert.ok(Number.isSafeInteger(columns) && Number.isSafeInteger(rows));
    const ttyFile = path.join(root, `${filename}.tty`);
    const terminal = spawn('script', ['-qefc', `stty cols ${columns} rows ${rows}; ${resizable ? `tty > ${shell(ttyFile)}; ` : ''}exec ${shell(process.execPath)} ${shell(path.join(repo, 'cli/warehouse-tui.mjs'))}`, '/dev/null'], { env: { ...env, WMS_URL: url, WMS_TOKEN: token, WMS_WAREHOUSE: warehouse, WMS_TUI_REFRESH_SECONDS: String(refreshSeconds), WMS_TUI_GRAPHICS: graphics ? 'kitty' : 'off', WMS_TUI_STATE_DIR: journal.root, TERM: 'xterm-ghostty', TERM_PROGRAM: 'ghostty' }, detached: true, stdio: ['pipe', 'pipe', 'pipe'] });
    let transcript = ''; let position = 0; let handled = 0; let driveError;
    const delays = []; let acknowledged = false;
    const drive = chunk => {
      transcript += chunk.toString();
      const query = /\x1b_Ga=q,t=d,f=24,s=1,v=1,i=(\d+);AAAA\x1b\\/.exec(transcript);
      if (graphics && query && !acknowledged) { acknowledged = true; terminal.stdin.write(`\x1b_Gi=${query[1]};OK\x1b\\`); }
      while (handled < steps.length) {
        const [prompt, answer] = steps[handled]; const index = transcript.indexOf(prompt, position); if (index < 0) break;
        position = index + prompt.length; handled++;
        if (typeof answer === 'object' && answer.wait) continue;
        if (typeof answer === 'object' && answer.delayMs) { delays.push(setTimeout(() => terminal.stdin.write(answer.keys), answer.delayMs)); continue; }
        if (typeof answer === 'object' && answer.resize) {
          try {
            assert.ok(resizable); const tty = readFileSync(ttyFile, 'utf8').trim();
            assert.match(tty, /^\/dev\/pts\/\d+$/);
            assert.ok(Number.isSafeInteger(answer.resize.columns) && Number.isSafeInteger(answer.resize.rows));
            // This is the PTY created by this driver, never an operator's pane.
            execFileSync('stty', ['-F', tty, 'cols', String(answer.resize.columns), 'rows', String(answer.resize.rows)]);
          } catch (error) { driveError = error; try { process.kill(-terminal.pid, 'SIGTERM'); } catch {} return; }
        } else terminal.stdin.write(typeof answer === 'object' ? answer.keys : `${answer}\n`);
      }
    };
    terminal.stdout.on('data', drive); terminal.stderr.on('data', drive);
    const timer = setTimeout(() => { try { process.kill(-terminal.pid, 'SIGTERM'); } catch {} }, timeoutMs);
    let code;
    try { [code] = await once(terminal, 'exit'); } finally { clearTimeout(timer); delays.forEach(clearTimeout); terminal.stdin.end(); }
    await fs.writeFile(path.join(root, filename), transcript, { mode: 0o600 });
    if (driveError) throw driveError;
    assert.equal(code, 0, `Terminal failed; evidence: ${root}`); assert.equal(handled, steps.length, `Incomplete PTY interaction; evidence: ${root}`);
    if (expectCommit) assert.ok(transcript.includes('Recovery status: committed'));
    assert.ok(!transcript.includes(token));
    return transcript;
  }
  const graphicsTranscript = await driveTerminal([
    ['Select: ', 'm'], ['Select: ', '1'], ['Action: ', 'b'], ['Action: ', 'm'], ['Action: ', '/A'],
    ['Action: ', 's'], ['Exact product SKU: ', 'TUI-PART'], ['Action: ', 'p'], ['Action: ', 'l'], ['Action: ', 'q'],
    ['Action: ', '?'], ['n next | p previous | Enter back: ', ''], ['Action: ', 'q'], ['Select: ', 'q'],
  ], 'kitty-graphics-terminal.txt', { expectCommit: false, graphics: true });
  assert.match(graphicsTranscript, /a=T,t=d,f=24,o=z/); assert.match(graphicsTranscript, /a=T,t=d,f=100/);
  assert.match(graphicsTranscript, /a=d,d=I,i=\d+,q=2/); assert.doesNotMatch(graphicsTranscript, /a=d,d=[aA][,;]/);
  assert.ok(graphicsTranscript.includes('Synthetic test plan')); assert.ok(graphicsTranscript.includes('Label value: TUI-PART'));
  assert.equal((await client.read(client.warehousePath('wh-main'))).version, committedVersion, 'visual navigation never writes');
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
  // Control a lot beyond the first server batch. Releasing one original hold
  // must preserve the other; after all releases, expiry can still block use.
  const beforeQuality = await client.read(client.warehousePath(newId));
  const originalControl = beforeQuality.lot_controls.find(row => row.sku === 'PAGED' && row.lot === lotAt(100));
  assert.equal(originalControl.holds.length, 1);
  const originalHold = originalControl.holds[0].id;
  const qualityPrefix = [
    ['Select: ', '9'], ['View: ', '/#100'], ['View: ', 'n'],
    ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'],
  ];
  const actualExpiry = '2020-02-29T12:00:00.123456-07:00';
  const qualityTranscript = await driveTerminal([
    ...qualityPrefix, ['Select: ', '2'], ['reason: ', 'Actual separate synthetic quality issue'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''],
    ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', '3'],
    ['expires_at: ', '2027-02-29T00:00:00Z'], ['expires_at: ', actualExpiry], ['reason: ', 'Actual original expiry label'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''],
    ['View: ', ''], ['Select: ', 'q'],
  ], 'selected-lot-quarantine-and-expiry-terminal.txt', { warehouse: newId });
  assert.ok(qualityTranscript.includes(`Selected lot: ${lotAt(100)}`));
  assert.ok(qualityTranscript.includes('at most 6 fractional digits'));
  assert.ok(qualityTranscript.includes('Batch 2'));
  const afterQuality = await client.read(client.warehousePath(newId));
  assert.equal(afterQuality.version, beforeQuality.version + 2);
  const controlled = afterQuality.lot_controls.find(row => row.sku === 'PAGED' && row.lot === lotAt(100));
  assert.equal(controlled.holds.length, 2);
  const addedHold = controlled.holds.find(hold => hold.id !== originalHold).id;
  assert.equal(controlled.expires_at, '2020-02-29T19:00:00.123456+00:00');
  const qualityRecords = await journal.listForIdentity(scope);
  const quarantine = qualityRecords.find(row => row.request.command.type === 'quarantine_lot');
  const recordedExpiry = qualityRecords.find(row => row.request.command.type === 'record_lot_expiry');
  assert.equal(quarantine.request.expected_version, beforeQuality.version);
  assert.deepEqual(quarantine.request.command, { type: 'quarantine_lot', sku: 'PAGED', lot: lotAt(100), reason: 'Actual separate synthetic quality issue' });
  assert.equal(recordedExpiry.request.expected_version, beforeQuality.version + 1);
  assert.deepEqual(recordedExpiry.request.command, { type: 'record_lot_expiry', sku: 'PAGED', lot: lotAt(100), expires_at: actualExpiry, reason: 'Actual original expiry label' });
  assert.ok(!qualityRecords.some(row => row.original.body.includes('2027-02-29')));
  const release = async (index, reason, filename) => driveTerminal([
    ...qualityPrefix, ['Select: ', '4'], ['Select: ', String(index)], ['reason: ', reason],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''],
    ['View: ', ''], ['Select: ', 'q'],
  ], filename, { warehouse: newId });
  await release(2, 'Actual disposition of separate issue', 'selected-lot-second-hold-release-terminal.txt');
  const afterOneRelease = await client.read(client.warehousePath(newId));
  assert.deepEqual(afterOneRelease.lot_controls.find(row => row.lot === lotAt(100)).holds.map(hold => hold.id), [originalHold], 'only the explicitly selected original hold is released');
  const qualityPage = async () => client.read(client.warehousePath(newId, `/lots?sku=PAGED&limit=100&after=${encodeURIComponent(JSON.stringify(['PAGED', lotAt(99)]))}`));
  assert.equal((await qualityPage()).items.find(row => row.lot === lotAt(100)).availability, 'quarantined');
  await release(1, 'Actual disposition of original issue', 'selected-lot-original-hold-release-terminal.txt');
  const afterReleases = await client.read(client.warehousePath(newId));
  assert.equal(afterReleases.version, beforeQuality.version + 4);
  assert.deepEqual(afterReleases.lot_controls.find(row => row.lot === lotAt(100)).holds, []);
  assert.equal((await qualityPage()).items.find(row => row.lot === lotAt(100)).availability, 'expired', 'removing quarantine does not override expired availability');
  const correctedExpiry = '2028-02-29T12:34:56.654321-07:00';
  await driveTerminal([
    ...qualityPrefix, ['Select: ', '3'], ['expires_at (optional): ', correctedExpiry], ['reason: ', 'Actual supervisor correction evidence'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''],
    ['View: ', ''], ['Select: ', 'q'],
  ], 'selected-lot-supervisor-expiry-correction-terminal.txt', { warehouse: newId });
  const correction = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'correct_lot_expiry');
  assert.equal(correction.stage, 'committed');
  assert.equal(correction.request.expected_version, beforeQuality.version + 4);
  assert.deepEqual(correction.request.command, { type: 'correct_lot_expiry', sku: 'PAGED', lot: lotAt(100), expires_at: correctedExpiry, reason: 'Actual supervisor correction evidence' });
  assert.equal((await qualityPage()).items.find(row => row.lot === lotAt(100)).availability, 'available', 'availability follows the server verdict after the explicit supervisor correction');
  const releases = (await journal.listForIdentity(scope)).filter(row => row.request.command.type === 'release_lot_hold');
  assert.deepEqual(new Set(releases.map(row => row.request.command.hold_id)), new Set([originalHold, addedHold]));
  for (const record of [quarantine, recordedExpiry, ...releases]) { assert.equal(record.stage, 'committed'); assert.equal(record.original.scope.warehouse, newId); }
  const afterCorrection = await client.read(client.warehousePath(newId));
  assert.equal(afterCorrection.version, beforeQuality.version + 5);
  assert.deepEqual(afterCorrection.stocks, beforeQuality.stocks, 'lot disposition moves no physical stock or reservations');
  assert.deepEqual(afterCorrection.orders, beforeQuality.orders);
  assert.deepEqual((await client.read(`/v1/serials?sku=TUI-PART&warehouse_id=${newId}&limit=100`)).items, arrivalCustody.items, 'lot disposition does not change physical serial custody');
  assert.equal((await client.read(client.warehousePath(newId, '/reconciliation'))).ok, true);
  // Ghostty-sized operations-center flows: actual pack conversion, selected
  // stock dispatch, partial package/handoff and original shipped-line return.
  for (const warehouse of [scope.warehouse, newId]) await seed(warehouse, { type: 'create_product', sku: 'OPS', name: 'Synthetic operations stock', category: 'Synthetic', reorder_point: 0, serial_controlled: false });
  await seed(scope.warehouse, { type: 'count_stock', sku: 'OPS', lot: 'OPS-PACK', location: 'A', counted: 24, reason: 'Actual synthetic opening count' });
  await seed(scope.warehouse, { type: 'set_unit_pack', sku: 'OPS', code: 'BOX', label: 'Observed six-each box', each_per_pack: 6, enabled: true, reason: 'Actual synthetic pack definition' });
  const beforePackMove = await client.read(client.warehousePath(scope.warehouse));
  await driveTerminal([
    ['Select: ', '1'], ['Select: ', '2'], ['sku: ', 'OPS'], ['lot: ', 'OPS-PACK'], ['from: ', 'A'], ['to: ', 'B'],
    ['quantity (whole each): ', 'pack'], ['Scan pack code (EA cancels): ', 'BOX'], ['Observed pack count: ', '2'],
    ['Enter to confirm: ', 'j'], ['n next | p previous | Enter back: ', ''], ['Enter to confirm: ', ''],
    ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['Select: ', 'q'],
  ], 'operations-pack-conversion-terminal.txt', { columns: 49, rows: 20 });
  const converted = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'move_stock' && row.request.command.sku === 'OPS');
  assert.equal(converted.stage, 'committed'); assert.equal(converted.request.expected_version, beforePackMove.version);
  assert.equal(converted.request.command.quantity, 12);
  assert.deepEqual(converted.request.unit_inputs, [{ target: [], sku: 'OPS', pack_code: 'BOX', definition_id: beforePackMove.unit_packs.find(pack => pack.sku === 'OPS').id, pack_quantity: 2, each_per_pack: 6 }]);
  const dispatchState = await client.read(client.warehousePath(scope.warehouse));
  const opsStocks = dispatchState.stocks.filter(row => JSON.stringify(row).toLowerCase().includes('ops-pack'));
  const sourceIndex = opsStocks.findIndex(row => row.location === 'A'); assert.ok(sourceIndex >= 0);
  await driveTerminal([
    ['Select: ', 'i'], ['View: ', '/OPS-PACK'], ['View: ', { keys: '\x1b[B'.repeat(sourceIndex + 1) }], ['View: ', 'a'], ['Select: ', '3'],
    ['reference: ', 'OPS-DISPATCH'], ['Select: ', '1'], ['carrier: ', 'Actual carrier'], ['handoff_reference: ', 'Actual departure'],
    ['id: ', 'Original-OPS-Transfer'], ['quantity (whole each): ', '3'], ['Add another line? (yes/no): ', 'no'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'operations-guided-dispatch-terminal.txt', { columns: 49, rows: 20 });
  const dispatchedWork = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'dispatch_transfer');
  assert.equal(dispatchedWork.stage, 'committed');
  assert.deepEqual(dispatchedWork.request.command.lines, [{ id: 'Original-OPS-Transfer', sku: 'OPS', lot: 'OPS-PACK', location: 'A', quantity: 3 }]);
  assert.equal(dispatchedWork.request.command.destination_warehouse_id, newId);
  await seed(scope.warehouse, { type: 'create_order', reference: 'OPS-ORDER', source_order_id: 'Synthetic-OPS-Order', customer: 'Synthetic customer', destination: 'Synthetic destination', priority: 'normal', lines: [{ id: 'Original-OPS-Line', sku: 'OPS', quantity: 5 }] });
  let opsOrder = (await client.read(client.warehousePath(scope.warehouse))).orders.find(order => order.reference === 'OPS-ORDER');
  await seed(scope.warehouse, { type: 'allocate_available', order_id: opsOrder.id });
  opsOrder = (await client.read(client.warehousePath(scope.warehouse))).orders.find(order => order.id === opsOrder.id);
  const opsAllocation = opsOrder.allocations[0];
  await seed(scope.warehouse, { type: 'pick', order_id: opsOrder.id, line_id: 'Original-OPS-Line', location: opsAllocation.location, lot: opsAllocation.lot, quantity: 5 });
  opsOrder = (await client.read(client.warehousePath(scope.warehouse))).orders.find(order => order.id === opsOrder.id);
  const packIndex = rowWork('orders', opsOrder, 'admin').findIndex(spec => spec.type === 'pack_shipment') + 2;
  await driveTerminal([
    ['Select: ', 'o'], ['View: ', '/OPS-ORDER'], ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', String(packIndex)],
    ['reference: ', 'OPS-PACKAGE'], ['Select: ', '1'], ['quantity (whole each): ', '2'], ['Add another line? (yes/no): ', 'no'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'operations-guided-partial-package-terminal.txt');
  const packedWork = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'pack_shipment');
  assert.equal(packedWork.stage, 'committed'); assert.equal(packedWork.request.command.order_id, opsOrder.id);
  assert.deepEqual(packedWork.request.command.lines, [{ line_id: 'Original-OPS-Line', location: opsAllocation.location, lot: opsAllocation.lot, quantity: 2 }]);
  opsOrder = (await client.read(client.warehousePath(scope.warehouse))).orders.find(order => order.id === opsOrder.id);
  const packageMenu = rowWork('orders', opsOrder, 'admin').length + 2;
  await driveTerminal([
    ['Select: ', 'o'], ['View: ', '/OPS-ORDER'], ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', String(packageMenu)],
    ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', '3'], ['carrier: ', 'Actual parcel carrier'], ['handoff_reference: ', 'Actual original parcel handoff'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['View: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'operations-guided-package-handoff-terminal.txt');
  const handedOff = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'handoff_shipment');
  assert.equal(handedOff.stage, 'committed');
  const packages = await client.read(client.warehousePath(scope.warehouse, `/orders/${opsOrder.id}/shipments`));
  assert.equal(packages.packages[0].id, handedOff.request.command.shipment_id); assert.ok(packages.packages[0].closure);
  opsOrder = (await client.read(client.warehousePath(scope.warehouse))).orders.find(order => order.id === opsOrder.id);
  assert.equal(opsOrder.status, 'partially_shipped');
  const returnIndex = rowWork('orders', opsOrder, 'admin').findIndex(spec => spec.type === 'create_return') + 2;
  const beforeReturn = await client.read(client.warehousePath(scope.warehouse));
  await driveTerminal([
    ['Select: ', 'o'], ['View: ', '/OPS-ORDER'], ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', String(returnIndex)],
    ['reference: ', 'OPS-RETURN'], ['reason: ', 'Actual original return authorization'], ['Select: ', '1'], ['id: ', 'Original-OPS-Return'],
    ['quantity (whole each): ', '1'], ['Add another line? (yes/no): ', 'no'], ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'],
    ['Enter to continue: ', ''], ['View: ', ''], ['Select: ', 'q'],
  ], 'operations-guided-return-terminal.txt');
  const returnedWork = (await journal.listForIdentity(scope)).find(row => row.request.command.type === 'create_return');
  assert.equal(returnedWork.stage, 'committed'); assert.equal(returnedWork.request.command.order_id, opsOrder.id);
  assert.deepEqual(returnedWork.request.command.lines, [{ id: 'Original-OPS-Return', order_line_id: 'Original-OPS-Line', lot: opsAllocation.lot, quantity: 1 }]);
  const afterReturn = await client.read(client.warehousePath(scope.warehouse));
  assert.deepEqual(afterReturn.stocks, beforeReturn.stocks, 'return authorization alone does not move physical goods');
  assert.equal(afterReturn.receipts.find(receipt => receipt.reference === 'OPS-RETURN').return_source.order_id, opsOrder.id);
  assert.equal((await client.read(client.warehousePath(scope.warehouse, '/reconciliation'))).ok, true);
  assert.equal((await client.read(client.warehousePath(newId, '/reconciliation'))).ok, true);
  // Interrupt real console reads through an owned loopback proxy. Only session
  // and inventory reads fail; no write or receipt lookup is allowed through it.
  let sessionReads = 0; let inventoryReads = 0; const proxied = [];
  const proxy = createServer(async (request, response) => {
    proxied.push({ method: request.method, path: request.url });
    try {
      if (request.method !== 'GET' || request.url.includes('/commands/')) {
        response.writeHead(405, { 'content-type': 'application/json' }); response.end(JSON.stringify({ error: 'Synthetic read-only outage proxy' })); return;
      }
      const sessionFault = request.url === '/v1/session' && [1, 3].includes(++sessionReads);
      const inventoryFault = request.url === client.warehousePath(scope.warehouse) && ++inventoryReads === 3;
      if (sessionFault || inventoryFault) {
        response.writeHead(503, { 'content-type': 'application/json' }); response.end(JSON.stringify({ error: 'Synthetic connection interruption' })); return;
      }
      const upstream = await fetch(new URL(request.url, origin), { headers: { authorization: request.headers.authorization }, redirect: 'error' });
      response.writeHead(upstream.status, { 'content-type': 'application/json' }); response.end(await upstream.text());
    } catch {
      response.writeHead(502, { 'content-type': 'application/json' }); response.end(JSON.stringify({ error: 'Synthetic proxy upstream unavailable' }));
    }
  });
  await new Promise(resolve => proxy.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => { proxy.close(resolve); proxy.closeAllConnections(); }));
  const proxyOrigin = `http://127.0.0.1:${proxy.address().port}`;
  const pending = await journal.prepare({ ...scope, origin: proxyOrigin }, { request_id: randomUUID(), expected_version: (await client.read(client.warehousePath(scope.warehouse))).version, command: { type: 'move_stock', sku: 'OPS', lot: 'OPS-PACK', from: 'A', to: 'B', quantity: 1 } });
  const beforeOutage = await client.read(client.warehousePath(scope.warehouse));
  const pendingBefore = await journal.load(pending.dir);
  const outageTranscript = await driveTerminal([
    ['Select: ', 'r'], ['Select: ', 'i'], ['View: ', '/OPS-PACK'],
    ['View: ', 'r'], ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'],
    ['View: ', 'r'], ['View: ', 'q'],
    ['Select: ', 'c'], ['Select: ', `/${pending.request.request_id}`], ['Select: ', ''],
    ['n next | p previous | Enter back: ', ''], ['Select: ', 'r'], ['Select: ', 'q'],
  ], 'connection-outage-terminal.txt', { expectCommit: false, columns: 49, rows: 20, url: proxyOrigin });
  assert.ok(outageTranscript.includes('Connection held') && outageTranscript.includes('STALE'));
  assert.ok(outageTranscript.includes('Refresh successfully'));
  assert.equal(sessionReads, 4); assert.equal(inventoryReads, 5);
  assert.ok(proxied.every(request => request.method === 'GET' && !request.path.includes('/commands/')));
  const { availability_checked_at: beforeCheck, ...beforeState } = beforeOutage;
  const { availability_checked_at: afterCheck, ...afterState } = await client.read(client.warehousePath(scope.warehouse));
  assert.ok(beforeCheck && afterCheck, 'availability is freshly evaluated on each read');
  assert.deepEqual(afterState, beforeState, 'outage/recovery inspection records no physical work');
  assert.deepEqual(await journal.load(pending.dir), pendingBefore, 'local recovery inspection never changes original checkpoints');
  await fs.writeFile(path.join(root, 'connection-outage-requests.json'), JSON.stringify(proxied, null, 2), { mode: 0o600 });
  const beforeResize = await client.read(client.warehousePath(scope.warehouse));
  const requestsBeforeResize = await journal.listForIdentity(scope);
  const resizeTranscript = await driveTerminal([
    ['Select: ', { keys: '/Inven' }], ['/Inven', { resize: { columns: 49, rows: 20 } }],
    ['Select: ', { keys: 'tory\n' }], ['Select: ', ''],
    ['View: ', { keys: '/OPS-PA' }], ['/OPS-PA', { resize: { columns: 70, rows: 24 } }],
    ['View: ', { keys: 'CK\n' }], ['View: ', '1'],
    ['n next | p previous | Enter back: ', ''], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'pane-resize-terminal.txt', { expectCommit: false, resizable: true });
  assert.ok(resizeTranscript.includes('Search: Inventory'));
  assert.ok(resizeTranscript.includes('"sku": "OPS"') && resizeTranscript.includes('"lot": "OPS-PACK"'));
  const { availability_checked_at: resizeCheck, ...afterResize } = await client.read(client.warehousePath(scope.warehouse));
  const { availability_checked_at: originalCheck, ...resizeState } = beforeResize;
  assert.ok(resizeCheck && originalCheck); assert.deepEqual(afterResize, resizeState);
  assert.deepEqual(await journal.listForIdentity(scope), requestsBeforeResize, 'pane resizing and read-only navigation never prepare or change original requests');
  let boardSessions = 0; let boardReads = 0; const boardRequests = [];
  const monitor = createServer(async (request, response) => {
    boardRequests.push({ method: request.method, path: request.url, at: new Date().toISOString() });
    if (request.method !== 'GET' || request.url.includes('/commands/')) { response.writeHead(405); response.end('{}'); return; }
    if (request.url === '/v1/session') boardSessions++;
    if (request.url === client.warehousePath(scope.warehouse) && ++boardReads === 2) {
      response.writeHead(503, { 'content-type': 'application/json' }); response.end(JSON.stringify({ error: 'Synthetic board interruption' })); return;
    }
    try {
      const upstream = await fetch(new URL(request.url, origin), { headers: { authorization: request.headers.authorization }, redirect: 'error' });
      response.writeHead(upstream.status, { 'content-type': 'application/json' }); response.end(await upstream.text());
    } catch { response.writeHead(502); response.end(JSON.stringify({ error: 'Synthetic monitor unavailable' })); }
  });
  await new Promise(resolve => monitor.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => { monitor.close(resolve); monitor.closeAllConnections(); }));
  const boardTranscript = await driveTerminal([
    ['Select: ', { keys: '/Holding' }], ['/Holding', { wait: true }], ['Select: ', { keys: '\x15' }],
    ['STALE:', { wait: true }], ['age 0s', { keys: 'z\n' }],
    ['PAUSED', { keys: '/Inven' }], ['/Inven', { wait: true }],
    ['Select: ', { keys: 'tory\n' }], ['Select: ', ''],
    ['View: ', { keys: 'q\n', delayMs: 5500 }], ['Select: ', 'q'],
  ], 'automatic-operations-board-terminal.txt', { expectCommit: false, columns: 49, rows: 20, url: `http://127.0.0.1:${monitor.address().port}`, refreshSeconds: 5, timeoutMs: 60000 });
  assert.ok(boardTranscript.includes('Search: Inventory') && boardTranscript.includes('On hand') && boardTranscript.includes('Reserved'));
  assert.equal(boardSessions, 4); assert.equal(boardReads, 5, 'no automatic refresh during paused input or the inventory task view');
  assert.ok(boardRequests.every(request => request.method === 'GET' && !request.path.includes('/commands/')));
  const { availability_checked_at: boardCheck, ...afterBoard } = await client.read(client.warehousePath(scope.warehouse));
  assert.ok(boardCheck); assert.deepEqual(afterBoard, resizeState);
  assert.deepEqual(await journal.listForIdentity(scope), requestsBeforeResize);
  await fs.writeFile(path.join(root, 'automatic-operations-board-requests.json'), JSON.stringify(boardRequests, null, 2), { mode: 0o600 });
  await driveTerminal([
    ['Select: ', 'a'], ['View: ', 'q'], ['Select: ', 'b'], ['View: ', 'q'], ['Select: ', 'e'], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'operations-board-direct-lanes-terminal.txt', { expectCommit: false, columns: 49, rows: 20 });
  // Continue and recover exact same-SKU lines after an original partial handoff.
  const partialSku = 'CONTINUE-PARTIAL'; const partialLot = 'Original-Partial-Lot';
  await seed(scope.warehouse, { type: 'create_product', sku: partialSku, name: 'Synthetic partial continuation', category: 'Synthetic', reorder_point: 0, serial_controlled: false });
  await seed(scope.warehouse, { type: 'count_stock', sku: partialSku, lot: partialLot, location: 'A', counted: 8, reason: 'Actual synthetic opening stock' });
  await seed(scope.warehouse, { type: 'create_order', reference: 'CONTINUE-PARTIAL', source_order_id: 'Original-Partial-Source', customer: 'Synthetic customer', destination: 'Synthetic destination', priority: 'normal', lines: [{ id: 'Original-Partial-L1', sku: partialSku, quantity: 6 }, { id: 'original-partial-l2', sku: partialSku, quantity: 4 }] });
  const partialState = () => client.read(client.warehousePath(scope.warehouse));
  const partialOrder = state => state.orders.find(row => row.reference === 'CONTINUE-PARTIAL');
  let partial = partialOrder(await partialState());
  await seed(scope.warehouse, { type: 'allocate_available', order_id: partial.id });
  await seed(scope.warehouse, { type: 'pick', order_id: partial.id, line_id: 'Original-Partial-L1', location: 'A', lot: partialLot, quantity: 2 });
  await seed(scope.warehouse, { type: 'pack_shipment', order_id: partial.id, reference: 'Original-Partial-Package', lines: [{ line_id: 'Original-Partial-L1', location: 'A', lot: partialLot, quantity: 2 }] });
  const originalPackage = (await client.read(client.warehousePath(scope.warehouse, `/orders/${partial.id}/shipments`))).packages.find(pack => pack.reference === 'Original-Partial-Package');
  await seed(scope.warehouse, { type: 'handoff_shipment', shipment_id: originalPackage.id, carrier: 'Actual synthetic carrier', handoff_reference: 'Original actual two-unit departure' });
  const originalHandoff = (await client.read(client.warehousePath(scope.warehouse, `/orders/${partial.id}/shipments`))).packages.find(pack => pack.id === originalPackage.id);
  async function partialStart(type, home = 'l') {
    const state = await partialState(); const order = partialOrder(state); const work = rowWork('orders', order, 'admin');
    const index = type === 'remaining' ? work.length + 3 : work.findIndex(spec => spec.type === type) + 2;
    assert.ok(type === 'remaining' || index > 1, `missing partial action ${type}`);
    return { state, order, steps: [['Select: ', home], ['View: ', '/CONTINUE-PARTIAL'], ['View: ', { keys: '\x1b[B' }], ['View: ', 'a'], ['Select: ', String(index)]] };
  }
  const readPartial = await partialStart('remaining'); assert.equal(readPartial.order.status, 'partially_shipped');
  const partialRecordsBefore = await journal.listForIdentity(scope);
  const progressTranscript = await driveTerminal([
    ...readPartial.steps, ['n next | p previous | Enter back: ', 'j'], ['n next | p previous | Enter back: ', ''], ['n next | p previous | Enter back: ', ''], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'partial-remaining-work-terminal.txt', { expectCommit: false, columns: 49, rows: 20 });
  assert.ok(progressTranscript.includes('Remaining active demand: 8 each') && progressTranscript.includes('To pick: 6 each'));
  assert.equal((await partialState()).version, readPartial.state.version);
  assert.deepEqual(await journal.listForIdentity(scope), partialRecordsBefore);
  const pickPartial = await partialStart('pick');
  await driveTerminal([
    ...pickPartial.steps, ['Select: ', '1'], ['location: ', 'A'], ['lot: ', partialLot], ['quantity (whole each): ', '3'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'partial-continued-pick-terminal.txt', { columns: 49, rows: 20 });
  partial = partialOrder(await partialState());
  assert.equal(orderProgress(partial).totals.shipped, '2'); assert.equal(orderProgress(partial).totals.staged, '3');
  const partialPick = (await journal.listForIdentity(scope)).find(record => record.request.command.type === 'pick' && record.request.command.order_id === partial.id);
  assert.equal(partialPick.request.expected_version, pickPartial.state.version);
  assert.deepEqual(partialPick.request.command, { type: 'pick', order_id: partial.id, line_id: 'Original-Partial-L1', location: 'A', lot: partialLot, quantity: 3 });
  await seed(scope.warehouse, { type: 'count_stock', sku: partialSku, lot: partialLot, location: 'A', counted: 5, reason: 'Actual synthetic additional two units' });
  const allocatePartial = await partialStart('allocate_available');
  await driveTerminal([
    ...allocatePartial.steps, ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'partial-additional-allocation-terminal.txt', { columns: 49, rows: 20 });
  partial = partialOrder(await partialState()); assert.equal(orderProgress(partial).totals.unallocated, '0'); assert.equal(orderProgress(partial).totals.to_pick, '5');
  const releasePartial = await partialStart('release_unpicked');
  await driveTerminal([
    ...releasePartial.steps, ['reason: ', 'Actual decision to release unpicked demand'], ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'partial-release-unpicked-terminal.txt', { columns: 49, rows: 20 });
  const afterPartialRelease = await partialState();
  assert.deepEqual(afterPartialRelease.stocks.map(({ reserved, ...row }) => row), releasePartial.state.stocks.map(({ reserved, ...row }) => row), 'release changes no physical stock');
  assert.equal(orderProgress(partialOrder(afterPartialRelease)).totals.to_pick, '0');
  const unpickPartial = await partialStart('unpick');
  await driveTerminal([
    ...unpickPartial.steps, ['Select: ', '1'], ['location: ', 'A'], ['lot: ', partialLot], ['quantity (whole each): ', '3'], ['reason: ', 'Actual return of three unshipped staged units'],
    ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'partial-return-staged-terminal.txt', { columns: 49, rows: 20 });
  const closePartial = await partialStart('cancel_remainder'); const beforeClosureRecords = await journal.listForIdentity(scope);
  await driveTerminal([
    ...closePartial.steps, ['reason: ', 'Actual proposed closure declined'], ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'no'], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'partial-declined-closure-terminal.txt', { expectCommit: false, columns: 49, rows: 20 });
  assert.deepEqual(await journal.listForIdentity(scope), beforeClosureRecords);
  await driveTerminal([
    ...closePartial.steps, ['reason: ', 'Actual decision to close the original unshipped remainder'], ['Enter to confirm: ', ''], ['Record this observed work? Type yes: ', 'yes'], ['Enter to continue: ', ''], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'partial-original-remainder-closure-terminal.txt', { columns: 49, rows: 20 });
  const afterClosure = await partialState(); partial = partialOrder(afterClosure);
  assert.equal(partial.status, 'closed_partial'); assert.equal(orderProgress(partial).totals.remaining, '0'); assert.equal(orderProgress(partial).totals.cancelled, '8'); assert.equal(orderProgress(partial).totals.shipped, '2');
  assert.deepEqual(afterClosure.stocks.map(({ reserved, ...row }) => row), closePartial.state.stocks.map(({ reserved, ...row }) => row), 'closing the commercial remainder does not move goods');
  assert.equal(afterClosure.stocks.find(row => row.sku === partialSku && row.location === 'A').reserved, 0);
  const cancellation = (await journal.listForIdentity(scope)).find(record => record.request.command.type === 'cancel_remainder' && record.request.command.order_id === partial.id);
  assert.equal(cancellation.stage, 'committed'); assert.equal(cancellation.request.expected_version, closePartial.state.version);
  assert.deepEqual(cancellation.request.command, { type: 'cancel_remainder', order_id: partial.id, reason: 'Actual decision to close the original unshipped remainder' });
  const closedRead = await partialStart('remaining', 'o');
  const closedTranscript = await driveTerminal([
    ['Select: ', 'l'], ['View: ', '/CONTINUE-PARTIAL'], ['View: ', 'q'], ...closedRead.steps,
    ['n next | p previous | Enter back: ', ''], ['View: ', 'q'], ['Select: ', 'q'],
  ], 'partial-closed-original-review-terminal.txt', { expectCommit: false, columns: 49, rows: 20 });
  assert.ok(closedTranscript.includes('No rows in this filter.') && closedTranscript.includes('Cancelled original demand: 8 each') && closedTranscript.includes('Remaining active demand: 0 each'));
  const retainedPackage = (await client.read(client.warehousePath(scope.warehouse, `/orders/${partial.id}/shipments`))).packages.find(pack => pack.id === originalPackage.id);
  assert.deepEqual(retainedPackage, originalHandoff, 'the original physical handoff remains unchanged');
  assert.equal((await client.read(client.warehousePath(scope.warehouse, '/reconciliation'))).ok, true);
  t.diagnostic(`Private synthetic TUI evidence retained: ${root}`);
});
