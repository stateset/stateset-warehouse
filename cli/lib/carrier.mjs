// Carrier labels on packed packages, from the terminal. The WMS records the
// label a carrier issued (it never calls a carrier). Recording is not a
// handoff and moves no inventory; handoff stays the reviewed command.
import { safeText } from './client.mjs';

const DECIMAL = /^(0|[1-9][0-9]{0,11})(\.[0-9]{1,8})?$/;

/** Validate typed fields early; the server applies the same rules again. */
export function labelRequest({ carrier, service, tracking, amount, currency, url }) {
  const trimmed = value => String(value ?? '').trim();
  const fields = { carrier: trimmed(carrier), service: trimmed(service), tracking: trimmed(tracking), amount: trimmed(amount), currency: trimmed(currency).toUpperCase(), url: trimmed(url) };
  for (const [name, max] of [['carrier', 120], ['service', 120]]) {
    if (!fields[name] || fields[name].length > max) throw new Error(`${name[0].toUpperCase()}${name.slice(1)} must be 1-${max} characters`);
  }
  if (!/^[\x21-\x7e]{1,64}$/.test(fields.tracking)) throw new Error('Tracking number must be 1-64 printable ASCII characters without spaces');
  if (fields.amount && (!DECIMAL.test(fields.amount) || !/^[A-Z]{3}$/.test(fields.currency))) throw new Error('Rate needs an exact decimal amount (up to 8 decimals) and a three-letter currency');
  if (!/^https:\/\/\S{1,2040}$/.test(fields.url)) throw new Error('Label link must be an HTTPS URL');
  return {
    carrier: fields.carrier, service: fields.service, tracking_number: fields.tracking,
    rate: fields.amount ? { amount: fields.amount, currency: fields.currency } : null,
    carton: null, label: { format: 'url', url: fields.url },
  };
}

/** One line per label, newest last, marking the active one. */
export function labelLines(labels) {
  if (!Array.isArray(labels)) throw new Error('Carrier labels could not be verified');
  if (!labels.length) return ['No carrier label recorded yet.'];
  return labels.map(l => `${l.voided ? 'voided' : 'ACTIVE'} | ${safeText(l.carrier)} ${safeText(l.service)} | ${safeText(l.tracking_number)}${l.rate ? ` | ${safeText(l.rate.amount)} ${safeText(l.rate.currency)}` : ''}${l.voided ? ` | ${safeText(l.voided.reason)}` : ''}`);
}

const path = (client, warehouse, packageId) => client.warehousePath(warehouse, `/packages/${encodeURIComponent(packageId)}/carrier-labels`);

export async function listLabels(client, warehouse, packageId) {
  const response = await client.call('GET', path(client, warehouse, packageId));
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${safeText(response.data?.error ?? 'Carrier labels unavailable')}`);
  return response.data.labels;
}

export async function recordLabel(client, warehouse, packageId, body) {
  const response = await client.call('POST', path(client, warehouse, packageId), body);
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${safeText(response.data?.error ?? 'Carrier label not recorded')}`);
  return response.data;
}
