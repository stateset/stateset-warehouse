import test from 'node:test';
import assert from 'node:assert/strict';
import { readTablePage } from '../lib/pages.mjs';
import { tableView } from '../lib/queues.mjs';

test('lot cursors reach the server unchanged and availability uses the server verdict', async () => {
  const cursor = JSON.stringify(['SKU / É', 'Lot " & ? # Original']);
  const row = { sku: 'SKU / É', lot: 'Original', physical: 8, reserved: 0, usable: 0, availability: 'quarantined', control: { holds: [{ id: 'Exact-HOLD' }] } };
  let called;
  const client = {
    warehousePath: (warehouse, suffix) => `/v1/warehouses/${encodeURIComponent(warehouse)}${suffix}`,
    async read(path) { called = new URL(path, 'https://example.com'); return { warehouse_id: 'Original-Facility', version: 9, checked_at: '2026-10-19T00:00:00Z', items: [row], next_cursor: null }; },
  };
  const page = await readTablePage(client, 'Original-Facility', 'lots', cursor);
  assert.equal(called.searchParams.get('after'), cursor);
  assert.equal(called.searchParams.get('limit'), '100');
  assert.equal(page.rows[0], row);
  assert.equal(page.rows[0].control.holds[0].id, 'Exact-HOLD');
  assert.deepEqual(tableView('lots', page, { filter: 'available' }).rows, [], 'physical minus reserved never substitutes for the server availability verdict');
  assert.deepEqual(tableView('lots', page, { filter: 'quarantined' }).rows, [row]);
});

test('transfer batches include settled work and retain original records and facility direction', async () => {
  const rows = [
    { id: 'Original-TRANSFER', reference: 'T-1', source_warehouse_id: 'Source', destination_warehouse_id: 'Destination', lines: [{ id: 'Exact-Line', quantity: 2, received: 2 }], ok: true },
    { id: 'original-transfer', reference: 'T-2', source_warehouse_id: 'Destination', destination_warehouse_id: 'Source', lines: [], ok: false },
  ];
  const client = {
    warehousePath: (_warehouse, suffix) => `/v1/warehouses/Destination${suffix}`,
    async read(path) {
      const query = new URL(path, 'https://example.com').searchParams;
      assert.equal(query.get('open_only'), 'false'); assert.equal(query.get('after'), '17');
      return { warehouse_id: 'Destination', version: 8, items: rows.map((data, index) => ({ cursor: index + 18, data })), next_cursor: 19 };
    },
  };
  const page = await readTablePage(client, 'Destination', 'transfers', 17);
  assert.equal(page.nextCursor, 19);
  assert.equal(page.rows[0], rows[0]);
  assert.deepEqual(tableView('transfers', page, { filter: 'inbound' }).rows, [rows[0]]);
  assert.deepEqual(tableView('transfers', page, { filter: 'outbound' }).rows, [rows[1]]);
  assert.deepEqual(tableView('transfers', page, { filter: 'held' }).rows, [rows[1]]);
});

test('invalid page shapes, identities and cursors are held instead of dropping records', async () => {
  const original = { warehouse_id: 'Facility', version: 0, items: [], next_cursor: null };
  const client = { warehousePath: () => '/v1/warehouses/Facility/lots', read: async () => original };
  for (const cursor of [0, {}, '', 'x'.repeat(2049)]) await assert.rejects(readTablePage(client, 'Facility', 'lots', cursor), /Invalid original page cursor/);
  for (const cursor of ['17', -1, Number.MAX_SAFE_INTEGER + 1]) await assert.rejects(readTablePage(client, 'Facility', 'transfers', cursor), /Invalid original page cursor/);
  for (const change of [{ warehouse_id: 'other' }, { version: 0.5 }, { items: [null] }, { items: Array(101).fill({}) }, { next_cursor: undefined }, { next_cursor: {} }]) {
    await assert.rejects(readTablePage({ ...client, read: async () => ({ ...original, ...change }) }, 'Facility', 'lots'), /Invalid/);
  }
});
