import * as fs from 'node:fs/promises';
import { constants } from 'node:fs';
import { createHash, randomUUID } from 'node:crypto';
import path from 'node:path';
import { parseJson } from './client.mjs';
const hash = text => createHash('sha256').update(text).digest('hex');
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const terminal = new Set(['committed', 'not_committed']);
const key = scope => hash(JSON.stringify([scope.origin, scope.tenant, scope.actor, scope.warehouse]));
function validScope(scope) {
  if (!scope || !['origin', 'tenant', 'actor', 'warehouse'].every(k => typeof scope[k] === 'string' && scope[k])) throw new Error('Incomplete original request scope');
}
async function directory(name) {
  const absolute = path.resolve(name);
  let current = path.parse(absolute).root;
  for (const part of absolute.slice(current.length).split(path.sep).filter(Boolean)) {
    current = path.join(current, part);
    let created = false;
    await fs.mkdir(current, { mode: 0o700 }).then(() => { created = true; }).catch(e => { if (e.code !== 'EEXIST') throw e; });
    const stat = await fs.lstat(current);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error('Recovery directories cannot be symlinks');
    // Persist each new directory's entry, including the identity-scope parent.
    // Syncing only the request directory does not durably publish its ancestors.
    if (created) await syncDirectory(path.dirname(current));
  }
  const stat = await fs.stat(absolute);
  if ((stat.mode & 0o077) || stat.uid !== process.getuid()) throw new Error('Recovery directory must be owned by this user with mode 0700');
}
async function syncDirectory(name) {
  const file = await fs.open(name, constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW);
  try { await file.sync(); } finally { await file.close(); }
}
async function privateRead(name) {
  const file = await fs.open(name, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = await file.stat();
    if (!stat.isFile() || (stat.mode & 0o077) || stat.uid !== process.getuid() || stat.size > 32 * 1024 * 1024) throw new Error('Unsafe recovery file');
    return await file.readFile('utf8');
  } finally { await file.close(); }
}
export class Journal {
  constructor(root, { beforePublish } = {}) { this.root = path.resolve(root); this.beforePublish = beforePublish; }
  async publish(dir, filename, value) {
    const temp = path.join(dir, `.pending-${randomUUID()}`);
    const body = JSON.stringify(value); const file = await fs.open(temp, 'wx', 0o600);
    try { await file.writeFile(body); await file.sync(); } finally { await file.close(); }
    try {
      await this.beforePublish?.(filename);
      await fs.link(temp, path.join(dir, filename));
      await syncDirectory(dir);
    } finally { await fs.unlink(temp); }
    return hash(body);
  }
  async prepare(scope, request) {
    validScope(scope);
    // Serialize once. This exact body is retained across all uncertain attempts.
    const body = JSON.stringify(request); const checked = parseJson(body);
    if (!uuid.test(checked.request_id) || !Number.isSafeInteger(checked.expected_version) || checked.expected_version < 0 || !checked.command?.type) throw new Error('Invalid original request');
    await directory(this.root);
    const parent = path.join(this.root, key(scope)); await directory(parent);
    const dir = path.join(parent, request.request_id); await fs.mkdir(dir, { mode: 0o700 }); await syncDirectory(parent);
    await this.publish(dir, '000000-original.json', { schema: 1, stage: 'original', scope, body, body_sha256: hash(body), created_at: new Date().toISOString() });
    return this.load(dir);
  }
  async load(dir) {
    const absolute = path.resolve(dir);
    if (path.dirname(path.dirname(absolute)) !== this.root || !uuid.test(path.basename(absolute))) throw new Error('Invalid recovery journal path');
    await directory(absolute);
    const names = (await fs.readdir(absolute)).filter(n => !n.startsWith('.pending-')).sort();
    if (!names.length || names[0] !== '000000-original.json') throw new Error('Incomplete recovery journal: no original request');
    const events = []; let previous = null;
    for (const [index, name] of names.entries()) {
      if (!new RegExp(`^${String(index).padStart(6, '0')}-[a-z_]+\\.json$`).test(name)) throw new Error('Recovery history sequence is broken');
      const raw = await privateRead(path.join(absolute, name)); const event = parseJson(raw);
      if (index && !['send_intent', 'uncertain', 'committed', 'not_committed'].includes(event.stage)) throw new Error('Unknown recovery history stage');
      if (index && (event.previous_sha256 !== previous || terminal.has(events.at(-1).stage))) throw new Error('Recovery history was changed or extended after completion');
      if (name !== `${String(index).padStart(6, '0')}-${event.stage}.json`) throw new Error('Recovery stage does not match its original file');
      events.push(event); previous = hash(raw);
    }
    const original = events[0]; validScope(original.scope);
    const request = parseJson(original.body);
    if (original.schema !== 1 || original.stage !== 'original' || hash(original.body) !== original.body_sha256 || path.basename(absolute) !== request.request_id || path.basename(path.dirname(absolute)) !== key(original.scope)) throw new Error('Original request or identity scope was changed');
    return { dir: absolute, original, request, events, previous, stage: events.at(-1).stage };
  }
  async append(record, stage, details = {}) {
    if (!['send_intent', 'uncertain', 'committed', 'not_committed'].includes(stage)) throw new Error('Unknown recovery stage');
    const current = await this.load(record.dir);
    if (terminal.has(current.stage)) throw new Error('Original request is already resolved');
    if (current.previous !== record.previous) throw new Error('Recovery journal changed in another process; reopen it');
    const filename = `${String(current.events.length).padStart(6, '0')}-${stage}.json`;
    await this.publish(record.dir, filename, { stage, previous_sha256: current.previous, recorded_at: new Date().toISOString(), details });
    return this.load(record.dir);
  }
  async list(scope) {
    validScope(scope); await directory(this.root);
    const parent = path.join(this.root, key(scope));
    try { await fs.lstat(parent); } catch (e) { if (e.code === 'ENOENT') return []; throw e; }
    await directory(parent);
    const records = [];
    for (const name of (await fs.readdir(parent)).sort()) {
      if (!uuid.test(name)) throw new Error('Unexpected recovery directory entry');
      records.push(await this.load(path.join(parent, name)));
    }
    return records;
  }
  async listForIdentity(identity) {
    if (!['origin', 'tenant', 'actor'].every(k => typeof identity[k] === 'string' && identity[k])) throw new Error('Incomplete recovery identity');
    await directory(this.root);
    const records = [];
    for (const name of (await fs.readdir(this.root)).sort()) {
      if (!/^[a-f0-9]{64}$/.test(name)) throw new Error('Unexpected recovery scope directory');
      const parent = path.join(this.root, name); await directory(parent);
      for (const id of (await fs.readdir(parent)).sort()) {
        const record = await this.load(path.join(parent, id));
        if (['origin', 'tenant', 'actor'].every(k => record.original.scope[k] === identity[k])) records.push(record);
      }
    }
    return records;
  }
}
export async function boundSession(client, scope) {
  const session = await client.session();
  if (scope.origin !== client.origin || scope.tenant !== session.identity.tenant || scope.actor !== session.identity.actor) throw new Error('Original request belongs to a different server, tenant or actor');
  return session;
}
export function rejectionMatches(proof, record) {
  const { scope } = record.original; const r = record.request;
  if (proof?.status !== 'not_committed' || proof.tenant !== scope.tenant || proof.actor !== scope.actor || proof.warehouse_id !== scope.warehouse || proof.request_id !== r.request_id || proof.expected_version !== r.expected_version || !Number.isSafeInteger(proof.current_version) || proof.current_version < r.expected_version) return false;
  if (r.resource_versions !== undefined) {
    const a = r.resource_versions, b = proof.expected_resource_versions, c = proof.current_resource_versions;
    return Object.keys(proof).length === 10 && proof.reason === 'changed_resources' && b && c &&
      Object.keys(a).length === Object.keys(b).length && Object.keys(a).length === Object.keys(c).length &&
      Object.keys(a).every(k => a[k] === b[k] && Number.isSafeInteger(c[k]) && c[k] >= a[k]) &&
      Object.keys(a).some(k => c[k] > a[k]);
  }
  return Object.keys(proof).length === 8 && proof.reason === 'stale_version' && proof.current_version > r.expected_version && proof.expected_resource_versions === undefined;
}
export async function sendOriginal(client, journal, record) {
  record = await journal.load(record.dir);
  if (terminal.has(record.stage)) return record;
  await boundSession(client, record.original.scope);
  record = await journal.append(record, 'send_intent'); // Disk failure prevents any send.
  let response;
  try {
    response = await client.call('POST', client.warehousePath(record.original.scope.warehouse, '/commands'), record.original.body);
  } catch {
    return journal.append(record, 'uncertain', { reason: 'No complete server response; retain this original request' });
  }
  if (response.ok && response.data?.result && typeof response.data.replayed === 'boolean') {
    return journal.append(record, 'committed', { response: response.data });
  }
  if (response.status === 409 && rejectionMatches(response.data?.command_outcome, record)) {
    return journal.append(record, 'not_committed', { response: response.data });
  }
  return journal.append(record, 'uncertain', { http_status: response.status, response: response.data });
}
