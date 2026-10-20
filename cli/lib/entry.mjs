import { commandForm } from './commands.mjs';

const key = values => JSON.stringify(values);
const cancelled = () => { const error = new Error('Entry cancelled'); error.code = 'INPUT_CANCELLED'; throw error; };
const choose = async (ui, title, rows) => {
  if (!rows.length) throw new Error('No remaining original sources for this work');
  const choice = await ui.menu(title, rows.map(row => [row.label, row]));
  if (choice === null) cancelled();
  return choice;
};
export function lineSources(snapshot, chosen, command) {
  const used = identity => command.lines.reduce((sum, line) => sum + (Object.entries(identity).every(([field, value]) => line[field] === value) ? line.quantity : 0), 0);
  let rows = [];
  if (command.type === 'dispatch_transfer') rows = snapshot.stocks.map(row => ({ identities: { sku: row.sku, lot: row.lot, location: row.location }, maximum: row.on_hand - row.reserved, label: `${row.sku} | ${row.lot} | ${row.location}` }));
  if (command.type === 'create_return') rows = chosen.returns.lines.map(row => ({ identities: { order_line_id: row.order_line_id, lot: row.lot }, maximum: row.remaining, label: `${row.order_line_id} | ${row.sku} | ${row.lot}` }));
  if (command.type === 'pack_shipment') rows = chosen.source.allocations.map(row => {
    const identities = { line_id: row.line_id, location: row.location, lot: row.lot };
    const packed = chosen.shipments.packages.filter(pack => pack.closure === null).flatMap(pack => pack.lines).filter(line => Object.entries(identities).every(([field, value]) => line[field] === value)).reduce((sum, line) => sum + line.quantity, 0);
    return { identities, maximum: row.picked - row.shipped - packed, label: `${row.line_id} | ${row.sku} | ${row.lot} | ${row.location}` };
  });
  return rows.map(row => ({ ...row, maximum: row.maximum - used(row.identities) })).filter(row => Number.isSafeInteger(row.maximum) && row.maximum > 0).map(row => ({ ...row, label: `${row.label} | ${row.maximum} each remaining` }));
}

export async function collectWork(client, ui, scope, snapshot, spec, chosen, selectFacility) {
  const inputs = new Map();
  const ask = prompt => prompt === 'destination_warehouse_id: ' && selectFacility ? selectFacility() : ui.ask(prompt);
  const quantity = async ({ values, command, row, minimum, value }) => {
    const draft = row === null ? { ...command, ...values } : command;
    const target = row === null ? [] : spec.type === 'pack_shipment' ? [values.line_id, values.location, values.lot] : [values.id];
    const targetKey = key(target); inputs.delete(targetKey);
    if (value !== 'pack') return;
    const order = snapshot.orders.find(order => order.id === draft.order_id);
    const sku = values.sku ?? order?.lines.find(line => line.id === (values.line_id ?? values.order_line_id))?.sku
      ?? snapshot.receipts.find(receipt => receipt.id === draft.receipt_id)?.lines.find(line => line.id === values.line_id)?.sku
      ?? (spec.type === 'receive_transfer' ? chosen?.source.lines.find(line => line.id === values.line_id)?.sku : undefined);
    if (!sku) { ui.line('Pack entry requires a reviewed original SKU. Enter each units for this form.', 'warning'); return null; }
    ui.line(`Actual packs for ${sku}; EA returns to each entry.`, 'accent');
    for (const pack of (snapshot.unit_packs ?? []).filter(pack => pack.sku === sku)) ui.line(`${pack.code}: ${pack.label} | ${pack.each_per_pack} each`);
    for (;;) {
      const code = await ui.ask('Scan pack code (EA cancels): '); if (code === null) throw new Error('Input ended; no request sent');
      if (code === 'EA') return null;
      if (!/^[A-Z0-9_-]{1,32}$/.test(code)) { ui.line('Scan an exact enabled pack code from this reviewed SKU.', 'warning'); continue; }
      const page = await client.read(client.warehousePath(scope.warehouse, `/unit-packs?sku=${encodeURIComponent(sku)}&code=${encodeURIComponent(code)}&review=${snapshot.version}&limit=100`));
      if (page.warehouse_id !== scope.warehouse || page.sku !== sku || page.code !== code || page.version !== snapshot.version || page.page_source_ok !== true || page.next_cursor !== null || !Array.isArray(page.items)) throw new Error('Pack definitions changed or are held; refresh the reviewed work');
      const pack = page.items.find(pack => pack.enabled === true && pack.source_ok === true && pack.code === code);
      if (!pack || typeof pack.id !== 'string' || !Number.isSafeInteger(pack.each_per_pack) || pack.each_per_pack < 1) { ui.line('Scan an exact enabled pack code from this reviewed SKU.', 'warning'); continue; }
      for (;;) {
        const count = await ui.ask('Observed pack count: '); if (count === null) throw new Error('Input ended; no request sent');
        if (!/^\d+$/.test(count) || !Number.isSafeInteger(Number(count)) || Number(count) < minimum || Number(count) > 1_000_000) { ui.line(`Enter a whole observed pack count from ${minimum} to 1000000.`, 'warning'); continue; }
        const each = BigInt(count) * BigInt(pack.each_per_pack);
        if (each > 1_000_000n) { ui.line('Converted quantity exceeds 1000000 each; observe a smaller work quantity.', 'warning'); continue; }
        inputs.set(targetKey, { target, sku, pack_code: code, definition_id: pack.id, pack_quantity: Number(count), each_per_pack: pack.each_per_pack });
        ui.line(`Observed ${count} ${code} × ${pack.each_per_pack} = ${each} each`, 'accent');
        return Number(each);
      }
    }
  };
  const guided = chosen && ['dispatch_transfer', 'pack_shipment', 'create_return'].includes(spec.type);
  const rowSource = guided ? async ({ command, index }) => {
    const rows = lineSources(snapshot, chosen, command);
    if (spec.type === 'dispatch_transfer' && index === 0) {
      const original = rows.find(row => ['sku', 'lot', 'location'].every(field => row.identities[field] === chosen.source[field]));
      if (!original) throw new Error('Selected stock has no unreserved units for dispatch');
      ui.line(original.label); return original;
    }
    return choose(ui, 'Select original quantity source', rows);
  } : undefined;
  const command = await commandForm(spec, ask, message => ui.line(message, 'warning'), chosen?.identities, { quantity, rowSource });
  return { command, unit_inputs: [...inputs.values()] };
}
