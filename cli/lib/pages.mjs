// Preserve opaque server cursors. Each batch is a fresh read, not a shared
// multi-page snapshot or authority for physical work.
export async function readTablePage(client, warehouse, kind, cursor = null) {
  if (!['lots', 'transfers'].includes(kind)) throw new Error('Unsupported paged table');
  const validCursor = value => kind === 'lots'
    ? typeof value === 'string' && value.length > 0 && value.length <= 2048
    : Number.isSafeInteger(value) && value > 0;
  if (cursor !== null && !validCursor(cursor)) throw new Error('Invalid original page cursor');
  const endpoint = client.warehousePath(warehouse, `/${kind}?limit=100${kind === 'transfers' ? '&open_only=false' : ''}${cursor !== null ? `&after=${encodeURIComponent(String(cursor))}` : ''}`);
  const page = await client.read(endpoint);
  if (!page || typeof page !== 'object' || page.warehouse_id !== warehouse || !Number.isSafeInteger(page.version) || page.version < 0 || !Array.isArray(page.items) || page.items.length > 100
    || kind === 'lots' && (typeof page.checked_at !== 'string' || !page.checked_at)
    || page.next_cursor !== null && !validCursor(page.next_cursor)) throw new Error('Invalid facility page response');
  const rows = kind === 'transfers' ? page.items.map(item => item.data) : page.items;
  if (rows.some(row => !row || typeof row !== 'object' || Array.isArray(row))) throw new Error('Invalid original page records');
  return {
    title: `${kind === 'lots' ? 'Lots and holds' : 'Facility transfers'} | ${warehouse} | snapshot v${page.version}`,
    rows, version: page.version, nextCursor: page.next_cursor, warehouseId: warehouse,
    note: kind === 'lots' ? `Availability checked at ${page.checked_at}. Refresh before acting; expiry can change with time.` : 'Includes open and settled transfers. Each batch is a fresh read.',
  };
}
