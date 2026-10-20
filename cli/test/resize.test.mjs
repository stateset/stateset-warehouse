import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough, Writable } from 'node:stream';
import { Terminal } from '../lib/terminal.mjs';
import { inspectRows, inspectText } from '../lib/views.mjs';

function device(columns = 120, rows = 40) {
  const input = new PassThrough(); input.isTTY = true; input.isRaw = false; input.setRawMode = value => { input.isRaw = value; };
  let text = ''; const output = new Writable({ write(chunk, _encoding, done) { text += chunk; done(); } });
  output.isTTY = true; output.columns = columns; output.rows = rows;
  const ui = new Terminal(input, output, { TERM: 'xterm-ghostty', NO_COLOR: '1' });
  const frames = () => text.split('\x1b[2J\x1b[H').slice(1).map(frame => frame.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, ''));
  return { ui, input, output, frames, async resize(cols, height) { output.columns = cols; output.rows = height; output.emit('resize'); await new Promise(setImmediate); } };
}
const settle = () => new Promise(setImmediate);

test('menu search and numeric input survive repeated pane resizing without being submitted', async () => {
  const io = device();
  try {
    const menu = io.ui.menu('Warehouse work', Array.from({ length: 20 }, (_, n) => [n === 11 ? 'Original receipt' : `Other ${n + 1}`, n + 1]));
    io.input.write('/Orig'); await settle();
    await io.resize(49, 20); await io.resize(70, 24);
    io.input.write('inal\n\n');
    assert.equal(await menu, 12);
    assert.ok(io.frames().some(frame => frame.includes('Search: Original') && frame.includes('12. Original receipt')));
    const numbered = io.ui.menu('Warehouse work', Array.from({ length: 20 }, (_, n) => [`Work ${n + 1}`, n + 1]));
    io.input.write('1'); await settle();
    await io.resize(49, 20); await io.resize(120, 40);
    io.input.write('2\n');
    assert.equal(await numbered, 12);
  } finally { io.ui.close(); }
  assert.equal(io.input.isRaw, false);
});

test('resizing retains the Unicode editor cursor and Escape discards only its current draft', async () => {
  const io = device();
  try {
    const menu = io.ui.menu('Original work', [['倉庫 e\u0301Original', 'original'], ['Other', 'other']]);
    io.input.write('/倉庫 e\u0301XOriginal' + '\x1b[D'.repeat(8)); await settle();
    await io.resize(49, 20);
    io.input.write('\x7f\n\n');
    assert.equal(await menu, 'original', 'Backspace still removes X at the original cursor');
    const cancelled = io.ui.menu('Cancel search', [['First', 1]]);
    io.input.write('/unfinished'); await settle(); await io.resize(70, 24);
    io.input.emit('keypress', '\x1b', { name: 'escape' });
    assert.equal(await cancelled, null);
    io.input.write('q\n');
    assert.equal(await io.ui.menu('Next menu', [['First', 1]]), null, 'cancelled draft cannot leak into another menu');
  } finally { io.ui.close(); }
});

const rows = Array.from({ length: 100 }, (_, n) => ({ sku: `SKU-${String(n).padStart(3, '0')}`, lot: 'Original-Lot', location: 'Original-Bin', on_hand: n + 1, reserved: 0 }));
test('table searches retain partial input across resize without reading or recording work', async () => {
  const io = device(); let loads = 0;
  try {
    const view = inspectRows(io.ui, 'stocks', async () => { loads++; return { title: 'Original inventory', version: 7, rows }; });
    await settle(); io.input.write('/SKU-0'); await settle();
    await io.resize(49, 20); io.input.write('99\n'); await settle();
    assert.ok(io.frames().at(-1).includes('1–1 of 1 rows') && io.frames().at(-1).includes('SKU-099'));
    assert.equal(loads, 1);
    io.input.write('q\n'); await view;
  } finally { io.ui.close(); }
});

test('a selected original row stays visible and actions remain bound to it after resize', async () => {
  const io = device(); let loads = 0; const actions = [];
  try {
    const view = inspectRows(io.ui, 'stocks', async () => { loads++; return { title: 'Original inventory', version: 7, rows }; }, { onAction: async (row, version) => { actions.push({ row, version }); } });
    await settle(); io.input.write('\x1b[F'); await settle(); await io.resize(49, 20);
    assert.ok(io.frames().at(-1).includes('SKU-099')); assert.equal(loads, 1); assert.equal(actions.length, 0);
    io.input.write('a\n'); await settle(); io.input.write('q\n'); await view;
    assert.deepEqual(actions, [{ row: rows[99], version: 7 }]); assert.equal(loads, 2);
  } finally { io.ui.close(); }
});

test('an unselected table retains its reading position when a nested detail view resizes', async () => {
  const io = device(); let loads = 0;
  try {
    const view = inspectRows(io.ui, 'stocks', async () => { loads++; return { title: 'Original inventory', version: 7, rows }; });
    await settle(); io.input.write('n\n'); await settle();
    const firstRow = Number(io.frames().at(-1).match(/(\d+)–\d+ of 100 rows/)[1]) - 1;
    assert.ok(firstRow > 0);
    io.input.write('j\n'); await settle(); await io.resize(49, 20);
    io.input.emit('keypress', '\x1b', { name: 'escape' }); await settle();
    assert.ok(io.frames().at(-1).includes(rows[firstRow].sku), 'the previously first visible original row remains on screen');
    assert.equal(loads, 1); io.input.write('q\n'); await view;
  } finally { io.ui.close(); }
});

test('long reviews retain the source passage across text reflow and preserve a typed navigation command', async () => {
  const io = device(49, 20);
  const text = Array.from({ length: 40 }, (_, n) => `Original passage ${n}: ` + 'Literal 倉庫 e\u0301 / '.repeat(8)).join('\n');
  try {
    const view = inspectText(io.ui, 'Original review', text);
    io.input.write('n\nn\n'); await settle();
    const passage = io.frames().at(-1).match(/Original passage (\d+)/)?.[0]; assert.ok(passage);
    io.input.write('p'); await settle(); await io.resize(120, 40);
    assert.ok(io.frames().at(-1).includes(passage), 'the same source passage remains visible after widening');
    io.input.write('\n'); await settle();
    assert.ok(io.frames().at(-1).includes('Original passage 0'), 'the pending p command was preserved and executed only on Enter');
    io.input.write('q\n'); assert.equal(await view, false);
  } finally { io.ui.close(); }
});
