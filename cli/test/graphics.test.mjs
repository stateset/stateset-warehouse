import test from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { PassThrough, Writable } from 'node:stream';
import { deflateSync, inflateSync } from 'node:zlib';
import { GraphicsReplies, KittyGraphics, graphicsMode } from '../lib/graphics.mjs';
import { Keys } from '../lib/keys.mjs';
import { Raster, safePng, pngInfo, crc32 } from '../lib/raster.mjs';

function output() {
  let text = ''; const stream = new Writable({ write(bytes, _encoding, done) { text += bytes.toString(); done(); } });
  stream.isTTY = true; stream.columns = 80; stream.rows = 32;
  return { stream, text: () => text };
}
test('graphics require an affirmative query reply and close clears only the owned image', async () => {
  const io = output(); const graphics = new KittyGraphics(io.stream, { TERM: 'xterm-ghostty' }, { id: 31 });
  const detected = graphics.detect(); assert.match(io.text(), /a=q,t=d,f=24,s=1,v=1,i=31;AAAA/);
  graphics.reply('i=32;OK'); assert.equal(graphics.state, 'querying');
  graphics.reply('i=31;OK'); assert.equal(await detected, true);
  assert.equal(await graphics.show(Buffer.alloc(12), { width: 2, height: 2, columns: 10, rows: 4 }), true);
  graphics.close(); assert.match(io.text(), new RegExp(`a=d,d=I,i=${graphics.imageId},q=2`));
  assert.doesNotMatch(io.text(), /a=d,d=[aA][,;]/);
  assert.equal(await graphics.show(Buffer.alloc(12), { width: 2, height: 2, columns: 10, rows: 4 }), false);
});
test('off, NO_COLOR, multiplexers and nonterminals fall back without emitting graphics queries', async () => {
  for (const env of [{ WMS_TUI_GRAPHICS: 'off' }, { NO_COLOR: '1' }, { TMUX: 'session' }, { STY: 'session' }, { TERM: 'dumb' }]) {
    const io = output(); const graphics = new KittyGraphics(io.stream, env);
    assert.equal(await graphics.detect(), false); assert.equal(io.text(), ''); graphics.close();
  }
  const io = output(); const graphics = new KittyGraphics(io.stream, {}, { interactive: false });
  assert.equal(await graphics.detect(), false); assert.equal(io.text(), '');
  assert.throws(() => graphicsMode('yes'), /must be/);
});
test('an unsupported or late graphics reply never enables rendering', async () => {
  const io = output(); const graphics = new KittyGraphics(io.stream, {}, { id: 31 });
  const promise = graphics.detect(); graphics.reply('i=31;ENOTSUP'); assert.equal(await promise, false);
  graphics.reply('i=31;OK'); assert.equal(graphics.state, 'unavailable'); graphics.close();
});
test('RGB transfer roundtrips through bounded base64 chunks with one final chunk', async () => {
  const io = output(); const graphics = new KittyGraphics(io.stream, {}, { id: 31 }); graphics.detect(); graphics.reply('i=31;OK');
  const bytes = randomBytes(128 * 96 * 3);
  await graphics.show(bytes, { width: 128, height: 96, columns: 60, rows: 8 });
  const frames = [...io.text().matchAll(/\x1b_G([^;]+);([^\x1b]*)\x1b\\/g)].slice(1);
  assert.ok(frames.length > 2); assert.match(frames[0][1], /a=T,t=d,f=24,o=z,s=128,v=96/);
  assert.ok(frames.every(frame => frame[2].length <= 4096 && frame[2].length % 4 === 0));
  assert.ok(frames.slice(0, -1).every(frame => frame[1].endsWith('m=1'))); assert.ok(frames.at(-1)[1].endsWith('m=0'));
  assert.deepEqual(inflateSync(Buffer.from(frames.map(frame => frame[2]).join(''), 'base64')), bytes); graphics.close();
});
test('invalid image dimensions and pane placements are rejected before transmission', async () => {
  const io = output(); const graphics = new KittyGraphics(io.stream, { WMS_TUI_GRAPHICS: 'off' });
  await assert.rejects(graphics.show(Buffer.alloc(3), { width: 2, height: 1, columns: 10, rows: 4 }), /RGB/);
  await assert.rejects(graphics.show(Buffer.alloc(3), { width: 1, height: 1, columns: 90, rows: 4 }), /pane/);
  await assert.rejects(graphics.show(Buffer.from('not a PNG'), { columns: 10, rows: 4, png: true }), /PNG/);
});
test('split protocol replies cannot become scanner data or break original UTF-8 and CRLF', async () => {
  const input = new PassThrough(); input.isTTY = true; input.setRawMode = value => { input.isRaw = value; };
  const io = output(); const replies = []; const keys = new Keys(input, io.stream, () => 79, { onProtocol: frame => replies.push(frame) });
  try {
    const bytes = Buffer.from('Original-e\u0301\r\x1b_Gi=31;OK\x1b\\\noriginal\r\n');
    for (let i = 0; i < bytes.length; i++) input.write(bytes.subarray(i, i + 1));
    assert.equal(await keys.read('First: '), 'Original-e\u0301'); assert.equal(await keys.read('Second: '), 'original');
    assert.deepEqual(replies, ['i=31;OK']);
  } finally { keys.close(); }
});
test('malformed graphics frames reject the whole input field and discard its trailing fragment', async () => {
  const input = new PassThrough(); input.isTTY = true; input.setRawMode = () => {};
  const io = output(); const keys = new Keys(input, io.stream, () => 79, { onProtocol: () => {} });
  try {
    const waiting = keys.read('Scan: '); input.write('Original\x1b_Gbad-response\x1b\\truncated\r\nComplete-Rescan\n');
    await assert.rejects(waiting, /No value accepted/);
    assert.equal(await keys.read('Rescan: '), 'Complete-Rescan');
  } finally { keys.close(); }
});
test('incomplete protocol frames cannot grow buffered stdin without a limit', async () => {
  const filter = new GraphicsReplies(() => {}); let faults = 0; filter.on('fault', () => { faults++; }); filter.resume();
  filter.write('\x1b_G' + 'x'.repeat(20000));
  assert.ok(filter.pending.length <= 1); assert.ok(faults > 0); filter.destroy();
});
test('PNG integrity and decoded-size bounds protect the terminal decoder', () => {
  const image = new Raster(16, 12); const bytes = image.png(); assert.deepEqual(pngInfo(bytes), { width: 16, height: 12 });
  assert.deepEqual(safePng(bytes), bytes);
  const broken = Buffer.from(bytes); broken[broken.length - 1] ^= 1;
  assert.throws(() => safePng(broken), /integrity/);
  assert.throws(() => safePng(bytes.subarray(0, -6)), /Truncated|Incomplete/);
});

