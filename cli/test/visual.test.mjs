import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough, Writable } from 'node:stream';
import { Terminal } from '../lib/terminal.mjs';
import { width } from '../lib/format.mjs';
import { boardLines, drawVisual, visualBoard, labelRaster } from '../lib/visual.mjs';
import { code128 } from '../lib/code128.mjs';
import { flowRaster, stockRaster, planRaster, safePng } from '../lib/raster.mjs';
import { consoleFixture, scope } from './console-fixture.mjs';

function device(columns, rows) {
  let text = ''; const input = new PassThrough(); input.isTTY = true; input.setRawMode = () => {};
  const output = new Writable({ write(chunk, _encoding, done) { text += chunk.toString(); done(); } }); output.isTTY = true; output.columns = columns; output.rows = rows;
  return { input, output, text: () => text };
}
test('flow, stock and actual configured geometry produce bounded valid raster previews', () => {
  const view = consoleFixture();
  for (const image of [flowRaster(view, 640, 160), stockRaster(view.bins.items, 640, 160), planRaster(view.floor_plan, 640, 160), planRaster(view.floor_plan, 640, 160, { zoom: 8, centerX: 1, centerY: 1, selected: 'BIN-A' })]) {
    assert.equal(image.bytes.length, 640 * 160 * 3); assert.ok(safePng(image.png()).length > 0);
  }
  const lines = boardLines(view, 'stock'); assert.ok(lines.some(line => line.includes('8 physical / 3 reserved each'))); assert.ok(lines.some(line => line.includes('do not measure capacity')));
});
test('Code 128 Set B retains exact case, start/stop, quiet zones and independent checksum vector', () => {
  const encoded = code128('AB'); assert.deepEqual(encoded.symbols, [104, 33, 34, 102, 106]);
  assert.equal(encoded.checksum, 102); assert.notDeepEqual(code128('Ab').symbols, encoded.symbols);
  for (const invalid of ['', 'line\nvalue', 'é', 'x'.repeat(81)]) assert.throws(() => code128(invalid), /ASCII/);
  const label = labelRaster('Sku-Ab12'); const middle = Math.floor(label.height / 2);
  assert.ok(label.bytes.subarray(middle * label.width * 3, (middle * label.width + 20) * 3).every(value => value === 255));
  assert.ok(label.bytes.subarray(((middle + 1) * label.width - 20) * 3, (middle + 1) * label.width * 3).every(value => value === 255));
  assert.ok(safePng(label.png()).length > 0);
});
test('small panes keep complete paged quantities and prompts while graphics remain optional', async () => {
  for (const [columns, rows] of [[49, 20], [70, 24], [70, 44]]) {
    const io = device(columns, rows); const ui = new Terminal(io.input, io.output, { WMS_TUI_GRAPHICS: 'off', NO_COLOR: '1' });
    try {
      const result = await drawVisual(ui, { title: 'Stock by bin', version: '9007199254740991', capturedAt: '2026-10-20T20:00:00Z', demo: true,
        lines: ['Exact: 900719925474099312345 each', ...Array.from({ length: 60 }, (_, n) => `Original source ${n}`)], footer: ['n next · p previous · q back'] });
      const text = io.text().replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '');
      assert.ok(text.includes('900719925474099312345 each')); assert.ok(text.includes('SYNTHETIC DEMO')); assert.ok(result.pages > 1);
      const lines = text.split('\n'); assert.ok(lines.length <= rows + 1, `${columns}×${rows}: ${lines.length}`);
      assert.ok(lines.every(line => width(line) <= columns), `${columns}×${rows}`);
    } finally { ui.close(); }
  }
});
test('failed refresh retains exact old data and does not create automatic reads during resize', async () => {
  const view = consoleFixture(); let reads = 0; let identityReads = 0; const snapshots = []; const actions = ['r', { key: 'resize', draft: { buffer: '/BIN-A', cursor: 6 } }, 'q'];
  const client = { origin: scope.origin, warehousePath: (id, suffix) => `/v1/warehouses/${id}${suffix}`,
    session: async () => { identityReads++; return { identity: { tenant: scope.tenant, actor: scope.actor, role: 'viewer' } }; },
    read: async () => { reads++; if (reads > 1) throw new Error('Read unavailable'); return view; } };
  const ui = { columns: 69, rows: 32, graphics: { detect: async () => false, clear() {} }, screen() {}, line(text) { snapshots.push(text); }, print() {},
    action: async (_prompt, draft) => { if (actions[0] === 'q') assert.equal(draft.buffer, '/BIN-A'); return actions.shift(); } };
  await visualBoard(client, scope, ui);
  assert.equal(reads, 2); assert.equal(identityReads, 2); assert.ok(snapshots.some(line => line.includes('STALE'))); assert.ok(snapshots.some(line => line.includes('10 each')));
});
