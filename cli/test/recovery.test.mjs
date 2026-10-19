import test from 'node:test';
import assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import path from 'node:path';
import os from 'node:os';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { Client, parseJson, originUrl, safeText } from '../lib/client.mjs';
import { Journal, sendOriginal, rejectionMatches } from '../lib/journal.mjs';
import { collectSerials, argumentsFor } from '../warehouse-tui.mjs';
import { commandForm, commands } from '../lib/commands.mjs';
const scope = { origin: 'http://127.0.0.1:8088', tenant: 'synthetic', actor: 'operator:original', warehouse: 'wh-main' };
const request = () => ({ request_id: randomUUID(), expected_version: 4, command: { type: 'move_stock', sku: 'SKU', lot: 'LOT', from: 'A', to: 'B', quantity: 1 }, serial_inputs: [{ target: [], sku: 'SKU', serials: ['Literal-É'] }] });
async function fixture(t, options) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'wms-tui-journal-test-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const journal = new Journal(root, options); return { root, journal, record: await journal.prepare(scope, request()) };
}
function clientWith(send, identity = { tenant: scope.tenant, actor: scope.actor, role: 'operator' }) {
  return { origin: scope.origin, session: async () => ({ identity }), warehousePath: (w, s) => `/v1/warehouses/${encodeURIComponent(w)}${s}`, call: send };
}

test('a lost successful response survives process restart and replays exact original bytes once', async t => {
  const { root, journal, record } = await fixture(t);
  let commits = 0; const bodies = []; const committed = new Map();
  const client = clientWith(async (_method, _path, body) => {
    // The send intent is durably visible before each network side effect.
    assert.equal((await new Journal(root).load(record.dir)).stage, 'send_intent');
    bodies.push(body); const r = parseJson(body);
    if (!committed.has(r.request_id)) { commits++; committed.set(r.request_id, { entity_id: 'original-result', version: 5 }); throw new Error('Connection lost after commit'); }
    return { ok: true, status: 200, data: { replayed: true, result: committed.get(r.request_id) } };
  });
  const uncertain = await sendOriginal(client, journal, record); assert.equal(uncertain.stage, 'uncertain');
  const restarted = new Journal(root); const pending = (await restarted.list(scope))[0];
  const recovered = await sendOriginal(client, restarted, pending);
  assert.equal(recovered.stage, 'committed'); assert.equal(commits, 1);
  assert.deepEqual(bodies, [record.original.body, record.original.body]);
  assert.equal(recovered.request.expected_version, 4); assert.deepEqual(recovered.request.serial_inputs, record.request.serial_inputs);
  await sendOriginal(client, restarted, recovered); assert.equal(bodies.length, 2);
  const files = await fs.readdir(record.dir);
  for (const file of files) assert.equal((await fs.stat(path.join(record.dir, file))).mode & 0o777, 0o600);
  assert.equal((await fs.stat(root)).mode & 0o777, 0o700);
});

test('changed actor, tenant or server cannot replay original work', async t => {
  const { journal, record } = await fixture(t); let sends = 0;
  for (const identity of [{ tenant: scope.tenant, actor: 'other' }, { tenant: 'other', actor: scope.actor }]) {
    await assert.rejects(sendOriginal(clientWith(async () => { sends++; }, identity), journal, record), /different server, tenant or actor/);
  }
  const client = clientWith(async () => { sends++; }); client.origin = 'https://other.example';
  await assert.rejects(sendOriginal(client, journal, record), /different server/); assert.equal(sends, 0);
});

test('disk failure before send intent prevents transmission; original request remains recoverable', async t => {
  let fail = false;
  const { root, journal, record } = await fixture(t, { beforePublish: name => { if (fail && name.endsWith('send_intent.json')) throw new Error('Synthetic fsync/publication failure'); } });
  fail = true; let sends = 0;
  await assert.rejects(sendOriginal(clientWith(async () => { sends++; }), journal, record), /Synthetic/);
  assert.equal(sends, 0); assert.equal((await new Journal(root).load(record.dir)).stage, 'original');
});