test('delayed APC prefixes stay out of scanner data while standalone Esc still cancels', async () => {
  const input = new PassThrough(); input.isTTY = true; input.setRawMode = value => { input.isRaw = value; };
  const io = output(); const replies = []; const keys = new Keys(input, io.stream, () => 79, { onProtocol: frame => replies.push(frame) });
  try {
    const read = keys.read('Scan: '); input.write('Exact\x1b_');
    await new Promise(resolve => setTimeout(resolve, 45)); input.write('Gi=31;OK\x1b\\-Label\n');
    assert.equal(await read, 'Exact-Label'); assert.deepEqual(replies, ['i=31;OK']);
    const cancel = keys.read('Scan: '); input.write('\x1b');
    await assert.rejects(cancel, { code: 'INPUT_CANCELLED' });
  } finally { keys.close(); assert.equal(input.isRaw, false); }
});

test('high-bit and paste-interrupted protocol replies never accept partial physical labels', async () => {
  for (const bytes of [Buffer.concat([Buffer.from('Prefix\x1b_Gi=31;'), Buffer.from([0xcf, 0xcb]), Buffer.from('\x1b\\Suffix\nExact-Rescan\n')]), Buffer.from('\x1b[200~Prefix\x1b_Gbad\x1b\\Suffix\x1b[201~\r\nExact-Rescan\n')]) {
    const input = new PassThrough(); input.isTTY = true; input.setRawMode = () => {};
    const keys = new Keys(input, output().stream, () => 79, { onProtocol: () => {} });
    try {
      const read = keys.read('Scan: '); input.write(bytes);
      await assert.rejects(read, /No value accepted/); assert.equal(await keys.read('Rescan: '), 'Exact-Rescan');
    } finally { keys.close(); }
  }
});

test('PNG animation, metadata, oversized inflation and high-bit chunk names stay bounded', () => {
  const chunk = (kind, data) => { const raw = Buffer.concat([Buffer.from(kind, 'latin1'), data]); const bytes = Buffer.alloc(data.length + 12); bytes.writeUInt32BE(data.length); raw.copy(bytes, 4); bytes.writeUInt32BE(crc32(raw), bytes.length - 4); return bytes; };
  const original = new Raster(2, 2).png(); const head = original.subarray(0, 33); const end = original.subarray(-12);
  assert.throws(() => safePng(Buffer.concat([head, chunk('acTL', Buffer.alloc(8)), original.subarray(33)])), /Animated/);
  assert.deepEqual(safePng(Buffer.concat([head, chunk('tEXt', Buffer.from('private metadata')), original.subarray(33)])), original);
  assert.throws(() => safePng(Buffer.concat([head, chunk('IDAT', deflateSync(Buffer.alloc(1024 * 1024))), end])), /larger|size|length|buffer/i);
  assert.throws(() => safePng(Buffer.concat([head, chunk('\xc9DAT', Buffer.alloc(1)), original.subarray(33)])), /integrity/);
});
