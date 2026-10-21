import { boundSession } from './journal.mjs';
import { safeText } from './client.mjs';
import { fit, wrap } from './format.mjs';
import { inspectJson, inspectText } from './views.mjs';
import { flowLabels, readConsole, readProduct, readProductImage } from './console.mjs';
import { Raster, flowRaster, stockRaster, planRaster } from './raster.mjs';
import { code128 } from './code128.mjs';

export function labelRaster(value, { demo = false } = {}) {
  const barcode = code128(value); const modules = barcode.widths.reduce((sum, n) => sum + n, 20);
  const image = new Raster(modules * 2, 112, [255, 255, 255]); let x = 20;
  barcode.widths.forEach((width, index) => { if (!(index % 2)) image.rect(x, 12, width * 2, 88, [0, 0, 0]); x += width * 2; });
  if (demo) image.text('DEMO', 20, 103, [0, 0, 0]);
  return image;
}

export function boardLines(view, mode, selected = '') {
  const lines = [`Facility: ${view.warehouse_name}`, `Original facility: ${view.warehouse_id}`,
    `Physical: ${view.stock.on_hand_each} each | Reserved: ${view.stock.reserved_each} each`];
  if (mode === 'flow') {
    lines.push(...view.flow.map((row, index) => `${flowLabels[index]}: ${row.orders} orders`), `Open exceptions: ${view.open_exceptions}`);
  } else if (mode === 'stock') {
    lines.push(`${view.bins.items.length} bins in this page of ${view.bins.total}; each quantities are physical stock.`, 'Bars compare this page: cyan physical, amber reserved. They do not measure capacity.');
    view.bins.items.forEach((bin, index) => lines.push(`${index + 1}. ${bin.code} | ${bin.on_hand_each} physical / ${bin.reserved_each} reserved each`, `   ${bin.zone || 'No zone'} | ${bin.kind}`));
  } else {
    const plan = view.floor_plan;
    if (!plan.available) lines.push(plan.reason === 'not_configured' ? 'No floor plan is available for this facility.' : 'Floor-plan bin positions need review. Stock views remain available.');
    else {
      lines.push(`${plan.name} | ${plan.width} × ${plan.height} ${plan.unit}`, `${plan.mapped_bins} of ${plan.total_bins} bins mapped. Positions are configured; quantities come from the reviewed warehouse.`);
      const rows = selected ? plan.bins.map((bin, index) => ({ ...bin, number: index + 1 })).filter(bin => bin.code === selected) : plan.bins.map((bin, index) => ({ ...bin, number: index + 1 }));
      rows.forEach(bin => lines.push(`${bin.number}. ${bin.code} | ${bin.on_hand_each} physical / ${bin.reserved_each} reserved each`));
    }
  }
  return lines;
}

async function visualHelp(ui) {
  await inspectText(ui, 'Visual console keys', [
    'All views are read-only. Graphics supplement the exact quantities and original labels in the text.',
    'f flow · b stock · m configured plan · s product SKU · r fresh review · j original JSON',
    'n next text/bin page · p previous · arrows scroll text or pan a configured plan',
    'Plan: +/- zoom · /EXACT-BIN find · 0 fit and clear selection',
    'Product: l label · p photo · n next text page · b previous',
    'F1 or ? help · Esc or q back. Resize preserves the current entry and sends no warehouse command.',
    'A failed read keeps the old snapshot marked STALE. Refresh before relying on it for work.',
  ].join('\n'));
}

// Reserve the prompt and complete footer before paging exact source text.
// Resizing reflows locally and sends no HTTP or warehouse command.
export async function drawVisual(ui, { title, version, capturedAt, demo = false, stale = '', lines, page = 0, graphic, footer }) {
  const footerLines = footer.flatMap(line => wrap(line, ui.columns));
  const supported = !stale && ui.columns >= 60 && ui.rows >= 24 && await ui.graphics.detect();
  const imageRows = supported && graphic ? Math.max(4, Math.min(10, ui.rows - 20)) : 0;
  const content = lines.flatMap(line => wrap(safeText(line), ui.columns));
  const budget = Math.max(1, ui.rows - 5 - footerLines.length - 2 - imageRows - 1);
  const pages = Math.max(1, Math.ceil(content.length / budget)); page = Math.max(0, Math.min(page, pages - 1));
  ui.screen(title); ui.line(fit(`Snapshot v${version}`, ui.columns).trimEnd(), 'muted');
  ui.line(fit(stale ? `${demo ? 'DEMO · ' : ''}STALE: ${stale}` : `${demo ? 'SYNTHETIC DEMO · ' : ''}Captured ${capturedAt}`, ui.columns).trimEnd(), stale ? 'danger' : demo ? 'warning' : 'muted');
  if (imageRows) {
    const image = typeof graphic === 'function' ? graphic(Math.min(1600, ui.columns * 8), imageRows * 16) : graphic;
    const shown = image instanceof Raster
      ? await ui.graphics.show(image.bytes, { width: image.width, height: image.height, columns: ui.columns, rows: imageRows })
      : await ui.graphics.show(image, { columns: ui.columns, rows: imageRows, png: true });
    if (shown) for (let row = 0; row < imageRows; row++) ui.print();
  }
  for (const line of content.slice(page * budget, (page + 1) * budget)) ui.line(line);
  ui.line(`Page ${page + 1}/${pages}`, 'muted'); footerLines.forEach(line => ui.line(line, 'muted'));
  return { page, pages };
}

