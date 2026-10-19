// Typed terminal forms. The server remains authoritative for roles and rules.
const receipt = 'id sku lot quantity#';
const order = 'id sku quantity#';
const packed = 'line_id location lot quantity#';
const transfer = 'id sku lot location quantity#';
const returned = 'id order_line_id lot quantity#';
const specs = [
  ['receive', 'Receive actual goods', 'receipt_id line_id quantity# location'],
  ['move_stock', 'Move stock', 'sku lot from to quantity#'],
  ['pick', 'Pick an order line', 'order_id line_id location lot quantity#'],
  ['unpick', 'Return staged stock', 'order_id line_id location lot quantity# reason'],
  ['allocate_available', 'Allocate available stock', 'order_id'],
  ['allocate', 'Allocate an entire order', 'order_id'],
  ['release_unpicked', 'Release unpicked reservations', 'order_id reason'],
  ['reset_allocation', 'Reset allocation after recovery', 'order_id reason'],
  ['pack_shipment', 'Pack a partial shipment', 'order_id reference', ['lines', packed]],
  ['unpack_shipment', 'Unpack a shipment', 'shipment_id reason'],
  ['handoff_shipment', 'Record actual carrier handoff', 'shipment_id carrier handoff_reference'],
  ['pack', 'Pack a complete order', 'order_id'],
  ['ship', 'Record complete order handoff', 'order_id carrier handoff_reference'],
  ['create_receipt', 'Create supplier receiving work', 'reference supplier', ['lines', receipt]],
  ['create_return', 'Authorize a customer return', 'reference order_id reason', ['lines', returned]],
  ['cancel_receipt', 'Cancel unreceived receipt remainder', 'receipt_id reason'],
  ['create_order', 'Create fulfillment work', 'reference source_order_id? customer destination priority', ['lines', order]],
  ['cancel_order', 'Cancel an unpicked order', 'order_id reason'],
  ['cancel_remainder', 'Close all unshipped order demand', 'order_id reason'],
  ['dispatch_transfer', 'Dispatch a facility transfer', 'reference destination_warehouse_id carrier handoff_reference', ['lines', transfer]],
  ['receive_transfer', 'Receive an arrived facility transfer', 'transfer_id line_id quantity# location arrival_reference'],
  ['record_transfer_loss', 'Record transfer loss', 'transfer_id line_id quantity# incident_reference reason', null, 'admin'],
  ['reverse_transfer_loss', 'Reverse a capped transfer loss', 'loss_id quantity# reason', null, 'admin'],
  ['count_stock', 'Record complete physical count', 'sku lot location counted# reason', null, 'admin'],
  ['quarantine_lot', 'Quarantine a lot in this facility', 'sku lot reason'],
  ['record_lot_expiry', 'Record an exact lot expiry time', 'sku lot expires_at reason'],
  ['correct_lot_expiry', 'Correct a lot expiry time', 'sku lot expires_at? reason', null, 'admin'],
  ['release_lot_hold', 'Release an original lot hold', 'hold_id reason', null, 'admin'],
  ['set_product_expiry_policy', 'Set product expiry policy', 'sku required! reason', null, 'admin'],
  ['set_unit_pack', 'Define a whole each pack', 'sku code label each_per_pack# enabled! reason', null, 'admin'],
  ['create_product', 'Create a product', 'sku name category reorder_point# serial_controlled!', null, 'admin'],
  ['create_location', 'Create a bin', 'code zone kind', null, 'admin'],
  ['create_warehouse', 'Create a facility', 'code name city timezone', null, 'admin'],
  ['update_warehouse', 'Update facility details', 'code name city timezone', null, 'admin'],
  ['report_exception', 'Report an exception', 'order_id? category detail'],
  ['resolve_exception', 'Resolve an exception', 'exception_id resolution'],
];
export const commands = specs.map(([type, label, fields, rows, role]) => ({ type, label, fields, rows, role }));
export function visibleCommands(role) {
  return commands.filter(c => role === 'admin' || role === 'operator' && c.role !== 'admin' || role === 'oms' && ['create_order', 'cancel_order', 'cancel_remainder'].includes(c.type));
}
export const needsSerialPlan = new Set(['count_stock', 'move_stock', 'receive', 'pick', 'unpick', 'receive_transfer', 'record_transfer_loss', 'reverse_transfer_loss', 'create_return', 'pack_shipment', 'dispatch_transfer']);
export async function formFields(spec, ask) {
  const result = {};
  for (const field of spec.split(' ')) {
    const name = field.replace(/[#!?]$/, '');
    for (;;) {
      const choices = { priority: ['normal', 'high', 'urgent'], kind: ['storage', 'receiving'] }[name];
      const value = await ask(`${name}${choices ? ` (${choices.join('/')})` : field.endsWith('?') ? ' (optional)' : field.endsWith('#') ? ' (whole each)' : field.endsWith('!') ? ' (yes/no)' : ''}: `);
      if (value === null) throw new Error('Input ended; no request sent');
      if (field.endsWith('?') && value === '') { result[name] = null; break; }
      if (field.endsWith('#')) {
        const minimum = ['counted', 'reorder_point'].includes(name) ? 0 : 1;
        if (!/^\d+$/.test(value) || !Number.isSafeInteger(Number(value)) || Number(value) < minimum || Number(value) > 1_000_000) continue;
        result[name] = Number(value); break;
      }
      if (field.endsWith('!')) {
        if (!['yes', 'no'].includes(value)) continue;
        result[name] = value === 'yes'; break;
      }
      if (!value || value.trim() !== value || Buffer.byteLength(value) > 240 || /[\x00-\x1f\x7f-\x9f]/.test(value)) continue;
      if (choices && !choices.includes(value)) continue;
      if (name === 'timezone') {
        try { new Intl.DateTimeFormat('en', { timeZone: value }); } catch { continue; }
      }
      result[name] = value; break;
    }
  }
  return result;
}
export async function commandForm(spec, ask) {
  const command = { type: spec.type, ...await formFields(spec.fields, ask) };
  if (spec.rows) {
    const [field, rowSpec] = spec.rows; command[field] = [];
    for (let count = 0; count < 100; count++) {
      command[field].push(await formFields(rowSpec, ask));
      if (await ask('Add another line? (yes/no): ') !== 'yes') break;
    }
  }
  return command;
}
