#!/usr/bin/env node
import { homedir } from 'node:os';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { Client, originUrl, parseJson, safeText, tokenFromEnv } from './lib/client.mjs';
import { Journal, boundSession, sendOriginal } from './lib/journal.mjs';
import { commandForm, visibleCommands, needsSerialPlan } from './lib/commands.mjs';
import { Terminal } from './lib/terminal.mjs';
import { inspectJson, inspectRows, overviewLines } from './lib/views.mjs';

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
const help = `StateSet Warehouse TUI — Node.js >=22.12
Usage: stateset-warehouse-tui [--url ORIGIN] [--warehouse ID] [--state-dir DIRECTORY]
       stateset-warehouse-tui --once session|warehouses|inventory|orders|receipts|reconciliation|recovery

Set WMS_URL (default http://127.0.0.1:8088) and WMS_TOKEN or WMS_TOKEN_FILE.
Use an individual operator credential. Credentials are never saved in recovery files.
Recovery defaults to $XDG_STATE_HOME/stateset-warehouse or ~/.local/state/stateset-warehouse.
Commands record whole each units and actually observed serial labels.
Uncertain work keeps its exact original request until resolved. The repository's
cli/README.md describes recovery and terminal workflows. q exits menus; Ctrl-C exits safely.
Home: i inventory, o orders, w work, c recovery, r refresh (then Enter).
Menus: /text searches, / clears, ? shows help. Tables: row number opens full
details; n/p page, j shows page JSON, r refreshes; Enter returns.`;
function scopeFor(client, session, warehouse) { return { origin: client.origin, tenant: session.identity.tenant, actor: session.identity.actor, warehouse }; }
async function selectWarehouse(client, ui) {
  let after = '';
  for (;;) {
    const page = await client.read(`/v1/warehouses?limit=100${after ? `&after=${encodeURIComponent(after)}` : ''}`);
    const items = page.warehouses.map(({ warehouse: row }) => [`${row.code} — ${row.name} (${row.id})`, row.id]);
    if (page.next_after) items.push(['Next page', '__next__']);
    const choice = await ui.menu('Select facility', items);
    if (choice === '__next__') { after = page.next_after; continue; }
    return choice;
  }
}
export async function collectSerials(client, warehouse, snapshot, command, ask) {
  if (!needsSerialPlan.has(command.type)) return [];
  const planId = randomUUID();
  const reply = await client.call('POST', client.warehousePath(warehouse, '/serial-observation-plan'), { plan_id: planId, command });
  if (!reply.ok) throw new Error(`Observation plan held: HTTP ${reply.status}; ${safeText(reply.data.error ?? 'unknown error')}`);
  const plan = reply.data;
  if (plan.plan_id !== planId || plan.warehouse_id !== warehouse || plan.command_type !== command.type || plan.observed_warehouse_version !== snapshot.version || !Array.isArray(plan.targets) || plan.targets.length > 100 || !Number.isSafeInteger(plan.required_labels) || plan.required_labels > 10000) throw new Error('Observation plan changed or does not match this reviewed work; review again');
  const inputs = []; const seen = new Set(); let total = 0;
  for (const target of plan.targets) {
    if (!Number.isSafeInteger(target.each_quantity) || target.each_quantity < 0 || total + target.each_quantity > 10000 || !Array.isArray(target.target) || target.target.length > 3 || !target.target.every(v => typeof v === 'string') || typeof target.sku !== 'string') throw new Error('Malformed observation target');
    total += target.each_quantity;
    const serials = [];
    if (!target.each_quantity && await ask(`Confirm physically observed zero labels for ${safeText(target.sku)} ${safeText(JSON.stringify(target.target))} (yes/no): `) !== 'yes') throw new Error('Empty count was not physically confirmed');
    for (let i = 0; i < target.each_quantity; i++) {
      const label = await ask(`Scan actual ${safeText(target.sku)} ${safeText(JSON.stringify(target.target))} label ${i + 1}/${target.each_quantity}: `);
      if (label === null || !label || label.trim() !== label || Buffer.byteLength(label) > 240 || /[\x00-\x1f\x7f-\x9f]/.test(label)) throw new Error('Invalid literal label; no request sent');
      const id = JSON.stringify([target.sku, label]);
      if (seen.has(id)) throw new Error('Duplicate literal serial observation; no request sent');
      seen.add(id); serials.push(label);
    }
    inputs.push({ target: target.target, sku: target.sku, serials });
  }
  if (total !== plan.required_labels) throw new Error('Observation plan total disagrees');
  return inputs;
}
async function perform(client, journal, scope, ui, supplied) {
  if ((await journal.listForIdentity(scope)).some(record => !['committed', 'not_committed'].includes(record.stage))) throw new Error('Resolve your original pending work in Recovery before recording new work');
  const session = await boundSession(client, scope);
  let snapshot = await client.read(client.warehousePath(scope.warehouse));
  let command = supplied;
  if (!command) {
    const spec = await ui.menu('Record warehouse work', visibleCommands(session.identity.role).map(c => [c.label, c]));
    if (!spec) return;
    ui.screen(spec.label);
    ui.line(`Facility: ${scope.warehouse} | reviewed v${snapshot.version}`);
    ui.line('Enter observed values. Esc cancels; Ctrl-C exits.', 'muted');
    command = await commandForm(spec, prompt => ui.ask(prompt), message => ui.line(message, 'warning'));
  }
  if (session.identity.role === 'oms' && command.type === 'create_order' && !command.source_order_id) throw new Error('OMS orders require an original source_order_id');
  if (command.type === 'create_warehouse') {
    scope = { ...scope, warehouse: `wh-${randomUUID()}` };
    snapshot = { version: 0 };
    ui.print(`New facility identity: ${scope.warehouse} | reviewed version 0`);
  }
  const serial_inputs = await collectSerials(client, scope.warehouse, snapshot, command, prompt => ui.ask(prompt));
  const request = { request_id: randomUUID(), expected_version: snapshot.version, command, ...(serial_inputs.length ? { serial_inputs } : {}) };
  if (!await inspectJson(ui, `Review actual work | ${scope.warehouse} | v${snapshot.version}`, { scope, request }, { prompt: '\nn next | p previous | Enter to confirm: ' })) return;
  ui.line(`Original request: ${request.request_id}`);
  if (await ui.ask('Record this observed work? Type yes: ') !== 'yes') return;
  const record = await journal.prepare(scope, request);
  const outcome = await sendOriginal(client, journal, record);
  await inspectJson(ui, `Recovery status: ${outcome.stage} | ${request.request_id}`, outcome.events.at(-1).details, { prompt: '\nn next | p previous | Enter to continue: ' });
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
async function readView(client, scope, ui, kind) {
  const base = client.warehousePath(scope.warehouse);
  if (['stocks', 'products', 'locations', 'orders', 'receipts', 'exceptions', 'activity', 'movements'].includes(kind)) {
    await inspectRows(ui, kind, async () => {
      const snapshot = await client.read(base);
      const title = { stocks: 'Inventory', locations: 'Bins', receipts: 'Receiving / returns' }[kind] ?? kind;
      return { title: `${title} | ${scope.warehouse} | snapshot v${snapshot.version}`, rows: snapshot[kind] };
    }); return;
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
    const type = kind === 'order-detail' ? 'orders' : 'receipts';
    const record = await client.read(`${base}/${type}/${encodeURIComponent(id)}`);
    if (type === 'orders') {
      const shipments = await client.read(`${base}/orders/${encodeURIComponent(id)}/shipments`);
      const returnEligibility = await client.read(`${base}/orders/${encodeURIComponent(id)}/return-eligibility`);
      await inspectJson(ui, `Original order | ${id}`, { record, shipments, returnEligibility });
    } else await inspectJson(ui, `Original receiving record | ${id}`, record);
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
    const endpoint = { reconciliation: `${base}/reconciliation`, transfers: `${base}/transfers?limit=100`, lots: `${base}/lots?limit=100`, operations: '/v1/operations/health' }[kind];
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
      const action = await ui.menu('Warehouse overview', [
        ['Record observed warehouse work', 'work'], ['Inventory', 'stocks'], ['Products', 'products'], ['Bins', 'locations'], ['Orders', 'orders'], ['Receiving / returns', 'receipts'], ['Original order and shipment review', 'order-detail'], ['Original receiving record', 'receipt-detail'], ['Lots and holds', 'lots'], ['Facility transfers', 'transfers'], ['Serial custody', 'serials'], ['Serial history', 'serial-history'], ['Exceptions', 'exceptions'], ['Retained history', 'history'], ['Reconciliation', 'reconciliation'], ['Operations', 'operations'], ['Original request recovery', 'recovery'], ['iCommerce interop', 'interop'], ['Select facility', 'warehouse'],
      ], {
        intro: overviewLines(snapshot, pending, session, warehouse),
        sections: { 1: 'WORK & INVENTORY', 9: 'TRACEABILITY & CONTROL', 16: 'SYSTEM & RECOVERY' },
        shortcuts: { i: 'stocks', o: 'orders', w: 'work', c: 'recovery', r: 'refresh' },
        hint: ['i inventory · o orders · w work · c recovery · r refresh', '/text search · ? help · q exit'],
      });
      if (!action) break;
      try {
        if (action === 'refresh') continue;
        if (action === 'work') warehouse = await perform(client, journal, scope, ui) ?? warehouse;
        else if (action === 'recovery') await recover(client, journal, scope, ui);
        else if (action === 'warehouse') warehouse = await selectWarehouse(client, ui) ?? warehouse;
        else if (action === 'interop') await interop(client, journal, scope, ui);
        else await readView(client, scope, ui, action);
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
