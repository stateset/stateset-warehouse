export const scope = { origin: 'https://warehouse.example.invalid', tenant: 'tenant-a', actor: 'operator-a', warehouse: 'wh-a' };
export function consoleFixture() {
  return { schema_version: 1, demo: true, tenant: scope.tenant, warehouse_id: scope.warehouse, warehouse_name: 'Synthetic warehouse', version: '9', captured_at: '2026-10-20T20:00:00Z', source_ok: true, unit: 'each',
    flow: ['pending', 'allocated', 'picking', 'picked', 'packed', 'partially_shipped'].map((status, index) => ({ status, orders: String(index) })), open_exceptions: '1', stock: { on_hand_each: '10', reserved_each: '3' },
    bins: { total: 2, next_after: null, items: [{ code: 'BIN-A', zone: 'A', kind: 'storage', on_hand_each: '8', reserved_each: '3', lot_positions: 1 }, { code: 'BIN-B', zone: 'B', kind: 'receiving', on_hand_each: '2', reserved_each: '0', lot_positions: 1 }] },
    floor_plan: { available: true, revision: 'a'.repeat(64), name: 'Synthetic configured plan', unit: 'grid', width: 100, height: 60, mapped_bins: 2, total_bins: 2,
      bins: [{ code: 'BIN-A', x: 5, y: 5, width: 35, height: 50, on_hand_each: '8', reserved_each: '3' }, { code: 'BIN-B', x: 60, y: 5, width: 35, height: 50, on_hand_each: '2', reserved_each: '0' }] } };
}
export function productFixture() {
  return { schema_version: 1, demo: true, tenant: scope.tenant, warehouse_id: scope.warehouse, version: '9', captured_at: '2026-10-20T20:00:00Z', source_ok: true, image_scope: 'b'.repeat(64), sku: 'Sku-Ab12', name: 'Synthetic part', category: 'Parts', serial_controlled: true, expiry_required: false, reorder_point_each: '100', label: { value: 'Sku-Ab12', unit: 'each', symbology: 'code128' }, image: null, image_status: 'not_configured' };
}