test('disk failure after committed HTTP response preserves original request and send intent', async t => {
  const { root, journal, record } = await fixture(t, { beforePublish: name => { if (name.endsWith('-committed.json')) throw new Error('Synthetic post-response disk failure'); } });
  await assert.rejects(sendOriginal(clientWith(async () => ({ ok: true, status: 200, data: { replayed: false, result: { version: 5 } } })), journal, record), /post-response/);
  const retained = await new Journal(root).load(record.dir); assert.equal(retained.stage, 'send_intent'); assert.equal(retained.original.body, record.original.body);
});

test('only a fully bound definitive stale proof resolves rejected work', async t => {
  const { journal, record } = await fixture(t);
  const proof = { status: 'not_committed', reason: 'stale_version', tenant: scope.tenant, actor: scope.actor, warehouse_id: scope.warehouse, request_id: record.request.request_id, expected_version: 4, current_version: 5 };
  assert.equal(rejectionMatches(proof, record), true);
  for (const field of ['tenant', 'actor', 'warehouse_id', 'request_id', 'expected_version', 'reason', 'status']) assert.equal(rejectionMatches({ ...proof, [field]: 'other' }, record), false, field);
  for (const data of [{ error: 'unavailable' }, { command_outcome: { ...proof, actor: 'other' } }, { found: false }]) {
    const next = await sendOriginal(clientWith(async () => ({ ok: false, status: 409, data })), journal, await journal.load(record.dir));
    assert.equal(next.stage, 'uncertain');
  }
  const done = await sendOriginal(clientWith(async () => ({ ok: false, status: 409, data: { command_outcome: proof } })), journal, await journal.load(record.dir));
  assert.equal(done.stage, 'not_committed');
});

test('tampering, incomplete history, unsafe permissions and symlink directories are held', async t => {
  const { root, journal, record } = await fixture(t);
  const next = await journal.append(record, 'send_intent');
  const filename = path.join(record.dir, '000001-send_intent.json'); const original = await fs.readFile(filename, 'utf8');
  await fs.writeFile(filename, original.replace('previous_sha256', 'forged_sha256'));
  await assert.rejects(journal.load(record.dir), /changed/);
  await fs.writeFile(filename, original);
  await fs.chmod(filename, 0o644); await assert.rejects(journal.load(record.dir), /Unsafe recovery file/); await fs.chmod(filename, 0o600);
  await fs.rename(filename, path.join(record.dir, '000002-send_intent.json')); await assert.rejects(journal.load(record.dir), /sequence/);
  const linked = path.join(root, 'linked'); await fs.symlink(record.dir, linked);
  await assert.rejects(new Journal(linked).list(scope), /symlinks/);
  assert.equal(next.stage, 'send_intent');
});

test('concurrent journal changes prevent a stale process from sending', async t => {
  const { journal, record } = await fixture(t);
  await journal.append(record, 'send_intent');
  await assert.rejects(journal.append(record, 'send_intent'), /changed in another process/);
});

test('HTTP transport refuses redirects and never sends credentials to the redirect target', async t => {
  let targetCalls = 0;
  const target = createServer((_req, res) => { targetCalls++; res.end('{}'); });
  await new Promise(resolve => target.listen(0, '127.0.0.1', resolve));
  const source = createServer((_req, res) => { res.writeHead(307, { location: `http://127.0.0.1:${target.address().port}/v1/session` }); res.end(); });
  await new Promise(resolve => source.listen(0, '127.0.0.1', resolve));
  t.after(async () => { await Promise.all([new Promise(r => target.close(r)), new Promise(r => source.close(r))]); });
  const client = new Client(`http://127.0.0.1:${source.address().port}`, 'synthetic-private-token');
  await assert.rejects(client.session()); assert.equal(targetCalls, 0);
  assert.ok(!JSON.stringify(client).includes('synthetic-private-token'));
});

