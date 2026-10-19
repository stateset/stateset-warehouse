import { createInterface } from 'node:readline';
import { safeText } from './client.mjs';

const segments = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
function cellWidth(value) {
  if (/^[\p{Mark}\u200d\ufe0f]+$/u.test(value)) return 0;
  const n = value.codePointAt(0);
  return /\p{Extended_Pictographic}|\p{Regional_Indicator}/u.test(value) || n >= 0x1100 && (
    n <= 0x115f || n === 0x2329 || n === 0x232a || n >= 0x2e80 && n <= 0xa4cf ||
    n >= 0xac00 && n <= 0xd7a3 || n >= 0xf900 && n <= 0xfaff ||
    n >= 0xfe10 && n <= 0xfe6f || n >= 0xff01 && n <= 0xff60 ||
    n >= 0xffe0 && n <= 0xffe6 || n >= 0x20000 && n <= 0x3fffd) ? 2 : 1;
}
export function width(value) {
  return [...segments.segment(safeText(String(value)))].reduce((total, { segment }) => total + cellWidth(segment), 0);
}
export function fit(value, size, right = false) {
  const text = safeText(String(value ?? '—'));
  let result = ''; let used = 0;
  if (width(text) <= size) result = text;
  else {
    for (const { segment } of segments.segment(text)) {
      if (used + cellWidth(segment) > size - 1) break;
      result += segment; used += cellWidth(segment);
    }
    result += size > 0 ? '…' : '';
  }
  const padding = ' '.repeat(Math.max(0, size - width(result)));
  return right ? padding + result : result + padding;
}
export function wrap(value, size) {
  const lines = []; let line = ''; let used = 0;
  for (const { segment } of segments.segment(safeText(String(value)))) {
    const cells = cellWidth(segment);
    if (used + cells > size && line) { lines.push(line); line = ''; used = 0; }
    line += segment; used += cells;
  }
  lines.push(line); return lines;
}

export class Terminal {
  constructor(input = process.stdin, output = process.stdout) {
    this.input = input; this.output = output; this.lines = []; this.waiter = null; this.closed = false;
    this.rl = createInterface({ input, output, terminal: input.isTTY && output.isTTY });
    this.rl.on('line', line => { if (this.waiter) { const resolve = this.waiter; this.waiter = null; resolve(line); } else this.lines.push(line); });
    this.rl.on('close', () => { this.closed = true; this.waiter?.(null); this.waiter = null; });
    this.rl.on('SIGINT', () => this.rl.close());
  }
  get columns() { return Math.max(20, (this.output.columns || 80) - 1); }
  get rows() { return Math.max(10, this.output.rows || 24); }
  print(text = '') { this.output.write(`${text}\n`); }
  line(text = '') { for (const line of wrap(text, this.columns)) this.print(line); }
  async ask(prompt) {
    if (this.closed && !this.lines.length) return null;
    this.output.write(prompt);
    if (this.lines.length) return this.lines.shift();
    return new Promise(resolve => { this.waiter = resolve; });
  }
  screen(title) {
    if (this.output.isTTY) this.output.write('\x1b[2J\x1b[H');
    this.line('STATESET / WAREHOUSE'); this.line(title); this.print('─'.repeat(this.columns));
  }
  json(value) { this.print(JSON.stringify(value, (_key, item) => typeof item === 'string' ? safeText(item) : item, 2)); }
  async menu(title, items, { intro = [], shortcuts = {}, hint = '' } = {}) {
    let query = ''; let page = 0; let message = '';
    for (;;) {
      this.screen(title); intro.forEach(line => this.line(line));
      if (intro.length) this.print();
      const filtered = items.map(([label, value], index) => ({ label, value, number: index + 1 }))
        .filter(item => item.label.toLowerCase().includes(query.toLowerCase()));
      // Keep the prompt visible even in short panes. Numbers stay stable when
      // searching or paging so existing operator muscle memory still applies.
      const used = 3 + intro.reduce((n, line) => n + wrap(line, this.columns).length, 0) + (intro.length ? 1 : 0);
      const footer = hint ? (Array.isArray(hint) ? hint : [hint]) : ['/text search · ? help · q back / exit'];
      const reserved = 3 + footer.reduce((n, line) => n + wrap(line, this.columns).length, 0)
        + (query ? wrap(`Search: ${query}`, this.columns).length : 0)
        + (message ? wrap(message, this.columns).length : 0);
      const count = Math.max(1, this.rows - used - reserved);
      page = Math.min(page, Math.max(0, Math.ceil(filtered.length / count) - 1));
      filtered.slice(page * count, (page + 1) * count).forEach(item => this.print(fit(`${String(item.number).padStart(2)}. ${item.label}`, this.columns).trimEnd()));
      if (!filtered.length) this.line('No matching actions. Enter / to clear the search.');
      if (filtered.length > count) this.line(`Page ${page + 1}/${Math.ceil(filtered.length / count)} · n next · p previous`);
      if (query) this.line(`Search: ${query}`);
      footer.forEach(line => this.line(line));
      if (message) this.line(message);
      const raw = await this.ask('\nSelect: '); const answer = raw?.trim();
      if (answer === undefined || answer === 'q') return null;
      if (answer === '?') { message = 'Type a number or shortcut, then Enter. / clears search.'; continue; }
      if (answer.startsWith('/')) { query = answer.slice(1); page = 0; message = ''; continue; }
      if (answer === 'n') { page = Math.min(page + 1, Math.max(0, Math.ceil(filtered.length / count) - 1)); continue; }
      if (answer === 'p') { page = Math.max(0, page - 1); continue; }
      if (Object.hasOwn(shortcuts, answer)) return shortcuts[answer];
      const item = filtered.find(item => String(item.number) === answer);
      if (item) return item.value;
      message = 'Choose a listed number, /text to search, or q to go back.';
    }
  }
  async pause() { await this.ask('\nEnter to continue: '); }
  close() { this.rl.close(); }
}
