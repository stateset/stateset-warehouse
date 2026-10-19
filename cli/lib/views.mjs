import { fit, wrap } from './terminal.mjs';
import { safeText } from './client.mjs';

// Columns are in priority order: narrow panes retain identity and operational
// quantities. Full original values are always available in the row detail.
const column = (label, key, size, numeric = false) => ({ label, value: typeof key === 'function' ? key : row => row[key], size, numeric });
const schemas = {
  stocks: [column('SKU', 'sku', 20), column('Bin', 'location', 14), column('On hand', 'on_hand', 9, true), column('Reserved', 'reserved', 9, true), column('Lot', 'lot', 18)],
  products: [column('SKU', 'sku', 20), column('Name', 'name', 30), column('Serial', row => row.serial_controlled ? 'yes' : 'no', 6), column('Reorder', 'reorder_point', 9, true), column('Category', 'category', 18)],
  locations: [column('Bin', 'code', 18), column('Zone', 'zone', 18), column('Kind', 'kind', 14)],
  orders: [column('Reference', 'reference', 18), column('Status', 'status', 15), column('Priority', 'priority', 8), column('Customer', 'customer', 26), column('Lines', row => row.lines.length, 6, true)],
  receipts: [column('Reference', 'reference', 18), column('Status', 'status', 15), column('Supplier', 'supplier', 26), column('Lines', row => row.lines.length, 6, true)],
  exceptions: [column('Category', 'category', 18), column('Status', 'status', 10), column('Detail', 'detail', 40), column('ID', 'id', 36)],
  movements: [column('SKU', 'sku', 20), column('Kind', 'kind', 16), column('Each', 'quantity', 9, true), column('From', 'from', 14), column('To', 'to', 14), column('Reference', 'reference', 20)],
  activity: [column('Kind', 'kind', 24), column('Reference', 'reference', 24), column('Detail', 'detail', 32), column('Actor', 'actor', 24)],
};
export function tableLines(kind, rows, columns, offset = 0) {
  const numberSize = Math.max(2, String(offset + rows.length).length);
  const available = columns - numberSize - 2;
  let chosen = []; let used = 0;
  for (const col of schemas[kind] ?? [column('Record', row => JSON.stringify(row), 60)]) {
    const size = Math.min(col.size, Math.max(8, available - used));
    if (used + size > available) break;
    chosen.push({ ...col, size }); used += size + 2;
  }
  const render = values => values.join('  ').trimEnd();
  return [
    render([fit('#', numberSize, true), ...chosen.map(col => fit(col.label, col.size, col.numeric))]),
    '─'.repeat(columns),
    ...rows.map((row, index) => render([fit(offset + index + 1, numberSize, true), ...chosen.map(col => fit(col.value(row), col.size, col.numeric))])),
  ];
}
export async function inspectJson(ui, title, value) {
  const raw = JSON.stringify(value, null, 2); let offset = 0;
  for (;;) {
    const lines = raw.split('\n').flatMap(line => wrap(line, ui.columns));
    const count = Math.max(1, ui.rows - 8);
    offset = Math.min(offset, Math.max(0, lines.length - 1));
    ui.screen(title); lines.slice(offset, offset + count).forEach(line => ui.print(line));
    ui.line(`Lines ${offset + 1}–${Math.min(offset + count, lines.length)} of ${lines.length}`);
    const answer = await ui.ask('\nn next | p previous | Enter back: ');
    if (answer === 'n' && offset + count < lines.length) offset += count;
    else if (answer === 'p') offset = Math.max(0, offset - count);
    else if (!answer || answer === 'q') return;
  }
}
export async function inspectRows(ui, kind, load) {
  let snapshot = await load(); let query = ''; let page = 0; let message = '';
  for (;;) {
    const all = snapshot.rows;
    const filtered = query ? all.filter(row => JSON.stringify(row).toLowerCase().includes(query.toLowerCase())) : all;
    const count = Math.max(1, ui.rows - 13);
    page = Math.min(page, Math.max(0, Math.ceil(filtered.length / count) - 1));
    const offset = page * count; const rows = filtered.slice(offset, offset + count);
    ui.screen(snapshot.title);
    ui.line(`${filtered.length ? offset + 1 : 0}–${offset + rows.length} of ${filtered.length} rows${query ? ` · ${all.length} total · /${query}` : ''}`);
    tableLines(kind, rows, ui.columns, offset).forEach(line => ui.print(line));
    if (!rows.length) ui.line(all.length ? 'No matches. Enter / to clear the search.' : 'No records in this snapshot.');
    ui.line('Row # details · /text search · / clear · r refresh');
    ui.line('n next · p previous · j page JSON · Enter back');
    if (message) ui.line(message);
    const answer = await ui.ask('\nView: ');
    if (!answer || answer === 'q') return;
    message = '';
    if (answer === 'n') page = Math.min(page + 1, Math.max(0, Math.ceil(filtered.length / count) - 1));
    else if (answer === 'p') page = Math.max(0, page - 1);
    else if (answer.startsWith('/')) { query = answer.slice(1); page = 0; }
    else if (answer === 'r') { snapshot = await load(); page = 0; }
    else if (answer === 'j') await inspectJson(ui, `${snapshot.title} | page JSON`, rows);
    else if (/^[1-9]\d*$/.test(answer) && Number(answer) > offset && Number(answer) <= offset + rows.length) {
      await inspectJson(ui, `${snapshot.title} | row ${answer}`, filtered[Number(answer) - 1]);
    } else message = 'Choose a row number on this page, or one of the view keys.';
  }
}

const sum = (rows, key) => rows.reduce((total, row) => total + BigInt(row[key]), 0n).toString();
export function overviewLines(snapshot, pending, session, warehouse) {
  const identity = session.identity;
  const lines = [
    `Operator: ${identity.actor} (${identity.role})`,
    `Facility: ${warehouse}`,
    `Tenant: ${identity.tenant}${session.demo ? ' | SYNTHETIC DEMO' : ''}`,
  ];
  if (snapshot.error) lines.push(`Snapshot unavailable: ${snapshot.error}`);
  else {
    const openOrders = snapshot.orders.filter(row => !['shipped', 'cancelled', 'closed_partial'].includes(row.status)).length;
    const incoming = snapshot.receipts.filter(row => !['received', 'cancelled'].includes(row.status)).length;
    const exceptions = snapshot.exceptions.filter(row => row.status === 'open').length;
    lines.push(`${snapshot.warehouse.code} · ${snapshot.warehouse.name} · v${snapshot.version}`);
    lines.push(`On hand ${sum(snapshot.stocks, 'on_hand')} ea · Reserved ${sum(snapshot.stocks, 'reserved')} ea`);
    lines.push(`Open orders ${openOrders} · Incoming ${incoming} · Exceptions ${exceptions}`);
    lines.push(`Snapshot at ${snapshot.readAt} · r refresh`);
  }
  lines.push(pending.error ? `Recovery check held: ${pending.error}` : pending.count ? `ACTION: ${pending.count} pending original request(s) · c recovery` : 'Recovery: no pending original requests for your identity');
  return lines.map(line => safeText(line));
}
