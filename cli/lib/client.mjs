import { readFile } from 'node:fs/promises';

export function parseJson(text) {
  return JSON.parse(text, (_key, value, context) => {
    if (typeof value === 'number' && (!Number.isSafeInteger(value) || !/^-?\d+$/.test(context?.source ?? String(value)))) {
      throw new Error('API numbers must be exact safe integers; encode decimal interop quantities as strings');
    }
    return value;
  });
}
export function safeText(value) {
  return String(value).replace(/[\x00-\x1f\x7f-\x9f\u202a-\u202e\u2066-\u2069]/g, c => `\\u${c.charCodeAt(0).toString(16).padStart(4, '0')}`);
}
export function originUrl(raw) {
  const url = new URL(raw);
  const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
  if ((url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback)) || url.username || url.password || url.search || url.hash || url.pathname !== '/') {
    throw new Error('WMS_URL must be an HTTPS origin (HTTP allowed only on loopback), without credentials, path, query or fragment');
  }
  return url.origin;
}
export async function tokenFromEnv(env = process.env) {
  if (env.WMS_TOKEN && env.WMS_TOKEN_FILE) throw new Error('Choose WMS_TOKEN or WMS_TOKEN_FILE');
  const token = env.WMS_TOKEN_FILE ? (await readFile(env.WMS_TOKEN_FILE, 'utf8')).trim() : env.WMS_TOKEN;
  if (!token || /\s/.test(token)) throw new Error('Set WMS_TOKEN or WMS_TOKEN_FILE to an operator bearer credential');
  return token;
}
export class Client {
  #token;
  constructor(origin, token, { fetchImpl = fetch, timeoutMs = 15000 } = {}) {
    this.origin = originUrl(origin); this.#token = token; this.fetch = fetchImpl; this.timeoutMs = timeoutMs;
  }
  async call(method, path, body) {
    if (!path.startsWith('/v1/') || path.startsWith('//') || new URL(path, this.origin).origin !== this.origin) throw new Error('Invalid WMS API path');
    const response = await this.fetch(new URL(path, this.origin), {
      method, redirect: 'error', signal: AbortSignal.timeout(this.timeoutMs),
      headers: { authorization: `Bearer ${this.#token}`, accept: 'application/json', ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
      ...(body === undefined ? {} : { body: typeof body === 'string' ? body : JSON.stringify(body) }),
    });
    // Bound response memory even when a server or proxy misbehaves.
    const reader = response.body?.getReader(); const chunks = []; let bytes = 0;
    if (reader) {
      try {
        for (;;) {
          const { done, value } = await reader.read(); if (done) break;
          bytes += value.length; if (bytes > 16 * 1024 * 1024) throw new Error('WMS response exceeds 16 MiB'); chunks.push(value);
        }
      } finally { await reader.cancel().catch(() => {}); }
    }
    const data = parseJson(Buffer.concat(chunks).toString('utf8'));
    return { status: response.status, ok: response.ok, data };
  }
  async read(path) {
    const response = await this.call('GET', path);
    if (!response.ok) throw new Error(`HTTP ${response.status}: ${safeText(response.data.error ?? response.data.message ?? 'WMS read failed')}`);
    return response.data;
  }
  async image(path) {
    const url = new URL(path, this.origin);
    if (!path.startsWith('/v1/') || path.startsWith('//') || url.origin !== this.origin || !url.pathname.startsWith('/v1/') || url.hash) throw new Error('Invalid WMS image path');
    const response = await this.fetch(url, { method: 'GET', redirect: 'error', signal: AbortSignal.timeout(this.timeoutMs),
      headers: { authorization: `Bearer ${this.#token}`, accept: 'image/png' } });
    const reader = response.body?.getReader(); const chunks = []; let bytes = 0;
    try {
      if (!response.ok || response.headers.get('content-type')?.split(';')[0].trim() !== 'image/png') throw new Error(`Product image unavailable (HTTP ${response.status}); refresh the preview`);
      if (!reader) throw new Error('Product image response is empty');
      for (;;) {
        const { done, value } = await reader.read(); if (done) break;
        bytes += value.length; if (bytes > 4 * 1024 * 1024) throw new Error('Product image exceeds 4 MiB'); chunks.push(value);
      }
      return Buffer.concat(chunks);
    } finally { await reader?.cancel().catch(() => {}); }
  }
  async session() {
    const session = await this.read('/v1/session');
    const identity = session.identity;
    if (!identity || !['tenant', 'actor', 'role'].every(k => typeof identity[k] === 'string' && identity[k])) throw new Error('WMS did not return a complete authenticated identity');
    return session;
  }
  warehousePath(warehouse, suffix = '') {
    if (typeof warehouse !== 'string' || !warehouse || ['.', '..'].includes(warehouse) || /[\x00-\x1f\x7f-\x9f]/.test(warehouse) || Buffer.byteLength(warehouse) > 240) throw new Error('Invalid original warehouse ID');
    return `/v1/warehouses/${encodeURIComponent(warehouse)}${suffix}`;
  }
}
