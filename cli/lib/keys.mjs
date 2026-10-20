import { emitKeypressEvents } from 'node:readline';
import { fit, width, wrap } from './format.mjs';

const segments = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
const boundary = (text, cursor, direction) => {
  const positions = [...segments.segment(text)].map(part => part.index).concat(text.length);
  return direction < 0 ? positions.filter(index => index < cursor).at(-1) ?? 0 : positions.find(index => index > cursor) ?? text.length;
};

// One reader owns raw input for its whole lifetime. Keystrokes arriving in the
// same scanner/PTY chunk stay queued across screen changes and prompts.
export class Keys {
  constructor(input, output, columns) {
    this.input = input; this.output = output; this.columns = columns;
    this.queue = []; this.waiter = null; this.closed = false; this.previousRaw = input.isRaw;
    this.paste = null; this.lastCR = false;
    this.receive = (text, key = {}) => {
      if (key.ctrl && key.name === 'c') { this.close(); return; }
      if (key.name === 'paste-start') { this.paste = ''; this.pasteOverflow = false; return; }
      if (key.name === 'paste-end') { const paste = this.paste; this.paste = null; this.push({ paste, overflow: this.pasteOverflow }); return; }
      if (this.paste !== null) {
        if (this.paste.length + (text?.length ?? 0) <= 1024 * 1024) this.paste += text ?? '';
        else this.pasteOverflow = true;
        return;
      }
      if (text === '\n' && this.lastCR) { this.lastCR = false; return; }
      this.lastCR = text === '\r';
      this.push({ text, key });
    };
    this.end = () => this.close();
    emitKeypressEvents(input);
    input.on('keypress', this.receive); input.on('end', this.end);
    input.setRawMode(true); input.resume();
    output.write('\x1b[?2004h');
  }
  push(event) {
    if (this.closed) return;
    if (this.waiter) { const waiter = this.waiter; this.waiter = null; waiter(event); }
    else this.queue.push(event);
  }
  async next(timeoutMs) {
    if (this.closed) return null;
    if (this.queue.length) return this.queue.shift();
    return new Promise(resolve => {
      let timer;
      this.waiter = event => { clearTimeout(timer); this.waiter = null; resolve(event); };
      if (timeoutMs !== undefined) timer = setTimeout(() => this.waiter?.({ key: { name: 'tick' } }), Math.max(0, timeoutMs));
    });
  }
  async read(prompt, navigate = false, controls = {}, draft, { timeoutMs } = {}) {
    if (this.closed) return null;
    let buffer = draft?.buffer ?? ''; let cursor = draft?.cursor ?? 0;
    const deadline = timeoutMs === undefined ? undefined : performance.now() + timeoutMs;
    let label;
    const showPrompt = () => {
      const prompts = prompt.split('\n').flatMap(line => wrap(line, this.columns()));
      if (width(prompts.at(-1)) > this.columns() - 16) prompts.push('> ');
      for (const line of prompts.slice(0, -1)) this.output.write(`${line}\n`);
      label = prompts.at(-1); this.output.write(label);
    };
    showPrompt();
    const render = () => {
      const available = Math.max(1, this.columns() - width(label));
      let start = cursor; let used = 0;
      const before = [...segments.segment(buffer.slice(0, cursor))];
      for (const part of before.reverse()) {
        const size = width(part.segment);
        if (used + size > available - 2) break;
        used += size; start = part.index;
      }
      const prefix = start ? '‹' : '';
      const visible = fit(prefix + buffer.slice(start), available).trimEnd();
      const column = Math.min(this.columns(), width(label) + width(prefix + buffer.slice(start, cursor)));
      this.output.write(`\x1b[${width(label) + 1}G\x1b[K${visible}\x1b[${column + 1}G`);
    };
    render();
    for (;;) {
      const event = await this.next(deadline === undefined ? undefined : deadline - performance.now());
      if (!event) return null;
      const { key = {}, text } = event;
      if (Object.hasOwn(event, 'paste')) {
        if (event.paste === null) continue;
        if (event.overflow || buffer.length + event.paste.length > 1024 * 1024) throw new Error('Input exceeds 1 MiB; no value accepted');
        if (/[\r\n\x00-\x1f\x7f-\x9f]/.test(event.paste)) {
          this.output.write('\r\x1b[2K');
          for (const line of wrap('Paste one field at a time; multi-line paste was ignored.', this.columns())) this.output.write(`${line}\n`);
          this.output.write(label);
        } else { buffer = buffer.slice(0, cursor) + event.paste + buffer.slice(cursor); cursor += event.paste.length; }
      } else if (key.name === 'return' || key.name === 'enter') {
        this.output.write('\n'); return buffer;
      } else if (key.ctrl && key.name === 'd' && !buffer) { this.close(); return null; }
      else if (!buffer && Object.hasOwn(controls, key.name)) return { key: controls[key.name] };
      else if (navigate && key.name === 'escape') return { key: 'escape' };
      else if (key.name === 'escape') { const error = new Error('Entry cancelled'); error.code = 'INPUT_CANCELLED'; throw error; }
      else if (navigate && ['resize', 'tick'].includes(key.name)) return { key: key.name, draft: { buffer, cursor } };
      else if (navigate && !buffer && ['up', 'down', 'left', 'right', 'pageup', 'pagedown', 'home', 'end', 'tab', 'f1', 'f2', 'f3', 'f4'].includes(key.name)) return { key: key.name === 'tab' ? (key.shift ? 'up' : 'down') : key.name };
      else if (key.name === 'resize') { this.output.write('\n'); showPrompt(); }
      else if (key.name === 'backspace' || key.ctrl && key.name === 'h') {
        const previous = boundary(buffer, cursor, -1); buffer = buffer.slice(0, previous) + buffer.slice(cursor); cursor = previous;
      } else if (key.name === 'delete' || key.ctrl && key.name === 'd') buffer = buffer.slice(0, cursor) + buffer.slice(boundary(buffer, cursor, 1));
      else if (key.name === 'left') cursor = boundary(buffer, cursor, -1);
      else if (key.name === 'right') cursor = boundary(buffer, cursor, 1);
      else if (key.name === 'home' || key.ctrl && key.name === 'a') cursor = 0;
      else if (key.name === 'end' || key.ctrl && key.name === 'e') cursor = buffer.length;
      else if (key.ctrl && key.name === 'u') { buffer = buffer.slice(cursor); cursor = 0; }
      else if (key.ctrl && key.name === 'k') buffer = buffer.slice(0, cursor);
      else if (!key.ctrl && !key.meta && typeof text === 'string' && !/[\x00-\x1f\x7f-\x9f]/.test(text)) {
        if (buffer.length + text.length > 1024 * 1024) throw new Error('Input exceeds 1 MiB; no value accepted');
        buffer = buffer.slice(0, cursor) + text + buffer.slice(cursor); cursor += text.length;
      }
      render();
    }
  }
  close() {
    if (this.closed) return;
    this.closed = true; this.queue = [];
    this.input.removeListener('keypress', this.receive); this.input.removeListener('end', this.end);
    this.input.setRawMode(Boolean(this.previousRaw)); this.input.pause();
    this.output.write('\x1b[?2004l\x1b[0m');
    this.waiter?.(null); this.waiter = null;
  }
}
