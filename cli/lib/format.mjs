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

