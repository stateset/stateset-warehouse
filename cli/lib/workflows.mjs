import { visibleCommands } from './commands.mjs';
import { readSelectedTransfer } from './pages.mjs';
import { orderProgress, remainingOrderLines } from './fulfillment.mjs';

const sources = new Set(['stocks', 'orders', 'receipts', 'exceptions', 'transfers', 'lots', 'shipments']);
export function remainingTransferLines(row) {
  return (row.lines ?? []).filter(line => [line.quantity, line.received, line.lost].every(value => Number.isSafeInteger(value) && value >= 0) && line.quantity - line.received > line.lost);
}
export function rowWork(kind, row, role, warehouse) {
  let types = [];
  if (kind === 'stocks') types = [...(row.on_hand > row.reserved ? ['move_stock', 'dispatch_transfer'] : row.on_hand > 0 ? ['move_stock'] : []), 'count_stock'];
  if (kind === 'receipts' && !['received', 'cancelled'].includes(row.status)) types = ['receive', 'cancel_receipt'];
  if (kind === 'exceptions' && row.status === 'open') types = ['resolve_exception'];
  if (kind === 'transfers' && row.ok === true && typeof warehouse === 'string' && row.destination_warehouse_id === warehouse && remainingTransferLines(row).length) types = ['receive_transfer'];
  if (kind === 'lots') types = ['quarantine_lot', row.control?.expiry_recorded ? 'correct_lot_expiry' : 'record_lot_expiry', ...(row.control?.holds?.length ? ['release_lot_hold'] : [])];
  if (kind === 'shipments' && row.closure === null) types = ['unpack_shipment', 'handoff_shipment'];
  if (kind === 'orders') {
    if (row.status === 'pending') types = ['allocate_available', 'cancel_order'];
    if (row.status === 'allocated') types = ['pick', 'release_unpicked', 'cancel_order'];
    if (row.status === 'picking') types = ['pick', 'unpick', 'release_unpicked'];
    if (row.status === 'picked') types = ['unpick', ...(!row.shipment_mode ? ['pack'] : [])];
    if (row.status === 'packed' && !row.shipment_mode) types = ['ship'];
    if (['allocated', 'picking', 'picked', 'partially_shipped'].includes(row.status) && row.allocations?.some(allocation => allocation.picked > allocation.shipped)) types.push('pack_shipment');
    if (row.shipment_mode === 'packages' && !['shipped', 'cancelled', 'closed_partial'].includes(row.status)) {
      const progress = orderProgress(row);
      types = types.filter(type => !['allocate_available', 'pick', 'unpick', 'release_unpicked', 'pack_shipment', 'cancel_order'].includes(type));
      if (!progress.error) {
        if (['pending', 'allocated'].includes(row.status)) types.push('cancel_order');
        if (BigInt(progress.totals.unallocated) > 0n) types.push('allocate_available');
        if (BigInt(progress.totals.to_pick) > 0n) types.push('pick', 'release_unpicked');
        if (BigInt(progress.totals.staged) > 0n) types.push('unpick', 'pack_shipment');
        if (row.status === 'partially_shipped' && progress.totals.staged === '0') types.push('cancel_remainder');
      }
    }
    if (['shipped', 'partially_shipped', 'closed_partial'].includes(row.status)) types.push('create_return');
    types.push('report_exception');
  }
  // These are navigation suggestions, never admission or availability proofs.
  return visibleCommands(role).filter(spec => types.includes(spec.type));
}
export function selectionFor(scope, kind, row, version, commandType, lineId, holdId) {
  if (!sources.has(kind)) throw new Error('Unsupported selected work source');
  const key = kind === 'lots' ? { sku: row.sku, lot: row.lot } : kind === 'stocks' ? { sku: row.sku, lot: row.lot, location: row.location } : { id: row.id, ...(kind === 'transfers' ? { seq: row.seq } : {}), ...(kind === 'shipments' ? { order_id: row.order_id } : {}) };
  return { scope: { ...scope }, kind, key, version, commandType, ...(lineId !== undefined ? { lineId } : {}), ...(holdId !== undefined ? { holdId } : {}) };
}
function checkSelection(snapshot, scope, selection) {
  if (!['origin', 'tenant', 'actor', 'warehouse'].every(key => selection.scope?.[key] === scope[key]) || snapshot.warehouse.id !== scope.warehouse) throw new Error('Selected work belongs to a different server, tenant, actor or facility');
  if (!Number.isSafeInteger(selection.version) || snapshot.version !== selection.version) throw new Error('Selected view is stale; refresh the table and select again. No request sent.');
  if (!sources.has(selection.kind)) throw new Error('Unsupported selected work source');
  const fields = selection.kind === 'lots' ? ['sku', 'lot'] : selection.kind === 'stocks' ? ['sku', 'lot', 'location'] : ['id'];
  if (!fields.every(key => typeof selection.key?.[key] === 'string' && selection.key[key])) throw new Error('Selected original identity is incomplete');
  if (selection.kind === 'transfers' && (!Number.isSafeInteger(selection.key.seq) || selection.key.seq < 1)) throw new Error('Selected original transfer identity is incomplete');
  if (selection.kind === 'shipments' && (typeof selection.key.order_id !== 'string' || !selection.key.order_id)) throw new Error('Selected original shipment order is incomplete');
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
  if (selection.kind === 'shipments' && row.order_id !== selection.key.order_id) throw new Error('Selected original shipment order changed');
  const spec = rowWork(selection.kind, row, role, scope.warehouse).find(spec => spec.type === selection.commandType);
  if (!spec) throw new Error('This action is unavailable for the selected record or current role');
  const recordField = { orders: 'order_id', receipts: 'receipt_id', exceptions: 'exception_id', transfers: 'transfer_id', shipments: 'shipment_id' }[selection.kind];
  let identities = selection.kind === 'lots' ? { sku: row.sku, lot: row.lot } : selection.kind === 'stocks'
    ? { sku: row.sku, lot: row.lot, [spec.type === 'move_stock' ? 'from' : 'location']: row.location }
    : { [recordField]: row.id };
  if (['receive', 'pick', 'unpick', 'receive_transfer'].includes(spec.type)) {
    const line = (spec.type === 'receive_transfer' ? remainingTransferLines(row) : selection.kind === 'orders' ? remainingOrderLines(row, spec.type) : row.lines).find(line => line.id === selection.lineId);
    if (!line) throw new Error('Select an exact original line before recording this work');
    identities.line_id = line.id;
  } else if (selection.lineId !== undefined) throw new Error('This action does not take a selected line');
  if (spec.type === 'release_lot_hold') {
    const hold = row.control?.holds.find(hold => hold.id === selection.holdId);
    if (!hold) throw new Error('Select an exact original hold before recording its release');
    identities = { hold_id: hold.id };
  } else if (selection.holdId !== undefined) throw new Error('This action does not take a selected hold');
  if (spec.type === 'dispatch_transfer') identities = {};
  return { spec, identities, source: row };
}

