import { deflateSync, inflateSync } from 'node:zlib';

const CRC = Array.from({ length: 256 }, (_, n) => { for (let i = 0; i < 8; i++) n = n & 1 ? 0xedb88320 ^ (n >>> 1) : n >>> 1; return n >>> 0; });
export function crc32(bytes) { let crc = 0xffffffff; for (const b of bytes) crc = CRC[(crc ^ b) & 255] ^ (crc >>> 8); return (crc ^ 0xffffffff) >>> 0; }
function chunk(kind, bytes) {
  const data = Buffer.concat([Buffer.from(kind), bytes]); const result = Buffer.alloc(bytes.length + 12);
  result.writeUInt32BE(bytes.length); data.copy(result, 4); result.writeUInt32BE(crc32(data), result.length - 4); return result;
}
const SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
export function pngInfo(bytes) {
  if (!Buffer.isBuffer(bytes) || bytes.length > 4 * 1024 * 1024 || bytes.length < 33
    || !bytes.subarray(0, 8).equals(SIGNATURE) || bytes.readUInt32BE(8) !== 13 || bytes.toString('ascii', 12, 16) !== 'IHDR') throw new Error('Expected a bounded static PNG');
  const width = bytes.readUInt32BE(16); const height = bytes.readUInt32BE(20);
  if (!width || !height || width > 2048 || height > 2048 || ![1, 2, 4, 8].includes(bytes[24])
    || ![0, 2, 3, 4, 6].includes(bytes[25]) || bytes[26] || bytes[27] || bytes[28] > 1) throw new Error('Unsupported PNG dimensions or format');
  return { width, height };
}
export function safePng(bytes) {
  const { width, height } = pngInfo(bytes); const kept = [SIGNATURE]; const compressed = []; let offset = 8; let ended = false; let headers = 0;
  while (offset + 12 <= bytes.length) {
    const size = bytes.readUInt32BE(offset); const end = offset + size + 12;
    if (end > bytes.length) throw new Error('Truncated PNG chunk');
    const kind = bytes.toString('latin1', offset + 4, offset + 8);
    if (!/^[A-Za-z]{4}$/.test(kind) || crc32(bytes.subarray(offset + 4, end - 4)) !== bytes.readUInt32BE(end - 4)) throw new Error('PNG integrity check failed');
    if (['acTL', 'fcTL', 'fdAT'].includes(kind)) throw new Error('Animated PNG previews are unavailable');
    if (kind === 'IHDR' && ++headers !== 1) throw new Error('Duplicate PNG header');
    if (kind === 'IDAT') compressed.push(bytes.subarray(offset + 8, end - 4));
    if (['IHDR', 'PLTE', 'tRNS', 'IDAT', 'IEND'].includes(kind)) kept.push(bytes.subarray(offset, end));
    else if (kind[0] === kind[0].toUpperCase()) throw new Error('Unsupported critical PNG chunk');
    if (kind === 'IEND') { if (size || end !== bytes.length) throw new Error('Invalid PNG end'); ended = true; break; }
    offset = end;
  }
  if (!ended || !compressed.length) throw new Error('Incomplete PNG');
  // Bound decoded scanlines too. Strip ancillary compressed profiles/metadata
  // before passing pixels to a terminal decoder.
  inflateSync(Buffer.concat(compressed), { maxOutputLength: (width * 4 + 9) * height + 65536 });
  return Buffer.concat(kept);
}