export async function visualBoard(client, scope, ui, initial = 'flow') {
  let mode = initial; let value; let stale = ''; let notice = ''; let page = 0; let history = [null]; let draft;
  let planView = { zoom: 1 }; let selected = '';
  const refresh = async (after = null, review = null) => {
    await boundSession(client, scope);
    const next = await readConsole(client, scope, { after, review });
    if (next.floor_plan.revision !== value?.floor_plan.revision) planView = { zoom: 1 };
    if (selected && !next.floor_plan.bins?.some(bin => bin.code === selected)) selected = '';
    value = next; stale = ''; notice = ''; page = 0;
  };
  ui.screen('Visual warehouse operations'); ui.line('Loading warehouse evidence…'); await refresh();
  for (;;) {
    let graphic;
    if (mode === 'flow') graphic = (width, height) => flowRaster(value, width, height);
    if (mode === 'stock') graphic = (width, height) => stockRaster(value.bins.items, width, height);
    if (mode === 'plan' && value.floor_plan.available) graphic = (width, height) => planRaster(value.floor_plan, width, height, { ...planView, selected });
    const footer = [mode === 'plan' ? 'arrows pan · +/- zoom · /BIN find · 0 fit' : 'n next · p previous · j details', 'f flow · b stock · m plan · s SKU · r refresh · ? help · q back'];
    const layout = await drawVisual(ui, { title: mode === 'plan' ? 'Configured floor plan' : mode === 'stock' ? 'Stock by bin' : 'Warehouse operations flow', version: value.version, capturedAt: value.captured_at, demo: value.demo, stale, lines: [...(notice ? [notice] : []), ...boardLines(value, mode, selected)], page, graphic, footer });
    page = layout.page;
    const answer = await ui.action('\nAction: ', draft); draft = undefined;
    if (answer === null || typeof answer === 'object' && answer.key === 'escape') return;
    if (typeof answer === 'object') {
      if (answer.key === 'resize') { draft = answer.draft; continue; }
      if (answer.key === 'f1') { await visualHelp(ui); continue; }
      if (mode === 'plan' && value.floor_plan.available) {
        const plan = value.floor_plan;
        const dx = plan.width / (5 * planView.zoom); const dy = plan.height / (5 * planView.zoom);
        planView.centerX = Math.max(0, Math.min(plan.width, (planView.centerX ?? plan.width / 2) + (answer.key === 'left' ? -dx : answer.key === 'right' ? dx : 0)));
        planView.centerY = Math.max(0, Math.min(plan.height, (planView.centerY ?? plan.height / 2) + (answer.key === 'up' ? -dy : answer.key === 'down' ? dy : 0)));
        if (answer.key === 'home') planView = { zoom: 1 };
      } else if (['up', 'pageup'].includes(answer.key)) page = Math.max(0, page - 1);
      else if (['down', 'pagedown'].includes(answer.key)) page = Math.min(layout.pages - 1, page + 1);
      continue;
    }
    const key = answer.trim();
    if (key === 'q' || key === '') return;
    try {
      if (key === '?') await visualHelp(ui);
      else if (['f', 'b', 'm'].includes(key)) { mode = { f: 'flow', b: 'stock', m: 'plan' }[key]; page = 0; }
      else if (key === 'r') { await refresh(); history = [null]; }
      else if (key === 'j') await inspectJson(ui, 'Original visual console evidence', value);
      else if (key === 's') await productPreview(client, scope, ui);
      else if (key === 'n') {
        if (page + 1 < layout.pages) page++;
        else if (mode === 'stock' && value.bins.next_after) { const after = value.bins.next_after; await refresh(after, value.version); history.push(after); }
      } else if (key === 'p') {
        if (page) page--;
        else if (mode === 'stock' && history.length > 1) { await refresh(history.at(-2), value.version); history.pop(); }
      } else if (mode === 'plan' && value.floor_plan.available) {
        if (key === '+' || key === '-') planView.zoom = Math.max(1, Math.min(8, planView.zoom * (key === '+' ? 2 : 0.5)));
        else if (key === '0') { planView = { zoom: 1 }; selected = ''; notice = ''; }
        else if (key.startsWith('/')) {
          const code = key.slice(1); const bin = value.floor_plan.bins.find(bin => bin.code === code);
          if (!bin) { notice = 'Bin is not mapped in this floor plan. Use 0 to reset.'; continue; }
          selected = code; notice = ''; planView = { zoom: 4, centerX: bin.x + bin.width / 2, centerY: bin.y + bin.height / 2 }; page = 0;
        }
      }
    } catch (error) { if (error.code === 'INPUT_CANCELLED') continue; stale = safeText(error.message); }
  }
}

