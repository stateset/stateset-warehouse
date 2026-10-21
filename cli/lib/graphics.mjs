import { Transform } from 'node:stream';
import { randomInt } from 'node:crypto';
import { deflateSync } from 'node:zlib';
import { safePng } from './raster.mjs';

const ESC = '\x1b';
const START = Buffer.from(`${ESC}_G`);
const END = Buffer.from(`${ESC}\\`);
const command = (metadata, payload = '') => `${ESC}_G${metadata};${payload}${ESC}\\`;

export function graphicsMode(value = 'auto') {
  if (!['auto', 'off', 'kitty'].includes(value)) throw new Error('WMS_TUI_GRAPHICS must be auto, off, or kitty');
  return value;
}

// Terminal replies share stdin with scanners. Filter complete protocol frames
// before readline interprets them as keystrokes; preserve every ordinary byte.
export class GraphicsReplies extends Transform {
  constructor(onReply) {
    super(); this.onReply = onReply; this.pending = Buffer.alloc(0); this.discarding = false;
  }
  _transform(chunk, _encoding, done) {
    clearTimeout(this.timer);
    let bytes = this.pending.length ? Buffer.concat([this.pending, chunk]) : chunk;
    this.pending = Buffer.alloc(0);
    if (this.discarding) {
      const end = bytes.indexOf(END);
      if (end < 0) { this.pending = bytes.at(-1) === 27 ? bytes.subarray(-1) : Buffer.alloc(0); this.expire(); done(); return; }
      bytes = bytes.subarray(end + END.length); this.discarding = false;
    }
    while (bytes.length) {
      const start = bytes.indexOf(27);
      if (start < 0) { this.push(bytes); break; }
      if (start) { this.push(bytes.subarray(0, start)); bytes = bytes.subarray(start); }
      if (bytes.length < START.length && START.subarray(0, bytes.length).equals(bytes)) {
        this.pending = bytes;
        // A lone Esc remains a navigation key. Once APC's introducer is
        // present, never forward a delayed terminal reply as scanner text.
        if (bytes.length === 1) this.timer = setTimeout(() => { this.push(this.pending); this.pending = Buffer.alloc(0); }, 25);
        else this.expire();
        break;
      }
      if (!bytes.subarray(0, START.length).equals(START)) { this.push(bytes.subarray(0, 1)); bytes = bytes.subarray(1); continue; }
      const end = bytes.indexOf(END, START.length);
      if (end < 0) {
        if (bytes.length > 4096) {
          this.discarding = true; this.emit('fault');
          this.pending = bytes.at(-1) === 27 ? bytes.subarray(-1) : Buffer.alloc(0);
        } else this.pending = bytes;
        this.expire(); break;
      }
      const frame = bytes.subarray(START.length, end).toString('latin1');
      if (end > 4096 || !/^i=[1-9]\d{0,9}(?:,p=[1-9]\d{0,9})?;[\x20-\x7e]{1,512}$/.test(frame)) this.emit('fault');
      else this.onReply(frame);
      bytes = bytes.subarray(end + END.length);
    }
    done();
  }
  expire() {
    this.timer = setTimeout(() => { this.pending = Buffer.alloc(0); this.discarding = false; this.emit('fault'); }, 1000);
  }
  _destroy(error, done) { clearTimeout(this.timer); this.pending = Buffer.alloc(0); done(error); }
}

export class KittyGraphics {
  constructor(output, env = process.env, { interactive = true, id = randomInt(1, 0x7fffffff) } = {}) {
    this.output = output; this.mode = graphicsMode(env.WMS_TUI_GRAPHICS);
    this.queryId = id; this.imageId = id + 0x80000000; this.closed = false; this.hasImage = false; this.pngCache = new WeakMap();
    this.enabled = interactive && output.isTTY && env.TERM !== 'dumb' && this.mode !== 'off'
      && (this.mode === 'kitty' || !env.TMUX && !env.STY && !Object.hasOwn(env, 'NO_COLOR'));
    this.state = this.enabled ? 'unknown' : 'off';
  }
  detect() {
    if (!this.enabled || this.closed) return Promise.resolve(false);
    if (this.query) return this.query;
    this.state = 'querying';
    this.query = new Promise(resolve => {
      this.resolve = resolve;
      this.timer = setTimeout(() => this.finish(false), 400);
      try { this.output.write(command(`a=q,t=d,f=24,s=1,v=1,i=${this.queryId}`, 'AAAA')); }
      catch { this.finish(false); }
    });
    return this.query;
  }
  finish(supported) {
    clearTimeout(this.timer);
    this.state = this.closed ? 'closed' : supported ? 'ready' : 'unavailable';
    this.resolve?.(supported); this.resolve = null;
  }
  reply(frame) {
    const match = /^i=(\d+)(?:,p=\d+)?;(.*)$/.exec(frame);
    if (!match) return;
    const id = Number(match[1]);
    if (id === this.queryId && this.state === 'querying') this.finish(match[2] === 'OK');
    else if (id === this.imageId && match[2] !== 'OK') this.state = 'unavailable';
  }
  async show(bytes, { width, height, columns, rows, png = false } = {}) {
    if (!Buffer.isBuffer(bytes) || bytes.length > 4 * 1024 * 1024) throw new Error('Graphic exceeds the bounded image size');
    if (![columns, rows].every(n => Number.isSafeInteger(n) && n > 0 && n <= 4096)) throw new Error('Invalid image placement');
    if (!png && (![width, height].every(n => Number.isSafeInteger(n) && n > 0 && n <= 2048)
      || width * height > 1024 * 1024 || bytes.length !== width * height * 3)) throw new Error('Invalid bounded RGB graphic');
    if (columns > (this.output.columns || 4096) || rows > (this.output.rows || 4096)) throw new Error('Graphic exceeds the terminal pane');
    if (png) { const original = bytes; bytes = this.pngCache.get(original) ?? safePng(original); this.pngCache.set(original, bytes); }
    if (!await this.detect() || this.state !== 'ready' || this.closed) return false;
    this.clear();
    const payload = (png ? bytes : deflateSync(bytes, { level: 3 })).toString('base64');
    const metadata = `a=T,t=d,f=${png ? '100' : `24,o=z,s=${width},v=${height}`},i=${this.imageId},p=1,c=${columns},r=${rows},C=1,q=2`;
    // One synchronous batch prevents text or another image from interleaving
    // with a chunked transfer. Every encoded chunk is <=4096 bytes.
    const frames = [];
    for (let offset = 0; offset < payload.length; offset += 4096) {
      const more = offset + 4096 < payload.length ? 1 : 0;
      frames.push(command(`${offset ? 'q=2' : metadata},m=${more}`, payload.slice(offset, offset + 4096)));
    }
    this.hasImage = true;
    await new Promise((resolve, reject) => this.output.write(frames.join(''), error => error ? reject(error) : resolve()));
    return !this.closed;
  }
  clear() {
    if (this.hasImage) this.output.write(command(`a=d,d=I,i=${this.imageId},q=2`));
    this.hasImage = false;
  }
  close() { this.clear(); this.closed = true; this.finish(false); }
}