const FONT = {
  '0':'01110100011001110101110011000101110','1':'00100011000010000100001000010001110','2':'01110100010000100010001000100011111',
  '3':'11110000010000101110000010000111110','4':'00010001100101010010111110001000010','5':'11111100001000011110000010000111110',
  '6':'01110100001000011110100011000101110','7':'11111000010001000100010000100001000','8':'01110100011000101110100011000101110','9':'01110100011000101111000010000101110',
  A:'01110100011000111111100011000110001',B:'11110100011000111110100011000111110',C:'01111100001000010000100001000001111',
  D:'11110100011000110001100011000111110',E:'11111100001000011110100001000011111',F:'11111100001000011110100001000010000',
  G:'01111100001000010111100011000101111',H:'10001100011000111111100011000110001',I:'01110001000010000100001000010001110',
  J:'00001000010000100001100011000101110',K:'10001100101010011000101001001010001',L:'10000100001000010000100001000011111',
  M:'10001110111010110101100011000110001',N:'10001110011010110011100011000110001',O:'01110100011000110001100011000101110',
  P:'11110100011000111110100001000010000',Q:'01110100011000110001101011001001101',R:'11110100011000111110101001001010001',
  S:'01111100001000001110000010000111110',T:'11111001000010000100001000010000100',U:'10001100011000110001100011000101110',
  V:'10001100011000110001100010101000100',W:'10001100011000110101101011101110001',X:'10001100010101000100010101000110001',
  Y:'10001100010101000100001000010000100',Z:'11111000010001000100010001000011111', '-':'00000000000000011111000000000000000',
};
export const palette = { background: [11, 18, 32], card: [20, 32, 50], edge: [41, 62, 82], text: [224, 237, 246], muted: [105, 134, 158], cyan: [48, 206, 211], amber: [255, 185, 72], green: [91, 211, 155] };
export class Raster {
  constructor(width, height, background = palette.background) {
    if (![width, height].every(n => Number.isInteger(n) && n > 0 && n <= 2048) || width * height > 1024 * 1024) throw new Error('Raster dimensions exceed the visual budget');
    this.width = width; this.height = height; this.bytes = Buffer.alloc(width * height * 3); this.rect(0, 0, width, height, background);
  }
  rect(x, y, width, height, color) {
    const left = Math.max(0, Math.floor(x)); const right = Math.min(this.width, Math.ceil(x + width));
    const top = Math.max(0, Math.floor(y)); const bottom = Math.min(this.height, Math.ceil(y + height));
    for (let row = top; row < bottom; row++) for (let col = left; col < right; col++) {
      const offset = (row * this.width + col) * 3; this.bytes[offset] = color[0]; this.bytes[offset + 1] = color[1]; this.bytes[offset + 2] = color[2];
    }
    return this;
  }
  text(text, x, y, color = palette.text, scale = 1) {
    for (const character of String(text)) {
      const glyph = FONT[character];
      if (glyph) for (let row = 0; row < 7; row++) for (let col = 0; col < 5; col++) if (glyph[row * 5 + col] === '1') this.rect(x + col * scale, y + row * scale, scale, scale, color);
      x += 6 * scale;
    }
    return this;
  }
  png() {
    const header = Buffer.alloc(13); header.writeUInt32BE(this.width); header.writeUInt32BE(this.height, 4); header[8] = 8; header[9] = 2;
    const rows = Buffer.alloc((this.width * 3 + 1) * this.height);
    for (let row = 0; row < this.height; row++) this.bytes.copy(rows, row * (this.width * 3 + 1) + 1, row * this.width * 3, (row + 1) * this.width * 3);
    return Buffer.concat([SIGNATURE, chunk('IHDR', header), chunk('IDAT', deflateSync(rows)), chunk('IEND', Buffer.alloc(0))]);
  }
}
export function flowRaster(view, width, height) {
  const image = new Raster(width, height); const labels = ['WAIT', 'READY', 'PICK', 'PACK', 'SHIP', 'PART'];
  const maximum = view.flow.reduce((max, row) => BigInt(row.orders) > max ? BigInt(row.orders) : max, 1n);
  const gap = 8; const cell = (width - gap * 7) / 6;
  view.flow.forEach((row, index) => {
    const x = gap + index * (cell + gap); const count = BigInt(row.orders); const bar = Number(count * BigInt(height - 46) / maximum);
    image.rect(x, 8, cell, height - 16, palette.card).rect(x, 8, cell, 3, index === 5 ? palette.amber : palette.cyan);
    image.text(labels[index], x + 6, 20, palette.text, 1);
    image.rect(x + 8, height - 16 - bar, cell - 16, bar, index === 5 ? palette.amber : palette.cyan);
  });
  return image;
}
export function stockRaster(items, width, height) {
  const image = new Raster(width, height); const rowBudget = Math.max(1, Math.min(3, Math.floor(height / 36))); const columns = Math.max(1, Math.ceil(items.length / rowBudget)); const rows = Math.max(1, Math.ceil(items.length / columns));
  const maximum = items.reduce((max, row) => BigInt(row.on_hand_each) > max ? BigInt(row.on_hand_each) : max, 1n);
  items.forEach((item, index) => {
    const cellWidth = width / columns; const cellHeight = height / rows; const x = index % columns * cellWidth + 4; const y = Math.floor(index / columns) * cellHeight + 4;
    const length = Math.max(0, Math.floor(cellWidth - 20)); const onHand = BigInt(item.on_hand_each); const reserved = BigInt(item.reserved_each);
    image.rect(x, y, cellWidth - 8, cellHeight - 8, palette.card).text(index + 1, x + 6, y + 5, palette.text);
    image.rect(x + 6, y + cellHeight - 23, length, 7, palette.edge);
    image.rect(x + 6, y + cellHeight - 23, Number(onHand * BigInt(length) / maximum), 7, palette.cyan);
    image.rect(x + 6, y + cellHeight - 23, Number(reserved * BigInt(length) / maximum), 7, palette.amber);
  });
  return image;
}
export function planRaster(plan, width, height, { zoom = 1, centerX = plan.width / 2, centerY = plan.height / 2, selected = '' } = {}) {
  const image = new Raster(width, height); const scale = Math.min((width - 20) / plan.width, (height - 20) / plan.height) * zoom;
  const left = width / 2 - centerX * scale; const top = height / 2 - centerY * scale;
  image.rect(left, top, plan.width * scale, plan.height * scale, palette.edge).rect(left + 2, top + 2, plan.width * scale - 4, plan.height * scale - 4, palette.background);
  plan.bins.forEach((bin, index) => {
    const x = left + bin.x * scale; const y = top + bin.y * scale; const w = bin.width * scale; const h = bin.height * scale;
    const color = bin.code === selected ? palette.amber : BigInt(bin.on_hand_each) ? palette.cyan : palette.muted;
    image.rect(x, y, w, h, color).rect(x + 2, y + 2, w - 4, h - 4, palette.card);
    if (w >= String(index + 1).length * 6 + 10 && h >= 13 && x >= 0 && y >= 0) image.text(index + 1, x + 5, y + 4, color);
  });
  return image;
}
