import { visibleCommands } from './commands.mjs';

const sources = new Set(['stocks', 'orders', 'receipts', 'exceptions']);
export function rowWork(kind, row, role) {
  let types = [];
  if (kind === 'stocks') types = [...(row.on_hand > 0 ? ['move_stock'] : []), 'count_stock'];
  if (kind === 'receipts' && !['received', 'cancelled'].includes(row.status)) types = ['receive', 'cancel_receipt'];
  if (kind === 'exceptions' && row.status === 'open') types = ['resolve_exception'];
  if (kind === 'orders') {
    if (row.status === 'pending') types = ['allocate_available', 'cancel_order'];
    if (row.status === 'allocated') types = ['pick', 'release_unpicked', 'cancel_order'];
    if (row.status === 'picking') types = ['pick', 'unpick', 'release_unpicked'];
    if (row.status === 'picked') types = ['unpick', ...(!row.shipment_mode ? ['pack'] : [])];
    if (row.status === 'packed' && !row.shipment_mode) types = ['ship'];
    types.push('report_exception');
  }
  // These are navigation suggestions, never admission or availability proofs.
  return visibleCommands(role).filter(spec => types.includes(spec.type));
}
export function selectionFor(scope, kind, row, version, commandType, lineId) {
  if (!sources.has(kind)) throw new Error('Unsupported selected work source');
  const key = kind === 'stocks' ? { sku: row.sku, lot: row.lot, location: row.location } : { id: row.id };
  return { scope: { ...scope }, kind, key, version, commandType, ...(lineId !== undefined ? { lineId } : {}) };
}
export function selectedWork(snapshot, scope, selection, role) {
  if (!['origin', 'tenant', 'actor', 'warehouse'].every(key => selection.scope?.[key] === scope[key]) || snapshot.warehouse.id !== scope.warehouse) throw new Error('Selected work belongs to a different server, tenant, actor or facility');
  if (!Number.isSafeInteger(selection.version) || snapshot.version !== selection.version) throw new Error('Selected view is stale; refresh the table and select again. No request sent.');
  if (!sources.has(selection.kind)) throw new Error('Unsupported selected work source');
  const fields = selection.kind === 'stocks' ? ['sku', 'lot', 'location'] : ['id'];
  if (!fields.every(key => typeof selection.key?.[key] === 'string' && selection.key[key])) throw new Error('Selected original identity is incomplete');
  const row = snapshot[selection.kind].find(row => fields.every(key => row[key] === selection.key[key]));
  if (!row) throw new Error('Selected original record is missing; refresh the table');
  const spec = rowWork(selection.kind, row, role).find(spec => spec.type === selection.commandType);
  if (!spec) throw new Error('This action is unavailable for the selected record or current role');
  const recordField = { orders: 'order_id', receipts: 'receipt_id', exceptions: 'exception_id' }[selection.kind];
  const identities = selection.kind === 'stocks'
    ? { sku: row.sku, lot: row.lot, [spec.type === 'move_stock' ? 'from' : 'location']: row.location }
    : { [recordField]: row.id };
  if (['receive', 'pick', 'unpick'].includes(spec.type)) {
    const line = row.lines.find(line => line.id === selection.lineId);
    if (!line) throw new Error('Select an exact original line before recording this work');
    identities.line_id = line.id;
  } else if (selection.lineId !== undefined) throw new Error('This action does not take a selected line');
  return { spec, identities };
}