test('numeric precision, unsafe origins and terminal controls cannot silently change identities', () => {
  for (const raw of ['{"version":9007199254740993}', '{"quantity":1.5}', '{"quantity":1.00000000000000001}']) assert.throws(() => parseJson(raw), /exact safe integers/);
  assert.deepEqual(parseJson('{"quantity":"1.00000000000000001","version":4}'), { quantity: '1.00000000000000001', version: 4 });
  for (const raw of ['http://remote.example', 'https://user:secret@warehouse.example', 'https://warehouse.example/other', 'https://warehouse.example/?token=secret']) assert.throws(() => originUrl(raw));
  assert.equal(originUrl('https://warehouse.example'), 'https://warehouse.example');
  assert.equal(safeText('\x1b[31m\u009b\u202e'), '\\u001b[31m\\u009b\\u202e');
  assert.throws(() => argumentsFor(['--token', 'secret']), /Unknown/);
});

test('serial collection requires real literal scans, preserves case and never uses custody labels', async () => {
  let planId;
  const client = { warehousePath: (_w, s) => s, call: async (_m, _p, r) => {
    planId = r.plan_id;
    return { ok: true, data: { plan_id: planId, warehouse_id: 'wh-main', command_type: 'move_stock', observed_warehouse_version: 4, required_labels: 2, targets: [{ target: [], sku: 'SKU', each_quantity: 2 }], custody_labels: ['DO-NOT-PREFILL'] } };
  } };
  const labels = ['Original', 'original'];
  const result = await collectSerials(client, 'wh-main', { version: 4 }, { type: 'move_stock' }, async () => labels.shift());
  assert.deepEqual(result, [{ target: [], sku: 'SKU', serials: ['Original', 'original'] }]); assert.ok(planId);
  await assert.rejects(collectSerials(client, 'wh-main', { version: 5 }, { type: 'move_stock' }, async () => 'irrelevant'), /changed/);
  await assert.rejects(collectSerials(client, 'wh-main', { version: 4 }, { type: 'move_stock' }, async () => 'Duplicate'), /Duplicate/);
});

test('typed multi-line forms preserve exact labels and whole each values', async () => {
  const values = ['Original receipt', 'Supplier', 'LINE', 'SKU', 'Lot-É', '2', 'no'];
  const command = await commandForm(commands.find(c => c.type === 'create_receipt'), async () => values.shift());
  assert.deepEqual(command, { type: 'create_receipt', reference: 'Original receipt', supplier: 'Supplier', lines: [{ id: 'LINE', sku: 'SKU', lot: 'Lot-É', quantity: 2 }] });
});

test('unknown new-facility work is discoverable across facilities and remains bound to its original target', async t => {
  const { root, journal } = await fixture(t);
  const originalScope = { ...scope, warehouse: `wh-${randomUUID()}` };
  const originalRequest = { request_id: randomUUID(), expected_version: 0, command: { type: 'create_warehouse', code: 'ANNEX', name: 'Synthetic annex', city: 'Synthetic city', timezone: 'UTC' } };
  const original = await journal.prepare(originalScope, originalRequest);
  let calls = 0;
  const client = clientWith(async (_method, path, body) => {
    calls++; assert.equal(path, `/v1/warehouses/${originalScope.warehouse}/commands`);
    assert.equal(body, original.original.body); assert.equal(parseJson(body).expected_version, 0);
    throw new Error('Synthetic unknown creation; directory may not contain this facility');
  });
  const outcome = await sendOriginal(client, journal, original); assert.equal(outcome.stage, 'uncertain');
  const pending = (await new Journal(root).listForIdentity(scope)).filter(r => r.stage === 'uncertain');
  assert.equal(pending.length, 1); assert.equal(pending[0].original.scope.warehouse, originalScope.warehouse);
  assert.deepEqual(await journal.listForIdentity({ ...scope, actor: 'other' }), []);
  await sendOriginal(client, new Journal(root), pending[0]); assert.equal(calls, 2);
});
