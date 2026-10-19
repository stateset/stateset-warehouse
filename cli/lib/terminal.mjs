import { createInterface } from 'node:readline';
import { safeText } from './client.mjs';

import { fit, wrap } from './format.mjs';
import { Keys } from './keys.mjs';
import { keyboardHelp } from './views.mjs';
export { fit, wrap, width } from './format.mjs';

export class Terminal {
  constructor(input = process.stdin, output = process.stdout, env = process.env) {
    this.input = input; this.output = output; this.lines = []; this.waiter = null; this.closed = false;
    this.colors = Boolean(output.isTTY && env.TERM !== 'dumb' && !Object.hasOwn(env, 'NO_COLOR'));
    if (input.isTTY && output.isTTY && typeof input.setRawMode === 'function') {
      output.write('\x1b[?1049h'); this.alternate = true;
      try { this.keys = new Keys(input, output, () => this.columns); }
      catch (error) { output.write('\x1b[?1049l'); throw error; }
      this.resize = () => this.keys.push({ key: { name: 'resize' } });
      output.on('resize', this.resize);
      this.exitCleanup = () => this.close();
      this.signals = Object.entries({ SIGHUP: 129, SIGINT: 130, SIGTERM: 143 }).map(([signal, code]) => {
        const handler = () => { this.close(); process.exit(code); };
        process.once(signal, handler); return [signal, handler];
      });
      process.once('exit', this.exitCleanup);
    } else {
      this.rl = createInterface({ input, output, terminal: false });
      this.rl.on('line', line => { if (this.waiter) { const resolve = this.waiter; this.waiter = null; resolve(line); } else this.lines.push(line); });
      this.rl.on('close', () => { this.closed = true; this.waiter?.(null); this.waiter = null; });
      this.rl.on('SIGINT', () => this.rl.close());
    }
  }
  get columns() { return Math.max(20, (this.output.columns || 80) - 1); }
  get rows() { return Math.max(10, this.output.rows || 24); }
  print(text = '') { this.output.write(`${text}\n`); }
  style(text, tone = '') {
    const codes = { accent: '1;36', muted: '2', title: '1', selected: '7;1', success: '32', warning: '33', danger: '1;31' };
    return this.colors && codes[tone] ? `\x1b[${codes[tone]}m${safeText(text)}\x1b[0m` : safeText(text);
  }
  line(text = '', tone = '') { for (const line of wrap(text, this.columns)) this.print(this.style(line, tone)); }
  async ask(prompt) {
    if (this.keys) return this.keys.read(prompt);
    if (this.closed && !this.lines.length) return null;
    this.output.write(prompt);
    if (this.lines.length) return this.lines.shift();
    return new Promise(resolve => { this.waiter = resolve; });
  }
  async action(prompt) { return this.keys ? this.keys.read(prompt, true) : this.ask(prompt); }
  async scan(prompt) { return this.keys ? this.keys.read(prompt, false, { f2: 'undo' }) : this.ask(prompt); }
  screen(title) {
    if (this.output.isTTY) this.output.write('\x1b[2J\x1b[H');
    this.line('STATESET / WAREHOUSE', 'accent'); this.line(title, 'title'); this.line('─'.repeat(this.columns), 'muted');
  }
  json(value) { this.print(JSON.stringify(value, (_key, item) => typeof item === 'string' ? safeText(item) : item, 2)); }
  async menu(title, items, { intro = [], shortcuts = {}, hint = '', sections = {} } = {}) {
    let query = ''; let page = 0; let message = ''; let selected = 1;
    for (;;) {
      this.screen(title); intro.forEach(line => this.line(line, line.startsWith('ACTION:') || line.includes('unavailable:') || line.startsWith('Recovery check held:') ? 'danger' : line.includes('SYNTHETIC DEMO') ? 'warning' : line.startsWith('Snapshot at') || line.startsWith('Recovery:') ? 'muted' : ''));
      if (intro.length) this.print();
      const filtered = items.map(([label, value], index) => ({ label, value, number: index + 1 }))
        .filter(item => item.label.toLowerCase().includes(query.toLowerCase()));
      if (!filtered.some(item => item.number === selected)) selected = filtered[0]?.number;
      // Keep the prompt visible even in short panes. Numbers stay stable when
      // searching or paging so existing operator muscle memory still applies.
      const used = 2 + wrap(title, this.columns).length + intro.reduce((n, line) => n + wrap(line, this.columns).length, 0) + (intro.length ? 1 : 0);
      const footer = hint ? (Array.isArray(hint) ? [...hint] : [hint]) : ['/text search · ? help · q back / exit'];
      if (this.keys) footer.push('↑↓ move · Enter open · Esc back · F1 help');
      const reserved = 3 + footer.reduce((n, line) => n + wrap(line, this.columns).length, 0)
        + Object.keys(sections).length
        + (query ? wrap(`Search: ${query}`, this.columns).length : 0)
        + (message ? wrap(message, this.columns).length : 0);
      const count = Math.max(1, this.rows - used - reserved);
      if (this.keys && selected !== undefined) page = Math.floor(filtered.findIndex(item => item.number === selected) / count);
      page = Math.min(page, Math.max(0, Math.ceil(filtered.length / count) - 1));
      let section;
      filtered.slice(page * count, (page + 1) * count).forEach(item => {
        const group = Object.keys(sections).map(Number).filter(number => number <= item.number).at(-1);
        if (group !== undefined && section !== group) { this.line(sections[group], 'accent'); section = group; }
        const active = item.number === selected;
        this.print(this.style(fit(`${this.keys ? active ? '› ' : '  ' : ''}${String(item.number).padStart(2)}. ${item.label}`, this.columns).trimEnd(), this.keys && active ? 'selected' : ''));
      });
      if (!filtered.length) this.line('No matching actions. Enter / to clear the search.');
      if (filtered.length > count) this.line(`Page ${page + 1}/${Math.ceil(filtered.length / count)} · n next · p previous`);
      if (query) this.line(`Search: ${query}`);
      footer.forEach(line => this.line(line, 'muted'));
      if (message) this.line(message, 'warning');
      const raw = await this.action('\nSelect: ');
      if (raw && typeof raw === 'object') {
        if (raw.key === 'escape') return null;
        if (raw.key === 'f1') { await keyboardHelp(this); continue; }
        let index = Math.max(0, filtered.findIndex(item => item.number === selected));
        if (raw.key === 'up') index--;
        if (raw.key === 'down') index++;
        if (['left', 'pageup'].includes(raw.key)) index -= count;
        if (['right', 'pagedown'].includes(raw.key)) index += count;
        if (raw.key === 'home') index = 0;
        if (raw.key === 'end') index = filtered.length - 1;
        index = Math.max(0, Math.min(filtered.length - 1, index));
        selected = filtered[index]?.number; page = Math.floor(index / count); message = ''; continue;
      }
      const answer = raw?.trim();
      if (answer === undefined || answer === 'q') return null;
      if (answer === '' && this.keys && selected !== undefined) return filtered.find(item => item.number === selected).value;
      if (answer === '?') { await keyboardHelp(this); continue; }
      if (answer.startsWith('/')) { query = answer.slice(1); page = 0; message = ''; continue; }
      if (answer === 'n' || answer === 'p') {
        page = Math.max(0, Math.min(page + (answer === 'n' ? 1 : -1), Math.max(0, Math.ceil(filtered.length / count) - 1)));
        selected = filtered[page * count]?.number; continue;
      }
      if (Object.hasOwn(shortcuts, answer)) return shortcuts[answer];
      const item = filtered.find(item => String(item.number) === answer);
      if (item) return item.value;
      message = 'Choose a listed number, /text to search, or q to go back.';
    }
  }
  async pause() { await this.ask('\nEnter to continue: '); }
  close() {
    if (this.exitCleanup) process.removeListener('exit', this.exitCleanup);
    for (const [signal, handler] of this.signals ?? []) process.removeListener(signal, handler);
    if (this.resize) this.output.removeListener('resize', this.resize);
    this.keys?.close(); this.rl?.close();
    if (this.alternate) { this.output.write('\x1b[0m\x1b[?1049l'); this.alternate = false; }
  }
}
