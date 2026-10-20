import { fit, wrap, width } from './format.mjs';
import { safeText } from './client.mjs';
import { filtersFor, sortsFor, tableView } from './queues.mjs';

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
  lots: [column('SKU', 'sku', 18), column('Lot', 'lot', 18), column('Availability', 'availability', 15), column('Physical', 'physical', 9, true), column('Reserved', 'reserved', 9, true), column('Usable', 'usable', 9, true)],
  transfers: [column('Reference', 'reference', 18), column('Check', row => row.ok === true ? 'ok' : 'held', 7), column('Source', 'source_warehouse_id', 18), column('Destination', 'destination_warehouse_id', 18), column('Lines', row => row.lines.length, 6, true)],
  shipments: [column('Reference', 'reference', 20), column('State', row => row.closure?.kind ?? 'packed', 14), column('Lines', row => row.lines.length, 6, true), column('Package', 'id', 36)],
  movements: [column('SKU', 'sku', 20), column('Kind', 'kind', 16), column('Each', 'quantity', 9, true), column('From', 'from', 14), column('To', 'to', 14), column('Reference', 'reference', 20)],
  activity: [column('Kind', 'kind', 24), column('Reference', 'reference', 24), column('Detail', 'detail', 32), column('Actor', 'actor', 24)],
};
function tableLayout(kind, rows, columns, offset = 0) {
  const numberSize = Math.max(2, String(offset + rows.length).length);
  const render = values => values.join('  ').trimEnd();
  if (kind === 'stocks') {
    const hand = rows.reduce((n, row) => Math.max(n, String(row.on_hand).length), 7);
    const reserved = rows.reduce((n, row) => Math.max(n, String(row.reserved).length), 8);
    const identity = columns - numberSize - 8 - hand - reserved;
    if (identity >= 10) {
      const bin = Math.min(14, Math.max(4, Math.floor(identity / 3)));
      const sku = Math.min(20, identity - bin); const lot = identity - bin - sku - 2;
      const values = (number, skuValue, binValue, handValue, reservedValue, lotValue) => render([fit(number, numberSize, true), fit(skuValue, sku), fit(binValue, bin), fit(handValue, hand, true), fit(reservedValue, reserved, true), ...(lot >= 8 ? [fit(lotValue, lot)] : [])]);
      return { rowHeight: 1, header: [values('#', 'SKU', 'Bin', 'On hand', 'Reserved', 'Lot'), '─'.repeat(columns)], row: (row, index) => [values(index + 1, row.sku, row.location, row.on_hand, row.reserved, row.lot)] };
    }
    const separate = rows.some(row => String(row.on_hand).length + String(row.reserved).length + 3 > columns);
    return {
      rowHeight: separate ? 3 : 2,
      header: [fit('# SKU / bin', columns).trimEnd(), ...wrap(separate ? 'H on hand · R reserved (each)' : 'On hand / Reserved (each)', columns), '─'.repeat(columns)],
      row: (row, index) => [fit(`${index + 1} ${row.sku} @ ${row.location}`, columns).trimEnd(), ...(separate ? [`H ${row.on_hand}`, `R ${row.reserved}`] : [`${row.on_hand} / ${row.reserved}`])],
    };
  }
  if (kind === 'lots' && (columns < 100 || rows.some(row => ['physical', 'reserved', 'usable'].some(key => String(row[key]).length > 9)))) {
    return {
      header: [fit('# SKU / lot', columns).trimEnd(), ...wrap('H physical · R reserved · U usable (each)', columns), '─'.repeat(columns)],
      row: (row, index) => {
        const facts = []; let line = '';
        for (const field of [row.availability ?? '?', `H ${row.physical}`, `R ${row.reserved}`, `U ${row.usable}`]) {
          if (line && width(`${line} · ${field}`) > columns) { facts.push(...wrap(line, columns)); line = ''; }
          line = line ? `${line} · ${field}` : field;
        }
        facts.push(...wrap(line, columns));
        return [fit(`${index + 1} ${row.sku} | ${row.lot}`, columns).trimEnd(), ...facts];
      },
    };
  }
  const available = columns - numberSize - 2;
  let chosen = []; let used = 0;
  for (const col of schemas[kind] ?? [column('Record', row => JSON.stringify(row), 60)]) {
    const size = Math.min(col.size, Math.max(8, available - used));
    if (used + size > available) break;
    chosen.push({ ...col, size }); used += size + 2;
  }
  return { rowHeight: 1, header: [render([fit('#', numberSize, true), ...chosen.map(col => fit(col.label, col.size, col.numeric))]), '─'.repeat(columns)], row: (row, index) => [render([fit(index + 1, numberSize, true), ...chosen.map(col => fit(col.value(row), col.size, col.numeric))])] };
}
export function tableLines(kind, rows, columns, offset = 0) {
  const layout = tableLayout(kind, rows, columns, offset);
  return [...layout.header, ...rows.flatMap((row, index) => layout.row(row, offset + index))];
}
export async function inspectJson(ui, title, value, options) {
  return inspectText(ui, title, JSON.stringify(value, null, 2), options);
}
export async function inspectText(ui, title, raw, { prompt = '\nn next | p previous | Enter back: ', allowUndo = false, details } = {}) {
  let offset = 0; let draft; let viewport;
  for (;;) {
    const shownTitle = ui.rows < 28 ? fit(title, ui.columns).trimEnd() : title;
    // Track source positions, rather than wrapped-line numbers, across reflow.
    // The anchor is display text only; original request values stay untouched.
    let position = 0; const positions = [];
    const lines = raw.split('\n').flatMap(line => {
      const wrapped = wrap(line, ui.columns);
      for (const part of wrapped) { positions.push(position); position += part.length; }
      position++; return wrapped;
    });
    if (viewport && viewport.columns !== ui.columns) offset = positions.findLastIndex(start => start <= viewport.position);
    const count = Math.max(1, ui.rows - 5 - wrap(shownTitle, ui.columns).length - (allowUndo ? wrap('F2 correct last observation before submission', ui.columns).length : 0) - (details ? 1 : 0) - prompt.split('\n').reduce((total, line) => total + wrap(line, ui.columns).length, 0));
    offset = Math.min(offset, Math.max(0, lines.length - 1));
    viewport = { columns: ui.columns, position: positions[offset] };
    ui.screen(shownTitle); lines.slice(offset, offset + count).forEach(line => ui.print(line));
    ui.line(`Lines ${offset + 1}–${Math.min(offset + count, lines.length)} of ${lines.length}`);
    if (allowUndo) ui.line('F2 correct last observation before submission', 'muted');
    if (details) ui.line('j complete original JSON', 'muted');
    const answer = await (ui.action ? ui.action(prompt, draft) : ui.ask(prompt)); draft = undefined;
    if (answer && typeof answer === 'object') {
      if (answer.key === 'resize') { draft = answer.draft; continue; }
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
    if (answer === 'j' && details) await inspectJson(ui, 'Complete original data', details);
    else if (answer === 'n' && offset + count < lines.length) offset += count;
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
t work queues · r refresh · q exit · F1 or ? help
a allocate · k pick · f pack · g ship · d inbound
b putaway · e issues · u priority · z pause/resume auto reads
l opens active partial/package fulfillment, including after a first handoff.
Home reads refresh every 15s while idle. Typing pauses reads.
Task/review views have no automatic reads. Writes require a fresh review.
v shows complete context when a compact screen shortens it.
Pane resizing retains unfinished input and your reading position.

TABLES
↑ / ↓ selects a row; Enter opens its complete details.
A displayed row number also opens details.
n / p page; j opens page JSON; r refreshes the view.
a opens actions for stock, orders, receipts, exceptions, transfers or lots.
F3 or f opens filters; F4 or s opens sort choices.
Changing filters or sorting clears the highlighted row.
Lots/transfers: n/p crosses server batches at page boundaries.
Search, filters and sorting apply to the displayed batch only.
Refresh returns to the first batch; each batch is a fresh read.
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
Work/outcome summaries: j opens the complete original JSON.
Quantity fields accept pack for an actual reviewed pack code/count.
Selected orders offer partial packages, handoffs and original returns.
Order row actions also review remaining fulfillment and original packages.
Partial work keeps exact line ownership; released/cancelled demand stays distinct.
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
export async function inspectRows(ui, kind, load, { onAction, filter: initialFilter = 'all', sort: initialSort = 'source', serverPaged = false } = {}) {
  let snapshot = await load(null); let query = ''; let page = 0; let message = ''; let selected = null;
  let draft; let viewport;
  let readError = ''; let readAt = new Date().toISOString();
  let cursors = [null]; let batch = 0;
  let filterId = initialFilter; let sortId = initialSort;
  const read = async cursor => {
    const next = await load(cursor);
    readError = ''; readAt = new Date().toISOString();
    return next;
  };
  const held = error => {
    selected = null; readError = safeText(error.message);
    message = `Read held: ${readError}. Current batch retained; retry or refresh.`;
  };
  for (;;) {
    const all = snapshot.rows;
    const view = tableView(kind, snapshot, { filter: filterId, sort: sortId, query });
    const filtered = view.rows;
    const settings = filterId !== 'all' || sortId !== 'source' ? `Filter: ${view.filter.label} · Sort: ${view.sort.label}` : '';
    const range = (start, end) => `${start}–${end} of ${filtered.length} rows${query || filterId !== 'all' ? ` · ${all.length} total${query ? ` · /${query}` : ''}` : ''}`;
    const batchInfo = serverPaged ? `Batch ${batch + 1} · ${snapshot.nextCursor === null ? 'last batch' : 'more on server'} · search/filter/sort apply to this batch` : '';
    const compact = Boolean(ui.keys) && ui.rows < 28;
    let footer = [`Row # details · /text search${serverPaged ? ' batch' : ''} · / clear · r refresh`, `n next · p previous · j page JSON · ${selected === null ? 'Enter back' : 'Esc back'}`];
    if (ui.keys) footer.push(selected === null ? '↑↓ select · PgUp/PgDn page · Esc back' : '↑↓ select · Enter details · Esc back');
    if (ui.keys) footer.push('F3 / f filters · F4 / s sort');
    if (onAction) footer.push('a actions for selected row · quantities stay explicit');
    if (compact) footer = [`↑↓ Enter details · ${onAction ? 'a work' : 'j JSON'} · Esc back`, '/ find · f filter · s sort · n/p · r · v info'].map(line => fit(line, ui.columns).trimEnd());
    const empty = all.length ? filterId === 'all' ? 'No matches. Enter / to clear the search.' : 'No rows in this filter. Use f to change; / clears search.' : 'No records in this snapshot.';
    const readStatus = readError ? 'STALE · r refresh before work' : '';
    const compactMeta = [readStatus, batchInfo, settings, snapshot.note].filter(Boolean).map(line => fit(line, ui.columns).trimEnd());
    const reserved = compact ? 10 + compactMeta.length + (message ? 1 : 0) + (!filtered.length ? 1 : 0) : 8 + wrap(snapshot.title, ui.columns).length + wrap(range(filtered.length, filtered.length), ui.columns).length
      + footer.reduce((total, line) => total + wrap(line, ui.columns).length, 0)
      + (message ? wrap(message, ui.columns).length : 0) + (settings ? wrap(settings, ui.columns).length : 0) + (!filtered.length ? wrap(empty, ui.columns).length : 0)
      + (readStatus ? wrap(readStatus, ui.columns).length : 0)
      + (batchInfo ? wrap(batchInfo, ui.columns).length : 0) + (snapshot.note ? wrap(snapshot.note, ui.columns).length : 0);
    const cursorPrefix = ui.keys ? '  ' : '';
    const layout = tableLayout(kind, filtered, ui.columns - cursorPrefix.length);
    const rowHeight = layout.rowHeight ?? filtered.reduce((height, row, index) => Math.max(height, layout.row(row, index).length), 1);
    const count = Math.max(1, Math.floor((ui.rows - reserved - layout.header.length + 2) / rowHeight));
    if (viewport && selected === null && (viewport.columns !== ui.columns || viewport.rows !== ui.rows)) page = Math.floor(viewport.offset / count);
    if (selected !== null) page = Math.floor(selected / count);
    page = Math.min(page, Math.max(0, Math.ceil(filtered.length / count) - 1));
    const offset = page * count; const rows = filtered.slice(offset, offset + count);
    viewport = { columns: ui.columns, rows: ui.rows, offset };
    ui.screen(compact ? fit(`${kind} | v${snapshot.version ?? '?'} | ${snapshot.warehouseId ?? snapshot.title}`, ui.columns).trimEnd() : snapshot.title);
    ui.line(compact ? fit(range(filtered.length ? offset + 1 : 0, offset + rows.length), ui.columns).trimEnd() : range(filtered.length ? offset + 1 : 0, offset + rows.length));
    if (compact) compactMeta.forEach(line => ui.line(line, 'muted'));
    else {
      if (readStatus) ui.line(readStatus, 'danger');
      if (batchInfo) ui.line(batchInfo, 'muted');
      if (snapshot.note) ui.line(snapshot.note, 'muted');
      if (settings) ui.line(settings, 'muted');
    }
    layout.header.forEach(line => ui.print(ui.style ? ui.style(cursorPrefix + line, 'accent') : cursorPrefix + line));
    rows.forEach((row, index) => {
      const tone = selected === offset + index ? 'selected'
        : row?.priority === 'urgent' || row?.availability === 'expired' || row?.ok === false ? 'danger'
          : row?.status === 'open' || ['quarantined', 'expiry_missing', 'invalid'].includes(row?.availability) ? 'warning' : ['shipped', 'received'].includes(row?.status) ? 'success' : '';
      layout.row(row, offset + index).forEach((line, part) => {
        const value = (ui.keys && selected === offset + index && part === 0 ? '› ' : cursorPrefix) + line;
        ui.print(ui.style ? ui.style(value, tone) : value);
      });
    });
    if (!rows.length) ui.line(compact ? fit(empty, ui.columns).trimEnd() : empty);
    footer.forEach(line => ui.line(line, 'muted'));
    if (message) ui.line(compact ? fit(message, ui.columns).trimEnd() : message);
    const movePage = async direction => {
      selected = null; message = '';
      const lastPage = Math.max(0, Math.ceil(filtered.length / count) - 1);
      if (direction > 0 && page < lastPage || direction < 0 && page > 0) { page += direction; viewport = undefined; return; }
      if (!serverPaged || direction > 0 && snapshot.nextCursor === null || direction < 0 && batch === 0) return;
      try {
        const cursor = direction > 0 ? snapshot.nextCursor : cursors[batch - 1];
        if (direction > 0 && cursors.slice(0, batch + 1).includes(cursor)) throw new Error('Repeated server cursor; refresh to read again');
        const next = await read(cursor);
        if (direction > 0) { cursors = [...cursors.slice(0, batch + 1), cursor]; batch++; }
        else batch--;
        snapshot = next; page = direction > 0 ? 0 : Number.MAX_SAFE_INTEGER; viewport = undefined;
      } catch (error) { held(error); }
    };
    let answer = await (ui.action ? ui.action('\nView: ', draft) : ui.ask('\nView: ')); draft = undefined;
    if (answer && typeof answer === 'object') {
      if (answer.key === 'resize') { draft = answer.draft; continue; }
      if (answer.key === 'escape') return;
      if (answer.key === 'f1') { await keyboardHelp(ui); continue; }
      if (answer.key === 'f3' || answer.key === 'f4') answer = answer.key === 'f3' ? 'f' : 's';
      else {
        if (['left', 'pageup', 'right', 'pagedown'].includes(answer.key)) {
          await movePage(['left', 'pageup'].includes(answer.key) ? -1 : 1);
        } else if (['up', 'down', 'home', 'end'].includes(answer.key) && filtered.length) {
          if (selected === null) selected = offset;
          else selected = Math.max(0, Math.min(filtered.length - 1, selected + (answer.key === 'up' ? -1 : answer.key === 'down' ? 1 : 0)));
          if (answer.key === 'home') selected = 0;
          if (answer.key === 'end') selected = filtered.length - 1;
          page = Math.floor(selected / count);
        }
        continue;
      }
    }
    if (answer === '' && selected !== null && filtered[selected]) answer = String(selected + 1);
    if (!answer || answer === 'q') return;
    message = '';
    if (answer === 'n') await movePage(1);
    else if (answer === 'p') await movePage(-1);
    else if (answer.startsWith('/')) { query = answer.slice(1); page = 0; selected = null; viewport = undefined; }
    else if (answer === 'r') {
      selected = null;
      try { snapshot = await read(null); cursors = [null]; batch = 0; page = 0; viewport = undefined; }
      catch (error) { held(error); }
    }
    else if (answer === 'f' || answer === 's') {
      const choices = answer === 'f' ? filtersFor(kind, snapshot) : sortsFor(kind);
      const chosen = await ui.menu(answer === 'f' ? 'Filter records' : 'Sort records', choices.map(choice => [choice.label, choice.id]));
      if (chosen !== null) {
        if (answer === 'f') filterId = chosen; else sortId = chosen;
        selected = null; page = 0; viewport = undefined;
      }
    }
    else if (answer === 'a' && onAction) {
      if (readError) { message = 'Refresh successfully before opening work from this retained view.'; continue; }
      if (selected === null || !filtered[selected]) { message = 'Highlight a row with the arrow keys before opening actions.'; continue; }
      const row = filtered[selected]; selected = null;
      try {
        await onAction(row, snapshot.version);
        snapshot = await read(serverPaged ? cursors[batch] : null); page = 0; viewport = undefined;
      } catch (error) { held(error); }
    }
    else if (answer === 'j') await inspectJson(ui, `${snapshot.title} | page JSON`, rows);
    else if (answer === 'v') await inspectText(ui, 'View context', [snapshot.title, readStatus, `Last successful read: ${readAt}`, readError && `Read held: ${readError}`, batchInfo, settings, snapshot.note, `Search: /${query}`, message].filter(Boolean).join('\n'));
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
    const at = statuses => snapshot.orders.filter(order => statuses.includes(order.status)).length;
    lines.push(`Allocate ${at(['pending'])} · Pick ${at(['allocated', 'picking'])} · Pack ${at(['picked'])} · Ship ${at(['packed'])}`);
    lines.push(`Priority work ${snapshot.orders.filter(order => !['shipped', 'cancelled', 'closed_partial'].includes(order.status) && ['high', 'urgent'].includes(order.priority)).length} · Quarantined lots ${(snapshot.lot_controls ?? []).filter(lot => lot.holds.length).length}`);
    lines.push(`Snapshot at ${snapshot.readAt} · r refresh`);
  }
  lines.push(pending.error ? `Recovery check held: ${pending.error}` : pending.count ? `ACTION: ${pending.count} pending original request(s) · c recovery` : 'Recovery: no pending original requests for your identity');
  return lines.map(line => safeText(line));
}
