import { createHash } from 'node:crypto';
import { pngInfo } from './raster.mjs';

export const flowStatuses = ['pending', 'allocated', 'picking', 'picked', 'packed', 'partially_shipped'];
export const flowLabels = ['Awaiting allocation', 'Ready to pick', 'Picking', 'Ready to pack', 'Ready to hand off', 'Partly shipped'];
const fail = () => { throw new Error('Visual console evidence is incomplete or changed; refresh the review'); };
const text = (value, max = 1024) => typeof value === 'string' && value.length > 0 && Buffer.byteLength(value) <= max && !/[\x00-\x1f\x7f-\x9f]/.test(value);
const count = value => typeof value === 'string' && /^(0|[1-9]\d{0,38})$/.test(value) && BigInt(value) <= (1n << 127n) - 1n;
const small = (value, min, max) => Number.isSafeInteger(value) && value >= min && value <= max;
const quantity = row => row && count(row.on_hand_each) && count(row.reserved_each) && BigInt(row.reserved_each) <= BigInt(row.on_hand_each);
function envelope(value, scope) {
  if (!value || value.schema_version !== 1 || value.tenant !== scope.tenant || value.warehouse_id !== scope.warehouse || value.source_ok !== true
    || typeof value.demo !== 'boolean' || !count(value.version) || BigInt(value.version) > 9223372036854775807n || !text(value.captured_at, 64) || !Number.isFinite(Date.parse(value.captured_at))) fail();
}
export function validateConsole(value, scope, { after = null, review = null, limit = 64 } = {}) {
  envelope(value, scope);
  if (value.unit !== 'each' || !text(value.warehouse_name) || !quantity(value.stock) || !count(value.open_exceptions)
    || !Array.isArray(value.flow) || value.flow.length !== flowStatuses.length
    || value.flow.some((row, index) => row.status !== flowStatuses[index] || !count(row.orders))) fail();
  const bins = value.bins;
  if (!bins || !Array.isArray(bins.items) || bins.items.length > limit || !small(bins.total, bins.items.length, 1000000)
    || !(bins.next_after === null || text(bins.next_after, 240)) || review !== null && value.version !== review) fail();
  let previous = after; let physical = 0n; let reserved = 0n;
  for (const bin of bins.items) {
    if (!text(bin.code, 240) || !text(bin.kind, 240) || typeof bin.zone !== 'string' || Buffer.byteLength(bin.zone) > 240
      || !quantity(bin) || !small(bin.lot_positions, 0, 10000000) || previous !== null && Buffer.compare(Buffer.from(bin.code), Buffer.from(previous)) <= 0) fail();
    previous = bin.code; physical += BigInt(bin.on_hand_each); reserved += BigInt(bin.reserved_each);
  }
  if (physical > BigInt(value.stock.on_hand_each) || reserved > BigInt(value.stock.reserved_each)
    || bins.next_after !== null && (!bins.items.length || bins.next_after !== bins.items.at(-1).code)) fail();
  const plan = value.floor_plan;
  if (!plan || typeof plan.available !== 'boolean') fail();
  if (plan.available) {
    if (!text(plan.name) || !['mm', 'cm', 'm', 'grid'].includes(plan.unit) || !/^[0-9a-f]{64}$/.test(plan.revision)
      || !small(plan.width, 1, 1000000) || !small(plan.height, 1, 1000000) || !Array.isArray(plan.bins) || plan.bins.length > 1024
      || plan.mapped_bins !== plan.bins.length || plan.mapped_bins > bins.total || plan.total_bins !== bins.total) fail();
    const seen = new Set();
    let mappedPhysical = 0n; let mappedReserved = 0n;
    for (const [index, bin] of plan.bins.entries()) {
      if (!text(bin.code, 240) || seen.has(bin.code) || !quantity(bin)
        || !small(bin.x, 0, plan.width) || !small(bin.y, 0, plan.height)
        || !small(bin.width, 1, plan.width - bin.x) || !small(bin.height, 1, plan.height - bin.y)) fail();
      seen.add(bin.code);
      if (plan.bins.slice(0, index).some(other => bin.x < other.x + other.width && other.x < bin.x + bin.width && bin.y < other.y + other.height && other.y < bin.y + bin.height)) fail();
      const paged = bins.items.find(row => row.code === bin.code);
      if (paged && (paged.on_hand_each !== bin.on_hand_each || paged.reserved_each !== bin.reserved_each)) fail();
      mappedPhysical += BigInt(bin.on_hand_each); mappedReserved += BigInt(bin.reserved_each);
    }
    if (mappedPhysical > BigInt(value.stock.on_hand_each) || mappedReserved > BigInt(value.stock.reserved_each)) fail();
  } else if (!['not_configured', 'bin_sources_changed'].includes(plan.reason)) fail();
  return value;
}
export function validateProduct(value, scope, sku) {
  envelope(value, scope);
  if (value.sku !== sku || !text(value.sku, 240) || !text(value.name) || !/^[0-9a-f]{64}$/.test(value.image_scope) || typeof value.category !== 'string' || value.category.length > 1024
    || typeof value.serial_controlled !== 'boolean' || typeof value.expiry_required !== 'boolean' || !count(value.reorder_point_each)
    || !value.label || value.label.value !== sku || value.label.unit !== 'each'
    || !['code128', null].includes(value.label.symbology)
    || value.label.symbology === 'code128' && (!/^[\x20-\x7e]{1,80}$/.test(sku))) fail();
  if (value.image !== null) {
    const image = value.image;
    if (!image || value.image_status !== 'ready' || !/^[0-9a-f]{64}$/.test(image.sha256) || !small(image.width, 1, 2048) || !small(image.height, 1, 2048) || !small(image.bytes, 33, 4 * 1024 * 1024)) fail();
  } else if (!['not_configured', 'unavailable'].includes(value.image_status)) fail();
  return value;
}
export async function readConsole(client, scope, { after = null, review = null, limit = 12 } = {}) {
  if (!small(limit, 1, 64) || after !== null && (!text(after, 240) || !count(review)) || review !== null && !count(review)) fail();
  const query = new URLSearchParams({ limit: String(limit) });
  if (after !== null) query.set('after', after);
  if (review !== null) query.set('review', review);
  return validateConsole(await client.read(client.warehousePath(scope.warehouse, `/console?${query}`)), scope, { after, review, limit });
}
export async function readProduct(client, scope, sku) {
  if (!text(sku, 240)) throw new Error('Enter the exact product SKU');
  return validateProduct(await client.read(client.warehousePath(scope.warehouse, `/console/products/${encodeURIComponent(sku)}`)), scope, sku);
}
export async function readProductImage(client, scope, product) {
  validateProduct(product, scope, product.sku);
  if (!product.image) return null;
  const query = new URLSearchParams({ review: product.version, sha256: product.image.sha256, scope: product.image_scope });
  const bytes = await client.image(client.warehousePath(scope.warehouse, `/console/products/${encodeURIComponent(product.sku)}/image?${query}`));
  const size = pngInfo(bytes);
  if (bytes.length !== product.image.bytes || size.width !== product.image.width || size.height !== product.image.height
    || createHash('sha256').update(bytes).digest('hex') !== product.image.sha256) fail();
  return bytes;
}
