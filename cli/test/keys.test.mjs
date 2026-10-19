import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough, Writable } from 'node:stream';
import { Keys } from '../lib/keys.mjs';
import { Terminal } from '../lib/terminal.mjs';

function device() {
  const input = new PassThrough(); const modes = []; let text = '';
  input.isTTY = true; input.isRaw = false;
  input.setRawMode = enabled => { input.isRaw = enabled; modes.push(enabled); };
  const output = new Writable({ write(chunk, _encoding, done) { text += chunk; done(); } });
  output.isTTY = true; output.columns = 70; output.rows = 44;
  return { input, output, modes, text: () => text };
}

test('one scanner chunk preserves exact case and CRLF boundaries across prompts', async () => {
  const io = device(); const keys = new Keys(io.input, io.output, () => 69);
  try {
    io.input.write('Original\r\noriginal\r\nThird\n');
    assert.equal(await keys.read('First label: '), 'Original');
    assert.equal(await keys.read('Second label: '), 'original');
    assert.equal(await keys.read('Third label: '), 'Third');
  } finally { keys.close(); }
  assert.deepEqual(io.modes, [true, false]);
  assert.ok(io.text().includes('\x1b[?2004l'));
});

test('keyboard editing preserves Unicode graphemes and literal surrounding spaces', async () => {
  const io = device(); const keys = new Keys(io.input, io.output, () => 69);
  try {
    io.input.write('Lot-e\u0301\x7f🙂\x1b[DX\x01 \x05 \n');
    assert.equal(await keys.read('Literal label: '), ' Lot-X🙂 ');
  } finally { keys.close(); }
});

test('queued navigation and form entry are delivered to the right prompts', async () => {
  const io = device(); const keys = new Keys(io.input, io.output, () => 69);
  try {
    io.input.write('\x1b[B\nExact-Serial\r\n');
    assert.deepEqual(await keys.read('Select: ', true), { key: 'down' });
    assert.equal(await keys.read('Select: ', true), '');
    assert.equal(await keys.read('Scan: '), 'Exact-Serial');
  } finally { keys.close(); }
});

test('bracketed paste preserves single fields and does not submit pasted lines', async () => {
  const io = device(); const keys = new Keys(io.input, io.output, () => 69);
  try {
    io.input.write('\x1b[200~bad\nyes\n\x1b[201~\x1b[200~Actual-Case\x1b[201~\n');
    assert.equal(await keys.read('Scan actual label: '), 'Actual-Case');
    assert.ok(io.text().includes('multi-line paste was ignored'));
  } finally { keys.close(); }
});

test('Ctrl-C and EOF restore terminal mode without accepting partial entry', async () => {
  for (const bytes of ['partial\x03', '\x04']) {
    const io = device(); const keys = new Keys(io.input, io.output, () => 69);
    const waiting = keys.read('Confirm: ');
    io.input.write(bytes);
    assert.equal(await waiting, null); assert.equal(io.input.isRaw, false);
    keys.close(); assert.deepEqual(io.modes, [true, false]);
  }
});

test('Escape cancels a form and oversized paste never accepts a truncated identity', async () => {
  const io = device(); const keys = new Keys(io.input, io.output, () => 69);
  try {
    io.input.emit('keypress', '\x1b', { name: 'escape' });
    await assert.rejects(keys.read('Scan: '), error => error.code === 'INPUT_CANCELLED');
    keys.push({ paste: 'x', overflow: true });
    await assert.rejects(keys.read('Scan: '), /exceeds 1 MiB; no value accepted/);
  } finally { keys.close(); }
});

test('arrow selection, search and Escape work without changing command numbers or leaking ANSI', async () => {
  const io = device(); const ui = new Terminal(io.input, io.output, { TERM: 'xterm-ghostty', NO_COLOR: '1' });
  try {
    io.input.write('\x1b[B\n');
    assert.equal(await ui.menu('Work', [['First', 'first'], ['Second', 'second']]), 'second');
    io.input.write('/third\n\n');
    assert.equal(await ui.menu('Work', [['First', 'first'], ['Second', 'second'], ['Third', 'third']]), 'third');
    assert.equal(ui.style('bad\x1b[2J', 'selected'), 'bad\\u001b[2J');
    const waiting = ui.menu('Exit', [['First', 'first']]);
    io.input.emit('keypress', '\x1b', { name: 'escape' });
    assert.equal(await waiting, null);
  } finally { ui.close(); }
  assert.ok(io.text().includes('\x1b[?1049h'));
  assert.ok(io.text().includes('\x1b[?1049l'));
  assert.equal(io.input.isRaw, false);
});

test('resize and paging retain numbered actions, help returns, and colours stay optional', async () => {
  const io = device(); const ui = new Terminal(io.input, io.output, { TERM: 'xterm-ghostty' });
  try {
    const waiting = ui.menu('Work', Array.from({ length: 36 }, (_, n) => [`Action ${n + 1}`, n + 1]));
    io.input.write('\x1b[F');
    await new Promise(setImmediate);
    io.output.columns = 49; io.output.rows = 20; io.output.emit('resize');
    await new Promise(setImmediate);
    io.input.write('\n');
    assert.equal(await waiting, 36);
    assert.ok(io.text().split('\x1b[2J\x1b[H').at(-1).includes('36. Action 36'), 'selected action remains visible after resize');
    io.input.write('?\n\nq\n');
    assert.equal(await ui.menu('Help', [['First', 'first']]), null);
    assert.ok(io.text().includes('Keyboard guide'));
    assert.match(ui.style('bad\x1b[2J', 'selected'), /^\x1b\[7;1mbad\\u001b\[2J\x1b\[0m$/);
  } finally { ui.close(); }
});

test('resizing a field preserves partially entered literal scanner text', async () => {
  const io = device(); const keys = new Keys(io.input, io.output, () => io.output.columns - 1);
  try {
    const waiting = keys.read('Exact original serial label: ');
    io.input.write('Case-preserved');
    await new Promise(setImmediate);
    io.output.columns = 30; keys.push({ key: { name: 'resize' } });
    io.input.write('-tail\n');
    assert.equal(await waiting, 'Case-preserved-tail');
  } finally { keys.close(); }
});

test('F2 is a dedicated scan correction key and never a reserved literal label', async () => {
  const io = device(); const keys = new Keys(io.input, io.output, () => 69);
  try {
    io.input.write('/undo\n\x1bOQ');
    assert.equal(await keys.read('Scan: ', false, { f2: 'undo' }), '/undo');
    assert.deepEqual(await keys.read('Scan: ', false, { f2: 'undo' }), { key: 'undo' });
  } finally { keys.close(); }
});