export async function productPreview(client, scope, ui, selectedSku) {
  const sku = selectedSku ?? await ui.ask('Exact product SKU: '); if (sku === null || sku === '') return;
  let product; let photo; let mode = 'label'; let page = 0; let stale = ''; let draft;
  const refresh = async () => { await boundSession(client, scope); product = await readProduct(client, scope, sku); photo = undefined; stale = ''; page = 0; };
  ui.screen('Product preview'); ui.line('Loading original product evidence…'); await refresh();
  for (;;) {
    let graphic;
    const lines = [`Product: ${product.name}`, `Exact SKU: ${product.sku}`, `Category: ${product.category || 'Unspecified'}`, `Reorder point: ${product.reorder_point_each} each`, product.serial_controlled ? 'Serials: actual observed labels required' : 'Serials: not controlled', product.expiry_required ? 'Expiry: original expiry evidence required' : 'Expiry: optional'];
    if (mode === 'label') {
      lines.push(`Label value: ${product.label.value}`, 'Unit: each · preview only; printer/scanner acceptance is separate.');
      if (product.label.symbology === 'code128') { const label = labelRaster(product.label.value, { demo: product.demo }); graphic = () => label; }
      else lines.push('A Code 128 preview is unavailable for this SKU. Its original literal value is preserved above.');
    } else if (!product.image) lines.push(product.image_status === 'unavailable' ? 'Configured photo is unavailable; ask an administrator to review the asset. Original product data remains available.' : 'No product photo is available. Label and original product data remain available.');
    else if (await ui.graphics.detect()) {
      try { if (photo === undefined) { await boundSession(client, scope); photo = await readProductImage(client, scope, product); } graphic = photo; }
      catch (error) { photo = null; stale = safeText(error.message); }
    } else lines.push('Product photo requires a graphics-capable terminal. Original product data is shown.');
    const layout = await drawVisual(ui, { title: mode === 'label' ? 'Product label preview' : 'Product photo preview', version: product.version, capturedAt: product.captured_at, demo: product.demo, stale, lines, page, graphic,
      footer: ['l label · p photo · r refresh · j details', 'n next page · b previous page · ? help · q back'] });
    page = layout.page;
    const answer = await ui.action('\nAction: ', draft); draft = undefined;
    if (answer === null || typeof answer === 'object' && answer.key === 'escape') return;
    if (typeof answer === 'object') { if (answer.key === 'resize') draft = answer.draft; if (answer.key === 'f1') await visualHelp(ui); if (['down', 'pagedown'].includes(answer.key)) page++; if (['up', 'pageup'].includes(answer.key)) page = Math.max(0, page - 1); continue; }
    const key = answer.trim(); if (!key || key === 'q') return;
    try {
      if (key === '?') await visualHelp(ui);
      else if (key === 'l' || key === 'p') { mode = key === 'l' ? 'label' : 'photo'; page = 0; }
      else if (key === 'r') await refresh();
      else if (key === 'j') await inspectJson(ui, 'Original product preview evidence', product);
      else if (key === 'n') page++;
      else if (key === 'b') page = Math.max(0, page - 1);
    } catch (error) { stale = safeText(error.message); }
  }
}

export async function visualConsole(client, scope, ui) {
  const selected = await ui.menu('Visual warehouse operations', [['Operations flow', 'flow'], ['Stock by bin', 'stock'], ['Configured floor plan', 'plan'], ['Product photo / label preview', 'product']]);
  if (selected === 'product') await productPreview(client, scope, ui);
  else if (selected) await visualBoard(client, scope, ui, selected);
}
