import { visibleCommands } from './commands.mjs';
import { readSelectedTransfer } from './pages.mjs';

const sources = new Set(['stocks', 'orders', 'receipts', 'exceptions', 'transfers', 'lots']);
export function remainingTransferLines(row) {
  return (row.lines ?? []).filter(line => [line.quantity, line.received, line.lost].every(value => Number.isSafeInteger(value) && value >= 0) && line.quantity - line.received > line.lost);
}
export function rowWork(kind, row, role, warehouse) {
  let types = [];
  if (kind === 'stocks') types = [...(row.on_hand > 0 ? ['move_stock'] : []), 'count_stock'];
  if (kind === 'receipts' && !['received', 'cancelled'].includes(row.status)) types = ['receive', 'cancel_receipt'];
  if (kind === 'exceptions' && row.status === 'open') types = ['resolve_exception'];
  if (kind === 'transfers' && row.ok === true && typeof warehouse === 'string' && row.destination_warehouse_id === warehouse && remainingTransferLines(row).length) types = ['receive_transfer'];
  if (kind === 'lots') types = ['quarantine_lot', row.control?.expiry_recorded ? 'correct_lot_expiry' : 'record_lot_expiry', ...(row.control?.holds?.length ? ['release_lot_hold'] : [])];
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
export function selectionFor(scope, kind, row, version, commandType, lineId, holdId) {
  if (!sources.has(kind)) throw new Error('Unsupported selected work source');
  const key = kind === 'lots' ? { sku: row.sku, lot: row.lot } : kind === 'stocks' ? { sku: row.sku, lot: row.lot, location: row.location } : { id: row.id, ...(kind === 'transfers' ? { seq: row.seq } : {}) };
  return { scope: { ...scope }, kind, key, version, commandType, ...(lineId !== undefined ? { lineId } : {}), ...(holdId !== undefined ? { holdId } : {}) };
}
function checkSelection(snapshot, scope, selection) {
  if (!['origin', 'tenant', 'actor', 'warehouse'].every(key => selection.scope?.[key] === scope[key]) || snapshot.warehouse.id !== scope.warehouse) throw new Error('Selected work belongs to a different server, tenant, actor or facility');
  if (!Number.isSafeInteger(selection.version) || snapshot.version !== selection.version) throw new Error('Selected view is stale; refresh the table and select again. No request sent.');
  if (!sources.has(selection.kind)) throw new Error('Unsupported selected work source');
  const fields = selection.kind === 'lots' ? ['sku', 'lot'] : selection.kind === 'stocks' ? ['sku', 'lot', 'location'] : ['id'];
  if (!fields.every(key => typeof selection.key?.[key] === 'string' && selection.key[key])) throw new Error('Selected original identity is incomplete');
  if (selection.kind === 'transfers' && (!Number.isSafeInteger(selection.key.seq) || selection.key.seq < 1)) throw new Error('Selected original transfer identity is incomplete');
  return fields;
}
export function selectedWork(snapshot, scope, selection, role) {
  const fields = checkSelection(snapshot, scope, selection);
  const matches = row => fields.every(key => row[key] === selection.key[key]);
  let row;
  if (selection.kind === 'lots') {
    const control = (snapshot.lot_controls ?? []).find(matches);
    if (snapshot.products.some(product => product.sku === selection.key.sku) && (control || snapshot.stocks.some(matches))) row = { sku: selection.key.sku, lot: selection.key.lot, control };
  } else row = snapshot[selection.kind].find(matches);
  if (!row) throw new Error('Selected original record is missing; refresh the table');
  if (selection.kind === 'transfers' && row.seq !== selection.key.seq) throw new Error('Selected original transfer identity changed; refresh and select again');
  const spec = rowWork(selection.kind, row, role, scope.warehouse).find(spec => spec.type === selection.commandType);
  if (!spec) throw new Error('This action is unavailable for the selected record or current role');
  const recordField = { orders: 'order_id', receipts: 'receipt_id', exceptions: 'exception_id', transfers: 'transfer_id' }[selection.kind];
  let identities = selection.kind === 'lots' ? { sku: row.sku, lot: row.lot } : selection.kind === 'stocks'
    ? { sku: row.sku, lot: row.lot, [spec.type === 'move_stock' ? 'from' : 'location']: row.location }
    : { [recordField]: row.id };
  if (['receive', 'pick', 'unpick', 'receive_transfer'].includes(spec.type)) {
    const line = (spec.type === 'receive_transfer' ? remainingTransferLines(row) : row.lines).find(line => line.id === selection.lineId);
    if (!line) throw new Error('Select an exact original line before recording this work');
    identities.line_id = line.id;
  } else if (selection.lineId !== undefined) throw new Error('This action does not take a selected line');
  if (spec.type === 'release_lot_hold') {
    const hold = row.control?.holds.find(hold => hold.id === selection.holdId);
    if (!hold) throw new Error('Select an exact original hold before recording its release');
    identities = { hold_id: hold.id };
  } else if (selection.holdId !== undefined) throw new Error('This action does not take a selected hold');
  return { spec, identities };
}

export async function reviewSelectedWork(client, snapshot, scope, selection, role) {
  checkSelection(snapshot, scope, selection);
  if (selection.kind !== 'transfers') return selectedWork(snapshot, scope, selection, role);
  if (selection.commandType !== 'receive_transfer' || !visibleCommands(role).some(spec => spec.type === selection.commandType)) throw new Error('This action is unavailable for the selected record or current role');
  const transfer = await readSelectedTransfer(client, scope.warehouse, selection.key, snapshot.version);
  return selectedWork({ ...snapshot, transfers: [transfer] }, scope, selection, role);
}
