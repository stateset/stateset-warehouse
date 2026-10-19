import { fit, wrap } from './format.mjs';
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
export async function inspectJson(ui, title, value, options) {
  return inspectText(ui, title, JSON.stringify(value, null, 2), options);
}
export async function inspectText(ui, title, raw, { prompt = '\nn next | p previous | Enter back: ', allowUndo = false } = {}) {
  let offset = 0;
  for (;;) {
    const lines = raw.split('\n').flatMap(line => wrap(line, ui.columns));
    const count = Math.max(1, ui.rows - 5 - wrap(title, ui.columns).length - (allowUndo ? wrap('F2 correct last observation before submission', ui.columns).length : 0) - prompt.split('\n').reduce((total, line) => total + wrap(line, ui.columns).length, 0));
    offset = Math.min(offset, Math.max(0, lines.length - 1));
    ui.screen(title); lines.slice(offset, offset + count).forEach(line => ui.print(line));
    ui.line(`Lines ${offset + 1}–${Math.min(offset + count, lines.length)} of ${lines.length}`);
    if (allowUndo) ui.line('F2 correct last observation before submission', 'muted');
    const answer = await (ui.action ? ui.action(prompt) : ui.ask(prompt));
    if (answer && typeof answer === 'object') {
      if (answer.key === 'escape') return false;
      if (answer.key === 'f2' && allowUndo) return 'undo';
      if (answer.key === 'up') offset = Math.max(0, offset - 1);
      if (answer.key === 'down') offset = Math.min(Math.max(0, lines.length - count), offset + 1);
      if (['pageup', 'left'].includes(answer.key)) offset = Math.max(0, offset - count);
      if (['pagedown', 'right'].includes(answer.key)) offset = Math.min(Math.max(0, lines.length - count), offset + count);
      if (answer.key === 'home') offset = 0;
      if (answer.key === 'end') offset = Math.max(0, lines.length - count);
      continue;
    }
    if (answer === 'n' && offset + count < lines.length) offset += count;
    else if (answer === 'p') offset = Math.max(0, offset - count);
    else if (answer === null || answer === 'q') return false;
    else if (answer === '') return true;
  }
}
export async function keyboardHelp(ui) {
  return inspectText(ui, 'Keyboard guide', `NAVIGATION
↑ / ↓ or Tab / Shift-Tab  Move through menu actions
Enter                    Open the highlighted action
Home / End               First / last action
PgUp / PgDn or ← / →      Change page
/text then Enter         Search; / then Enter clears
Number then Enter        Open the original numbered action
Esc                      Return; cancel a form or review
Ctrl-C                   Exit, preserving original requests

HOME SHORTCUTS (then Enter)
i inventory · o orders · w observed work · c recovery
r refresh · q exit · F1 or ? help

TABLES
↑ / ↓ selects a row; Enter opens its complete details.
A displayed row number also opens details.
n / p page; j opens page JSON; r refreshes the view.
a opens actions for a highlighted stock, order or receipt.
Enter returns when no row is selected; Esc always returns.
Detail views support arrows, Home/End and paging keys.

OBSERVED WORK
Scan actual literal labels; case and spaces are preserved.
Duplicate or malformed scans can be retried without losing earlier scans.
F2 at an empty scan field undoes the previous observation.
F2 in the unsent request review corrects its last observation.
Paste one field at a time. Multi-line paste is ignored.
Review the complete paged request, then explicitly type yes.
Esc before submission cancels entry; it cannot undo sent work.
Recover uncertain work using its complete original request.
Arrow keys never record a physical warehouse action.`);
}
export function scanProgress(ui, state, scope, version, command = {}) {
  ui.screen('Capture actual serial observations');
  ui.line(`Facility: ${scope.warehouse} | reviewed v${version}`);
  ui.line(`Captured ${state.observed}/${state.required} labels | group ${state.targetIndex + 1}/${state.targetCount}`, 'accent');
  if (command.from) ui.line(`Move: ${command.from} → ${command.to} | lot ${command.lot}`);
  else if (command.location) ui.line(`Bin: ${command.location}${command.lot ? ` | lot ${command.lot}` : ''}`);
  ui.line(state.target.each_quantity === 0 ? 'Physically confirm this empty target.' : 'Scan each actual label exactly as observed.', 'muted');
  ui.line('F2 undo previous observation · empty field required', 'muted');
  ui.line('Esc cancels entry before submission', 'muted');
  if (state.latest.length) {
    ui.line('Recent accepted labels:', 'muted');
    for (const row of state.latest) ui.line(fit(`${row.sku} | ${row.serial}`, ui.columns).trimEnd());
  }
  if (state.message) ui.line(state.message, 'warning');
}
export async function inspectRows(ui, kind, load, { onAction } = {}) {
  let snapshot = await load(); let query = ''; let page = 0; let message = ''; let selected = null;
  for (;;) {
    const all = snapshot.rows;
    const filtered = query ? all.filter(row => JSON.stringify(row).toLowerCase().includes(query.toLowerCase())) : all;
    const range = (start, end) => `${start}–${end} of ${filtered.length} rows${query ? ` · ${all.length} total · /${query}` : ''}`;
    const footer = ['Row # details · /text search · / clear · r refresh', `n next · p previous · j page JSON · ${selected === null ? 'Enter back' : 'Esc back'}`];
    if (ui.keys) footer.push(selected === null ? '↑↓ select · PgUp/PgDn page · Esc back' : '↑↓ select · Enter details · Esc back');
    if (onAction) footer.push('a actions for selected row · quantities stay explicit');
    const empty = all.length ? 'No matches. Enter / to clear the search.' : 'No records in this snapshot.';
    const reserved = 8 + wrap(snapshot.title, ui.columns).length + wrap(range(filtered.length, filtered.length), ui.columns).length
      + footer.reduce((total, line) => total + wrap(line, ui.columns).length, 0)
      + (message ? wrap(message, ui.columns).length : 0) + (!filtered.length ? wrap(empty, ui.columns).length : 0);
    const count = Math.max(1, ui.rows - reserved);
    if (selected !== null) page = Math.floor(selected / count);
    page = Math.min(page, Math.max(0, Math.ceil(filtered.length / count) - 1));
    const offset = page * count; const rows = filtered.slice(offset, offset + count);
    ui.screen(snapshot.title);
    ui.line(range(filtered.length ? offset + 1 : 0, offset + rows.length));
    tableLines(kind, rows, ui.columns, offset).forEach((line, index) => {
      const row = rows[index - 2];
      const tone = index === 0 ? 'accent' : index === 1 ? 'muted' : selected === offset + index - 2 ? 'selected'
        : row?.priority === 'urgent' ? 'danger' : row?.status === 'open' ? 'warning' : ['shipped', 'received'].includes(row?.status) ? 'success' : '';
      ui.print(ui.style ? ui.style(line, tone) : line);
    });
    if (!rows.length) ui.line(empty);
    footer.forEach(line => ui.line(line, 'muted'));
    if (message) ui.line(message);
    let answer = await (ui.action ? ui.action('\nView: ') : ui.ask('\nView: '));
    if (answer && typeof answer === 'object') {
      if (answer.key === 'escape') return;
      if (answer.key === 'f1') { await keyboardHelp(ui); continue; }
      if (['left', 'pageup', 'right', 'pagedown'].includes(answer.key)) {
        page = Math.max(0, Math.min(page + (['left', 'pageup'].includes(answer.key) ? -1 : 1), Math.max(0, Math.ceil(filtered.length / count) - 1)));
        selected = null;
      } else if (['up', 'down', 'home', 'end'].includes(answer.key) && filtered.length) {
        if (selected === null) selected = offset;
        else selected = Math.max(0, Math.min(filtered.length - 1, selected + (answer.key === 'up' ? -1 : answer.key === 'down' ? 1 : 0)));
        if (answer.key === 'home') selected = 0;
        if (answer.key === 'end') selected = filtered.length - 1;
        page = Math.floor(selected / count);
      }
      continue;
    }
    if (answer === '' && selected !== null && filtered[selected]) answer = String(selected + 1);
    if (!answer || answer === 'q') return;
    message = '';
    if (answer === 'n') { page = Math.min(page + 1, Math.max(0, Math.ceil(filtered.length / count) - 1)); selected = null; }
    else if (answer === 'p') { page = Math.max(0, page - 1); selected = null; }
    else if (answer.startsWith('/')) { query = answer.slice(1); page = 0; selected = null; }
    else if (answer === 'r') { snapshot = await load(); page = 0; selected = null; }
    else if (answer === 'a' && onAction) {
      if (selected === null || !filtered[selected]) { message = 'Highlight a row with the arrow keys before opening actions.'; continue; }
      await onAction(filtered[selected], snapshot.version);
      snapshot = await load(); selected = null; page = 0;
    }
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
