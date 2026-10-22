import test from 'node:test';
import assert from 'node:assert/strict';
import { labelRequest, labelLines, listLabels, recordLabel } from '../lib/carrier.mjs';

const fields = { carrier: ' Synthetic Parcel ', service: 'Ground', tracking: '1Z999', amount: '12.34', currency: 'usd', url: 'https://carrier.example/label.pdf' };

test('typed fields become the exact server request', () => {
  assert.deepEqual(labelRequest(fields), {
    carrier: 'Synthetic Parcel', service: 'Ground', tracking_number: '1Z999',
    rate: { amount: '12.34', currency: 'USD' }, carton: null,
    label: { format: 'url', url: 'https://carrier.example/label.pdf' },
  });
  assert.equal(labelRequest({ ...fields, amount: '', currency: '' }).rate, null);
});

test('bad fields are refused before anything is sent', () => {
  for (const bad of [{ carrier: '' }, { tracking: '1Z 999' }, { tracking: 'x'.repeat(65) }, { amount: '012' }, { amount: '1.123456789' }, { amount: '5', currency: 'US' }, { url: 'http://carrier.example/l.pdf' }, { url: '' }]) {
    assert.throws(() => labelRequest({ ...fields, ...bad }), undefined, JSON.stringify(bad));
  }
});

test('label lines mark the active label and void reasons', () => {
  assert.deepEqual(labelLines([]), ['No carrier label recorded yet.']);
  const lines = labelLines([
    { carrier: 'A', service: 'G', tracking_number: 'T1', rate: null, voided: { reason: 'reprint' } },
    { carrier: 'A', service: 'G', tracking_number: 'T2', rate: { amount: '1.00', currency: 'USD' }, voided: null },
  ]);
  assert.match(lines[0], /^voided .* T1 \| reprint$/);
  assert.match(lines[1], /^ACTIVE .* T2 \| 1\.00 USD$/);
  assert.throws(() => labelLines(null), /verified/);
});

test('list and record use the package carrier-label route and surface conflicts', async () => {
  const calls = [];
  const client = {
    warehousePath: (w, suffix) => `/v1/warehouses/${w}${suffix}`,
    call: async (method, path, body) => { calls.push([method, path]); return method === 'GET' ? { ok: true, status: 200, data: { labels: [] } } : { ok: false, status: 409, data: { error: 'Package already has an active label' } }; },
  };
  assert.deepEqual(await listLabels(client, 'wh', 'PKG 1'), []);
  await assert.rejects(recordLabel(client, 'wh', 'PKG 1', labelRequest(fields)), /HTTP 409: Package already has an active label/);
  assert.deepEqual(calls, [['GET', '/v1/warehouses/wh/packages/PKG%201/carrier-labels'], ['POST', '/v1/warehouses/wh/packages/PKG%201/carrier-labels']]);
});
