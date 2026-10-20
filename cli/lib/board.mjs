import { boundSession } from './journal.mjs';
import { safeText } from './client.mjs';
import { overviewLines } from './views.mjs';
import { workQueues } from './queues.mjs';
import { fit } from './format.mjs';

export function refreshInterval(value = '15') {
  if (!/^(0|[1-9]\d*)$/.test(value) || value !== '0' && (Number(value) < 5 || Number(value) > 300)) throw new Error('WMS_TUI_REFRESH_SECONDS must be 0 (paused) or a whole number from 5 to 300');
  return Number(value) * 1000;
}
export const laneShortcuts = { a: 'pending', k: 'picking', f: 'picked', g: 'packed', d: 'incoming', b: 'receiving', e: 'open', u: 'attention' };
export function laneFor(key, snapshot) {
  return workQueues(snapshot).find(queue => queue.filter === laneShortcuts[key] && (key !== 'e' || queue.kind === 'exceptions'));
}
const age = ms => ms < 60000 ? `${Math.floor(ms / 1000)}s` : ms < 3600000 ? `${Math.floor(ms / 60000)}m` : `${Math.floor(ms / 3600000)}h`;
const openOrder = order => !['shipped', 'cancelled', 'closed_partial'].includes(order.status);
export function attentionLines(snapshot, now) {
  const rows = [
    ...(snapshot.exceptions ?? []).filter(row => row.status === 'open').map(row => ({ rank: 0, at: row.created_at, text: `Issue ${row.category} | ${row.id}` })),
    ...(snapshot.orders ?? []).filter(row => openOrder(row) && ['urgent', 'high'].includes(row.priority)).map(row => ({ rank: row.priority === 'urgent' ? 1 : 2, at: row.created_at, text: `${row.priority} ${row.reference} | ${row.status}` })),
  ].sort((a, b) => {
    const first = Date.parse(a.at); const second = Date.parse(b.at);
    return a.rank - b.rank || (Number.isFinite(first) ? first : Infinity) - (Number.isFinite(second) ? second : Infinity);
  });
  return rows.slice(0, 3).map(row => `${row.text} | age ${Number.isFinite(Date.parse(row.at)) && Date.parse(row.at) <= now ? age(now - Date.parse(row.at)) : 'unknown'}`);
}

// Home-only read model. Callers await each refresh before opening another view;
// no timer, credential or snapshot survives in a background task or state file.
export class OperationsBoard {
  constructor(client, journal, scope, session, { intervalMs = 15000, now = Date.now } = {}) {
    this.client = client; this.journal = journal; this.scope = scope; this.session = session;
    this.intervalMs = intervalMs || 15000; this.paused = intervalMs === 0; this.now = now;
    this.snapshot = null; this.pending = { count: 0 }; this.error = ''; this.sessionError = ''; this.lastAttempt = -Infinity;
  }
  refresh({ verifySession = true } = {}) {
    if (this.inFlight) return this.inFlight;
    this.inFlight = this.read(verifySession).finally(() => { this.inFlight = null; this.lastAttempt = this.now(); });
    return this.inFlight;
  }
  async read(verifySession) {
    this.sessionError = '';
    if (verifySession) {
      try { this.session = await boundSession(this.client, this.scope); }
      catch (error) { this.sessionError = safeText(error.message); }
    }
    const readPending = this.journal.listForIdentity(this.scope)
      .then(records => { this.pending = { count: records.filter(record => !['committed', 'not_committed'].includes(record.stage)).length }; })
      .catch(error => { this.pending = { error: safeText(error.message) }; });
    if (!this.sessionError) {
      try {
        const snapshot = await this.client.read(this.client.warehousePath(this.scope.warehouse));
        this.snapshot = { ...snapshot, readAt: new Date(this.now()).toISOString() }; this.error = '';
      } catch (error) { this.error = safeText(error.message); }
    }
    await readPending;
  }
  async tick({ editing = false } = {}) {
    if (!editing && !this.paused && !this.sessionError && this.now() - this.lastAttempt >= this.intervalMs) await this.refresh();
  }
  togglePause() { this.paused = !this.paused; }
  get queueSnapshot() { return this.error || this.sessionError ? { error: this.error || this.sessionError } : this.snapshot ?? { error: 'No snapshot read yet' }; }
  context({ columns = 120 } = {}) {
    const now = this.now(); const elapsed = this.snapshot ? Math.max(0, now - Date.parse(this.snapshot.readAt)) : null;
    const stale = Boolean(this.error || this.sessionError || elapsed === null || elapsed >= Math.max(30000, this.intervalMs * 2));
    const status = `${stale ? 'STALE' : 'CURRENT'} · ${this.paused ? 'PAUSED' : `AUTO ${this.intervalMs / 1000}s`} · age ${elapsed === null ? '?' : age(elapsed)}`;
    const snapshot = this.snapshot ?? { error: this.error || this.sessionError || 'No snapshot read yet' };
    const overview = overviewLines(snapshot, this.pending, this.session, this.scope.warehouse);
    const count = key => laneFor(key, this.queueSnapshot)?.count ?? '?';
    const lanes = [`a Allocate ${count('a')} · k Pick ${count('k')} · f Pack ${count('f')} · g Ship ${count('g')}`, `d Inbound ${count('d')} · b Putaway ${count('b')} · e Issues ${count('e')} · u Priority ${count('u')}`];
    const warning = this.sessionError ? `ACTION: Session held · r reconnect · c local recovery` : stale ? 'STALE: Last read retained · r refresh' : '';
    const header = `${this.snapshot?.warehouse.code ?? this.scope.warehouse} | v${this.snapshot?.version ?? '?'} | ${status}${this.session.demo ? ' | DEMO' : ''}`;
    const attention = this.snapshot ? attentionLines(this.snapshot, now) : [];
    const problem = this.sessionError || this.error ? `Read held: ${this.sessionError || this.error}` : '';
    const recovery = overview.find(line => /^(ACTION:|Recovery:|Recovery check held:)/.test(line));
    const detailsIntro = [warning, header, ...lanes, ...overview, ...attention, problem, 'r refresh · z pause/resume · refresh pauses during typing and task views'].filter(Boolean);
    return {
      intro: [warning, header, ...lanes, overview.find(line => line.startsWith('On hand')),
        ...attention.slice(0, 2), `${overview[0]} | Facility: ${this.scope.warehouse} | Tenant: ${this.scope.tenant}`, recovery, problem,
      ].filter(Boolean).map(line => fit(line, columns).trimEnd()),
      compactIntro: [`${this.paused ? 'PAUSED' : `AUTO ${this.intervalMs / 1000}s`} · age ${elapsed === null ? '?' : age(elapsed)} | ${this.snapshot?.warehouse.code ?? this.scope.warehouse} v${this.snapshot?.version ?? '?'}${this.session.demo ? ' | DEMO' : ''}`,
        `a Alloc ${count('a')} · k Pick ${count('k')} · f Pack ${count('f')} · g Ship ${count('g')}`,
        `d In ${count('d')} · b Put ${count('b')} · e Issues ${count('e')} · u Prio ${count('u')}`],
      detailsIntro,
    };
  }
}
