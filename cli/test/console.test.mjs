import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { Client } from '../lib/client.mjs';
import { validateConsole, validateProduct, readConsole, readProductImage } from '../lib/console.mjs';
import { consoleFixture, productFixture, scope } from './console-fixture.mjs';
import { Raster } from '../lib/raster.mjs';

test('visual evidence binds tenant, facility, schema, proof and exact quantities', () => {
  assert.equal(validateConsole(consoleFixture(), scope).stock.on_hand_each, '10');
  for (const fault of ['tenant', 'warehouse', 'schema', 'proof', 'version', 'rounded', 'reserved', 'stage', 'duplicate', 'time', 'demo']) {
    const value = consoleFixture();
    if (fault === 'tenant') value.tenant = 'tenant-b';
    if (fault === 'warehouse') value.warehouse_id = 'other';
    if (fault === 'schema') value.schema_version = 2;
    if (fault === 'proof') value.source_ok = false;
    if (fault === 'version') value.version = '9223372036854775808';
    if (fault === 'rounded') value.stock.on_hand_each = 10;
    if (fault === 'reserved') value.stock.reserved_each = '11';
    if (fault === 'stage') value.flow[0].status = 'unknown';
    if (fault === 'duplicate') value.bins.items[1].code = 'BIN-A';
    if (fault === 'time') value.captured_at = 'not-a-time';
    if (fault === 'demo') delete value.demo;
    assert.throws(() => validateConsole(value, scope), /evidence/, fault);
  }
});
test('quantities beyond Number precision stay exact in the visual model', () => {
  const value = consoleFixture(); const huge = '900719925474099312345';
  value.stock.on_hand_each = huge; value.bins.items[0].on_hand_each = '900719925474099312343'; value.floor_plan.bins[0].on_hand_each = value.bins.items[0].on_hand_each;
  assert.equal(validateConsole(value, scope).stock.on_hand_each, huge);
});
test('page cursors retain the reviewed version and cannot substitute bin identities', async () => {
  const value = consoleFixture(); value.bins.items = [value.bins.items[1]];
  const paths = []; const client = { warehousePath: (id, suffix) => `/v1/warehouses/${id}${suffix}`, read: async path => { paths.push(path); return value; } };
  assert.equal((await readConsole(client, scope, { after: 'BIN-A', review: '9', limit: 1 })).bins.items[0].code, 'BIN-B');
  const url = new URL(paths[0], scope.origin); assert.equal(url.searchParams.get('review'), '9'); assert.equal(url.searchParams.get('after'), 'BIN-A');
  value.version = '10'; await assert.rejects(readConsole(client, scope, { after: 'BIN-A', review: '9', limit: 1 }), /changed/);
  await assert.rejects(readConsole(client, scope, { after: 'BIN-A' }), /evidence/);
});
test('floor-plan bounds and source coverage cannot silently become a fake physical layout', () => {
  for (const fault of ['outside', 'duplicate', 'coverage', 'revision', 'absent']) {
    const value = consoleFixture();
    if (fault === 'outside') value.floor_plan.bins[0].x = 99999;
    if (fault === 'duplicate') value.floor_plan.bins[1].code = 'BIN-A';
    if (fault === 'coverage') value.floor_plan.mapped_bins = 3;
    if (fault === 'revision') value.floor_plan.revision = '';
    if (fault === 'absent') delete value.floor_plan.available;
    assert.throws(() => validateConsole(value, scope), /evidence/, fault);
  }
  const value = consoleFixture(); value.floor_plan = { available: false, reason: 'not_configured' };
  assert.equal(validateConsole(value, scope).floor_plan.available, false);
});
test('product label evidence preserves the exact original SKU and rejects substituted symbology data', () => {
  const product = productFixture(); assert.equal(validateProduct(product, scope, 'Sku-Ab12').label.value, 'Sku-Ab12');
  for (const fault of ['case', 'label', 'identity', 'symbology', 'serial', 'image-status']) {
    const value = productFixture();
    if (fault === 'case') value.sku = 'SKU-AB12';
    if (fault === 'label') value.label.value = 'different';
    if (fault === 'identity') value.image_scope = 'invalid';
    if (fault === 'symbology') value.label.symbology = 'unverified';
    if (fault === 'serial') value.serial_controlled = 'yes';
    if (fault === 'image-status') value.image_status = 'ready';
    assert.throws(() => validateProduct(value, scope, 'Sku-Ab12'), /evidence/, fault);
  }
});
test('product bytes bind the reviewed digest, dimensions, size and identity fence', async () => {
  const bytes = new Raster(16, 12).png(); const product = productFixture();
  product.image_status = 'ready'; product.image = { width: 16, height: 12, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };
  let request; const client = { warehousePath: (id, suffix) => `/v1/warehouses/${id}${suffix}`, image: async path => { request = new URL(path, scope.origin); return bytes; } };
  assert.deepEqual(await readProductImage(client, scope, product), bytes);
  assert.equal(request.searchParams.get('scope'), product.image_scope); assert.equal(request.searchParams.get('review'), '9');
  assert.equal(request.searchParams.get('sha256'), product.image.sha256);
  const corrupted = Buffer.from(bytes); corrupted[corrupted.length - 1] ^= 1; client.image = async () => corrupted;
  await assert.rejects(readProductImage(client, scope, product), /evidence/);
});
test('binary API reads retain authentication, reject redirects and cancel oversized responses', async () => {
  const bytes = new Raster(2, 2).png(); let captured;
  const client = new Client(scope.origin, 'not-a-secret', { fetchImpl: async (url, init) => { captured = { url, init }; return new Response(bytes, { headers: { 'content-type': 'image/png' } }); } });
  assert.deepEqual(await client.image('/v1/warehouses/wh-a/console/products/Sku-Ab12/image'), bytes);
  assert.equal(captured.init.redirect, 'error'); assert.equal(captured.init.headers.authorization, 'Bearer not-a-secret');
  await assert.rejects(client.image('https://other.example/image.png'), /Invalid/);
  await assert.rejects(client.image('/v1/../../private'), /Invalid/);
  let cancelled = false;
  client.fetch = async () => new Response(new ReadableStream({ pull(controller) { controller.enqueue(new Uint8Array(4 * 1024 * 1024 + 1)); }, cancel() { cancelled = true; } }), { headers: { 'content-type': 'image/png' } });
  await assert.rejects(client.image('/v1/warehouses/wh-a/console/products/x/image'), /4 MiB/); assert.equal(cancelled, true);
  client.fetch = async () => new Response('no image', { status: 409 });
  await assert.rejects(client.image('/v1/warehouses/wh-a/console/products/x/image'), /409/);
});