export async function reviewSelectedWork(client, snapshot, scope, selection, role) {
  checkSelection(snapshot, scope, selection);
  if (selection.kind === 'shipments') {
    const page = await shipmentReview(client, scope, selection.key.order_id, snapshot.version);
    return selectedWork({ ...snapshot, shipments: page.packages }, scope, selection, role);
  }
  if (selection.kind !== 'transfers') {
    const chosen = selectedWork(snapshot, scope, selection, role);
    if (chosen.spec.type === 'pack_shipment') chosen.shipments = await shipmentReview(client, scope, chosen.source.id, snapshot.version);
    if (chosen.spec.type === 'create_return') {
      const page = await client.read(client.warehousePath(scope.warehouse, `/orders/${encodeURIComponent(chosen.source.id)}/return-eligibility`));
      if (page.order_id !== chosen.source.id || page.version !== snapshot.version || !Array.isArray(page.lines)) throw new Error('Return source review changed; refresh and select again');
      chosen.returns = page;
    }
    return chosen;
  }
  if (selection.commandType !== 'receive_transfer' || !visibleCommands(role).some(spec => spec.type === selection.commandType)) throw new Error('This action is unavailable for the selected record or current role');
  const transfer = await readSelectedTransfer(client, scope.warehouse, selection.key, snapshot.version);
  return selectedWork({ ...snapshot, transfers: [transfer] }, scope, selection, role);
}

export async function shipmentReview(client, scope, order, version) {
  const page = await client.read(client.warehousePath(scope.warehouse, `/orders/${encodeURIComponent(order)}/shipments`));
  if (page.warehouse_id !== scope.warehouse || page.order_id !== order || version !== undefined && page.version !== version || page.ok !== true || !Array.isArray(page.packages)) throw new Error('Shipment source review changed or is held; refresh and select again');
  return page;
}
