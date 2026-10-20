#!/usr/bin/env node
import { homedir } from 'node:os';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { Client, originUrl, parseJson, safeText, tokenFromEnv } from './lib/client.mjs';
import { Journal, boundSession, sendOriginal } from './lib/journal.mjs';
import { visibleCommands } from './lib/commands.mjs';
import { Terminal } from './lib/terminal.mjs';
import { inspectJson, inspectText, inspectRows, overviewLines, scanProgress } from './lib/views.mjs';
import { rowWork, reviewSelectedWork, selectionFor, remainingTransferLines, shipmentReview } from './lib/workflows.mjs';
import { observationSession } from './lib/scanning.mjs';
import { workQueues } from './lib/queues.mjs';
import { readTablePage } from './lib/pages.mjs';
import { readOrderReview } from './lib/reviews.mjs';
import { workSummary, outcomeSummary } from './lib/summaries.mjs';
import { collectWork } from './lib/entry.mjs';

export function argumentsFor(argv) {
  const result = {};
  for (let i = 0; i < argv.length; i++) {
    const name = argv[i];
    if (name === '--help' || name === '-h') { result.help = true; continue; }
    if (!['--url', '--warehouse', '--state-dir', '--once'].includes(name) || !argv[i + 1] || argv[i + 1].startsWith('--')) throw new Error(`Unknown or incomplete option: ${safeText(name)}`);
    if (result[name.slice(2)] !== undefined) throw new Error(`Repeated option: ${name}`);
    result[name.slice(2)] = argv[++i];
  }
  return result;
}
const help = `StateSet WMS operations center for Ghostty — Node.js >=22.12
Usage: stateset-warehouse-tui [--url ORIGIN] [--warehouse ID] [--state-dir DIRECTORY]
       stateset-warehouse-tui --once session|warehouses|inventory|orders|receipts|reconciliation|recovery

Set WMS_URL (default http://127.0.0.1:8088) and WMS_TOKEN or WMS_TOKEN_FILE.
Use an individual operator credential. Credentials are never saved in recovery files.
Recovery defaults to $XDG_STATE_HOME/stateset-warehouse or ~/.local/state/stateset-warehouse.
Commands record whole each units and actually observed serial labels.
Uncertain work keeps its exact original request until resolved. The repository's
cli/README.md describes recovery and terminal workflows. q exits menus; Ctrl-C exits safely.
Home: i inventory, o orders, w work, c recovery, r refresh (then Enter).
      t work queues. Tables: F3/f filters; F4/s sorting.
Compact panes: v shows full context. Reviews: j shows original JSON.
Quantity fields: pack scans an actual reviewed packaging code and count.
Menus: /text searches, / clears, ? shows help. Tables: row number opens full
details; a opens highlighted row actions, n/p page, j shows page JSON,
r refreshes; Enter returns when no row is selected. Esc returns or cancels.`;
function scopeFor(client, session, warehouse) { return { origin: client.origin, tenant: session.identity.tenant, actor: session.identity.actor, warehouse }; }
async function selectWarehouse(client, ui, exclude) {
  const nextPage = Symbol('next facility page');
  let after = '';
  for (;;) {
    const page = await client.read(`/v1/warehouses?limit=100${after ? `&after=${encodeURIComponent(after)}` : ''}`);
    const items = page.warehouses.filter(({ warehouse }) => warehouse.id !== exclude).map(({ warehouse: row }) => [`${row.code} — ${row.name} (${row.id})`, row.id]);
    if (page.next_after) items.push(['Next page', nextPage]);
    const choice = await ui.menu('Select facility', items);
    if (choice === nextPage) { after = page.next_after; continue; }
    return choice;
  }
}
export { collectSerials } from './lib/scanning.mjs';
async function perform(client, journal, scope, ui, supplied, selection) {
  if ((await journal.listForIdentity(scope)).some(record => !['committed', 'not_committed'].includes(record.stage))) throw new Error('Resolve your original pending work in Recovery before recording new work');
  const session = await boundSession(client, scope);
  let snapshot = await client.read(client.warehousePath(scope.warehouse));
  let command = supplied; let unit_inputs = [];
  if (!command) {
    const chosen = selection ? await reviewSelectedWork(client, snapshot, scope, selection, session.identity.role) : null;
    const spec = chosen?.spec ?? await ui.menu('Record warehouse work', visibleCommands(session.identity.role).map(c => [c.label, c]));
    if (!spec) return;
    ui.screen(spec.label);
    ui.line(`Facility: ${scope.warehouse} | reviewed v${snapshot.version}`);
    ui.line('Enter observed values. Esc cancels; Ctrl-C exits.', 'muted');
    for (const [name, value] of Object.entries(chosen?.identities ?? {})) ui.line(`Selected ${name}: ${value}`);
    ui.line('Quantity: enter each units, or pack to scan a packaging code.', 'muted');
    const collected = await collectWork(client, ui, scope, snapshot, spec, chosen, async () => {
      const warehouse = await selectWarehouse(client, ui, scope.warehouse);
      if (warehouse === null) { const error = new Error('Destination selection cancelled'); error.code = 'INPUT_CANCELLED'; throw error; }
      return warehouse;
    });
    command = collected.command; unit_inputs = collected.unit_inputs;
  }
  if (session.identity.role === 'oms' && command.type === 'create_order' && !command.source_order_id) throw new Error('OMS orders require an original source_order_id');
  if (command.type === 'create_warehouse') {
    scope = { ...scope, warehouse: `wh-${randomUUID()}` };
    snapshot = { version: 0 };
    ui.print(`New facility identity: ${scope.warehouse} | reviewed version 0`);
  }
  const observations = await observationSession(client, scope.warehouse, snapshot, command);
  const capture = () => observations.capture(prompt => ui.scan(prompt), { retry: true, progress: state => scanProgress(ui, state, scope, snapshot.version, command) });
  await capture();
  const requestId = randomUUID(); let request;
  for (;;) {
    const serial_inputs = observations.inputs;
    request = { request_id: requestId, expected_version: snapshot.version, command, ...(serial_inputs.length ? { serial_inputs } : {}), ...(unit_inputs.length ? { unit_inputs } : {}) };
    const review = await inspectText(ui, `Review actual work | ${scope.warehouse} | v${snapshot.version}`, workSummary(scope, request), { details: { scope, request }, prompt: '\nn next | p previous | Enter to confirm: ', allowUndo: observations.canUndo });
    if (review === 'undo') { observations.undo(); await capture(); continue; }
    if (!review) return;
    break;
  }
  ui.line(`Original request: ${request.request_id}`);
  if (await ui.ask('Record this observed work? Type yes: ') !== 'yes') return;
  const record = await journal.prepare(scope, request);
  const outcome = await sendOriginal(client, journal, record);
  await inspectText(ui, `Recovery status: ${outcome.stage} | ${request.request_id}`, outcomeSummary(outcome), { details: outcome.events.at(-1).details, prompt: '\nn next | p previous | Enter to continue: ' });
  if (command.type === 'create_warehouse' && outcome.stage === 'committed') return scope.warehouse;
}
async function recover(client, journal, scope, ui) {
  const records = await journal.listForIdentity(scope);
  const record = await ui.menu('Original request recovery — all your facilities', records.map(r => [`${r.stage} | ${r.original.scope.warehouse} | ${r.request.command.type} | ${r.request.request_id}`, r]));
  if (!record) return;
  if (!await inspectJson(ui, 'Original request — preserved scope and complete body', { scope: record.original.scope, request: record.request, stage: record.stage })) return;
  if (['committed', 'not_committed'].includes(record.stage)) { await inspectJson(ui, `Original outcome: ${record.stage}`, record.events.at(-1).details); return; }
  const action = await ui.menu('Recovery action', [['Look up original actor-owned command receipt', 'lookup'], ['Resend complete exact original request', 'retry']]);
  if (!action) return;
  if (action === 'lookup') {
    await boundSession(client, record.original.scope);
    const receipt = await client.read(client.warehousePath(record.original.scope.warehouse, `/commands/${encodeURIComponent(record.request.request_id)}`));
    if (receipt.found === true && receipt.result) {
      await journal.append(record, 'committed', { receipt }); ui.print('Original command receipt found and preserved.');
    } else ui.print('No receipt found. Outcome remains uncertain; preserve the original request.');
  } else if (await ui.ask('Resend the exact original request? Type yes: ') === 'yes') {
    const outcome = await sendOriginal(client, journal, record);
    await inspectJson(ui, `Original recovery status: ${outcome.stage}`, outcome.events.at(-1).details); return;
  }
  await ui.pause();
}
async function inspectOriginal(client, scope, ui, kind, id) {
  if (kind === 'orders') {
    await inspectJson(ui, `Original order | ${id}`, await readOrderReview(client, scope.warehouse, id));
  } else await inspectJson(ui, `Original receiving record | ${id}`, await client.read(client.warehousePath(scope.warehouse, `/${kind}/${encodeURIComponent(id)}`)));
}
async function selectedActions(client, journal, scope, ui, kind, row, version) {
  const session = await boundSession(client, scope);
  const work = rowWork(kind, row, session.identity.role, scope.warehouse);
  const action = await ui.menu('Selected record actions', [
    ['Review complete original record', 'review'],
    ...work.map(spec => [spec.label, spec]),
    ...(kind === 'exceptions' && row.order_id ? [['Review linked order and shipments', 'linked-order']] : []),
    ...(kind === 'orders' ? [['Shipment packages and handoffs', 'packages']] : []),
  ], { intro: [kind === 'lots' ? `${row.sku} | ${row.lot}` : kind === 'stocks' ? `${row.sku} | ${row.lot} | ${row.location}` : `${kind === 'exceptions' ? row.category : row.reference} | ${row.id}`, `Selected facility: ${scope.warehouse} | snapshot v${version}`] });
  if (!action) return;
  if (action === 'review') {
    if (['stocks', 'exceptions', 'transfers', 'lots', 'shipments'].includes(kind)) await inspectJson(ui, `Original selected ${kind === 'stocks' ? 'stock row' : kind === 'exceptions' ? 'exception' : kind === 'lots' ? 'lot' : kind === 'shipments' ? 'shipment package' : 'transfer'}`, row);
    else await inspectOriginal(client, scope, ui, kind, row.id);
    return;
  }
  if (action === 'linked-order') { await inspectOriginal(client, scope, ui, 'orders', row.order_id); return; }
  if (action === 'packages') {
    await inspectRows(ui, 'shipments', async () => {
      const page = await shipmentReview(client, scope, row.id);
      return { title: `Shipment packages | ${row.reference} | v${page.version}`, rows: page.packages, version: page.version, warehouseId: scope.warehouse };
    }, { onAction: (pack, version) => selectedActions(client, journal, scope, ui, 'shipments', pack, version) });
    return;
  }
  let lineId;
  if (['receive', 'pick', 'unpick', 'receive_transfer'].includes(action.type)) {
    const lines = action.type === 'receive_transfer' ? remainingTransferLines(row) : row.lines;
    lineId = await ui.menu('Select original line', lines.map(line => [
      `${line.id} | ${line.sku}${line.lot ? ` | ${line.lot}` : ''} | ${action.type === 'receive_transfer' ? `in transit ${line.quantity - line.received - line.lost}` : `ordered ${line.quantity}`} each`, line.id,
    ]));
    if (lineId === null) return;
  }
  let holdId;
  if (action.type === 'release_lot_hold') {
    holdId = await ui.menu('Select original hold', row.control.holds.map(hold => [`${hold.id} | ${hold.reason}`, hold.id]));
    if (holdId === null) return;
  }
  try { await perform(client, journal, scope, ui, undefined, selectionFor(scope, kind, row, version, action.type, lineId, holdId)); }
  catch (error) { if (error.code !== 'INPUT_CANCELLED') throw error; }
}
async function readView(client, scope, ui, kind, journal, settings = {}) {
  const base = client.warehousePath(scope.warehouse);
  if (['lots', 'transfers'].includes(kind)) {
    await inspectRows(ui, kind, cursor => readTablePage(client, scope.warehouse, kind, cursor), {
      serverPaged: true,
      onAction: (row, version) => selectedActions(client, journal, scope, ui, kind, row, version),
    }); return;
  }
  if (['stocks', 'products', 'locations', 'orders', 'receipts', 'exceptions', 'activity', 'movements'].includes(kind)) {
    await inspectRows(ui, kind, async () => {
      const snapshot = await client.read(base);
      const title = { stocks: 'Inventory', locations: 'Bins', receipts: 'Receiving / returns' }[kind] ?? kind;
      return { title: `${title} | ${scope.warehouse} | snapshot v${snapshot.version}`, rows: snapshot[kind], version: snapshot.version, locations: snapshot.locations, warehouseId: scope.warehouse };
    }, { filter: settings.filter, sort: settings.sort, onAction: ['stocks', 'orders', 'receipts', 'exceptions'].includes(kind)
      ? (row, version) => selectedActions(client, journal, scope, ui, kind, row, version) : undefined }); return;
  }
  if (kind === 'serials' || kind === 'serial-history') {
    const sku = await ui.ask('Exact SKU: '); const serial = await ui.ask('Exact serial (empty for page): ');
    if (!sku || serial === null) return;
    if (kind === 'serial-history' && !serial) throw new Error('Serial history requires the original literal serial');
    let after = '';
    for (;;) {
      const page = await client.read(`/v1/${kind}?warehouse_id=${encodeURIComponent(scope.warehouse)}&sku=${encodeURIComponent(sku)}${serial ? `&serial=${encodeURIComponent(serial)}` : ''}&limit=100${after ? `&after=${encodeURIComponent(after)}` : ''}`);
      if (!await inspectJson(ui, `${kind} | ${scope.warehouse}`, page)) return;
      if (!page.next_cursor || await ui.ask('Next page? (yes/no): ') !== 'yes') break;
      after = page.next_cursor;
    }
    return;
  } else if (kind === 'order-detail' || kind === 'receipt-detail') {
    const id = await ui.ask('Original record ID: '); if (!id) return;
    await inspectOriginal(client, scope, ui, kind === 'order-detail' ? 'orders' : 'receipts', id);
    return;
  } else if (kind === 'history') {
    const section = await ui.menu('Retained history', ['orders', 'receipts', 'movements', 'activity', 'exceptions'].map(s => [s, s])); if (!section) return;
    let before = '';
    for (;;) {
      const page = await client.read(`${base}/history/${section}?limit=100${before ? `&before=${before}` : ''}`);
      if (!await inspectJson(ui, `Retained ${section} | ${scope.warehouse}`, page)) return;
      if (!page.next_cursor || await ui.ask('Next page? (yes/no): ') !== 'yes') break;
      before = page.next_cursor;
    }
    return;
  } else {
    const endpoint = { reconciliation: `${base}/reconciliation`, operations: '/v1/operations/health' }[kind];
    await inspectJson(ui, `${kind} | ${scope.warehouse}`, await client.read(endpoint));
  }
}
async function interop(client, journal, scope, ui) {
  const choice = await ui.menu('iCommerce interoperability', [['Preview an upstream MoveInventory or CreateInventoryItem', 'preview'], ['Export actual iCommerce inventory with explicit location IDs', 'inventory']]);
  if (!choice) return;
  ui.print(choice === 'preview' ? 'Enter the strict preview JSON (kind, input and mappings/category). Decimal quantities must be strings.' : 'Enter mappings JSON: {"locations":{"7":"RECV-01",...},"lots":{}}');
  const raw = await ui.ask('JSON: '); if (!raw) return;
  parseJson(raw); // Reject unsafe numeric precision without rewriting wire fields.
  const response = await client.call('POST', client.warehousePath(scope.warehouse, `/icommerce/${choice}`), raw);
  if (!await inspectJson(ui, `iCommerce ${choice} | HTTP ${response.status}`, response.data)) return;
  if (!response.ok) return;
  if (choice === 'preview' && await ui.ask('Review this canonical command for actual execution? (yes/no): ') === 'yes') {
    // Refresh the execution review visibly; never translate an uncertain request.
    await perform(client, journal, scope, ui, response.data.command);
  }
}
export async function main(argv = process.argv.slice(2), env = process.env) {
  const args = argumentsFor(argv); if (args.help) { console.log(help); return; }
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (major < 22 || major === 22 && minor < 12) throw new Error('Node.js >=22.12 is required');
  const client = new Client(originUrl(args.url ?? env.WMS_URL ?? 'http://127.0.0.1:8088'), await tokenFromEnv(env));
  let session = await client.session(); let warehouse = args.warehouse ?? env.WMS_WAREHOUSE ?? session.default_warehouse_id;
  if (!warehouse) throw new Error('Select an explicit warehouse');
  const journal = new Journal(args['state-dir'] ?? env.WMS_TUI_STATE_DIR ?? path.join(env.XDG_STATE_HOME ?? path.join(homedir(), '.local/state'), 'stateset-warehouse'));
  if (args.once) {
    const scope = scopeFor(client, session, warehouse); let data;
    if (args.once === 'session') data = session;
    else if (args.once === 'warehouses') data = await client.read('/v1/warehouses?limit=100');
    else if (args.once === 'recovery') data = (await journal.listForIdentity(scope)).map(r => ({ warehouse_id: r.original.scope.warehouse, request_id: r.request.request_id, command_type: r.request.command.type, stage: r.stage }));
    else if (args.once === 'reconciliation') data = await client.read(client.warehousePath(warehouse, '/reconciliation'));
    else if (['inventory', 'orders', 'receipts'].includes(args.once)) { const snapshot = await client.read(client.warehousePath(warehouse)); data = { scope, version: snapshot.version, items: snapshot[args.once === 'inventory' ? 'stocks' : args.once] }; }
    else throw new Error('Unknown --once view; see --help');
    console.log(JSON.stringify(data, null, 2)); return;
  }
  if (!process.stdin.isTTY || !process.stdout.isTTY) throw new Error('Interactive mode requires a terminal; use --once for read-only automation');
  const ui = new Terminal(process.stdin, process.stdout, env);
  try {
    for (;;) {
      session = await client.session(); const scope = scopeFor(client, session, warehouse);
      // Read failures must leave Recovery and facility selection accessible.
      // The overview never supplies authority/version to a subsequent write.
      const [snapshot, pending] = await Promise.all([
        client.read(client.warehousePath(warehouse)).then(value => ({ ...value, readAt: new Date().toISOString() })).catch(error => ({ error: error.message })),
        journal.listForIdentity(scope).then(records => ({ count: records.filter(record => !['committed', 'not_committed'].includes(record.stage)).length })).catch(error => ({ error: error.message })),
      ]);
      const overview = overviewLines(snapshot, pending, session, warehouse);
      const action = await ui.menu('WMS operations center', [
        ['Record observed warehouse work', 'work'], ['Inventory', 'stocks'], ['Products', 'products'], ['Bins', 'locations'], ['Orders', 'orders'], ['Receiving / returns', 'receipts'], ['Original order and shipment review', 'order-detail'], ['Original receiving record', 'receipt-detail'], ['Lots and holds', 'lots'], ['Facility transfers', 'transfers'], ['Serial custody', 'serials'], ['Serial history', 'serial-history'], ['Exceptions', 'exceptions'], ['Retained history', 'history'], ['Reconciliation', 'reconciliation'], ['Operations', 'operations'], ['Original request recovery', 'recovery'], ['iCommerce interop', 'interop'], ['Select facility', 'warehouse'], ['Work queues', 'queues'],
      ], {
        intro: overview,
        compactIntro: [
          snapshot.error ? `Facility: ${warehouse}` : `${snapshot.warehouse.code} | v${snapshot.version} | ${snapshot.readAt.slice(11, 19)} UTC${session.demo ? ' | DEMO' : ''}`,
          ...overview.filter(line => line.startsWith('Open orders') || line.startsWith('Allocate') || line.startsWith('Priority work')),
        ],
        compactHint: ['t queues · i stock · o orders · w work', '↑↓ Enter · / find · r refresh · v info · q'],
        sections: { 1: 'WORK & INVENTORY', 9: 'TRACEABILITY & CONTROL', 16: 'OPERATIONS & RECOVERY' },
        shortcuts: { i: 'stocks', o: 'orders', w: 'work', c: 'recovery', r: 'refresh', t: 'queues' },
        hint: ['t queues · i inventory · o orders · w work', 'c recovery · r refresh · /text search · ? help · q exit'],
      });
      if (!action) break;
      try {
        if (action === 'refresh') continue;
        if (action === 'work') warehouse = await perform(client, journal, scope, ui) ?? warehouse;
        else if (action === 'recovery') await recover(client, journal, scope, ui);
        else if (action === 'warehouse') warehouse = await selectWarehouse(client, ui) ?? warehouse;
        else if (action === 'interop') await interop(client, journal, scope, ui);
        else if (action === 'queues') {
          const queue = await ui.menu('Work queues', workQueues(snapshot).map(queue => [`${queue.label} | ${queue.count ?? '?'} records`, queue]), {
            intro: [`Facility: ${warehouse}`, snapshot.error ? 'Counts unavailable. Choose a queue to retry its view.' : `Counts from snapshot v${snapshot.version}. Opens a fresh view.`],
          });
          if (queue) await readView(client, scope, ui, queue.kind, journal, queue);
        }
        else await readView(client, scope, ui, action, journal);
      } catch (error) {
        if (error.code === 'INPUT_CANCELLED') continue;
        ui.print(`\nHeld: ${safeText(error.message)}\nExisting original requests remain in Recovery.`); await ui.pause();
      }
    }
  } finally { ui.close(); }
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch(error => { console.error(`Warehouse TUI: ${safeText(error.message)}`); process.exitCode = 1; });
}
