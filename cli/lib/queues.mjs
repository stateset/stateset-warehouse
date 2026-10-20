const terminalOrders = new Set(['shipped', 'cancelled', 'closed_partial']);
const openOrder = row => !terminalOrders.has(row.status);
const compare = (left, right) => left < right ? -1 : left > right ? 1 : 0;
const text = (left, right, field) => compare(String(left[field] ?? ''), String(right[field] ?? ''));
const stockIdentity = (left, right) => text(left, right, 'sku') || text(left, right, 'location') || text(left, right, 'lot');
const priority = row => ({ urgent: 0, high: 1, normal: 2 }[row.priority] ?? 3);
const filter = (id, label, match) => ({ id, label, match });
const sort = (id, label, compareRows) => ({ id, label, compareRows });

export function filtersFor(kind, snapshot = {}) {
  const receiving = new Set((snapshot.locations ?? []).filter(row => row.kind === 'receiving').map(row => row.code));
  return [filter('all', 'All records', () => true), ...({
    stocks: [
      filter('on_hand', 'Positive on-hand stock', row => row.on_hand > 0),
      filter('reserved', 'Stock with reservations', row => row.reserved > 0),
      filter('zero', 'Zero on-hand stock', row => row.on_hand === 0),
      filter('receiving', 'Stock in receiving bins', row => row.on_hand > 0 && receiving.has(row.location)),
    ],
    orders: [
      filter('open', 'Open orders', openOrder),
      filter('pending', 'Pending allocation', row => row.status === 'pending'),
      filter('picking', 'Allocated / picking', row => ['allocated', 'picking'].includes(row.status)),
      filter('picked', 'Picked orders', row => row.status === 'picked'),
      filter('packed', 'Packed orders', row => row.status === 'packed'),
      filter('attention', 'High / urgent open orders', row => openOrder(row) && ['high', 'urgent'].includes(row.priority)),
      filter('shipped', 'Shipped orders', row => row.status === 'shipped'),
      filter('closed', 'Cancelled / closed partial', row => ['cancelled', 'closed_partial'].includes(row.status)),
      filter('partial', 'Open partial / package fulfillment', row => openOrder(row) && (row.shipment_mode === 'packages' || row.status === 'partially_shipped')),
    ],
    receipts: [
      filter('incoming', 'Incoming receipts', row => !['received', 'cancelled'].includes(row.status)),
      filter('received', 'Received receipts', row => row.status === 'received'),
      filter('cancelled', 'Cancelled receipts', row => row.status === 'cancelled'),
    ],
    exceptions: [filter('open', 'Open exceptions', row => row.status === 'open'), filter('resolved', 'Resolved exceptions', row => row.status === 'resolved')],
    products: [filter('serial', 'Serial-controlled products', row => row.serial_controlled === true), filter('expiry', 'Products requiring expiry', row => row.expiry_required === true)],
    locations: [filter('receiving', 'Receiving bins', row => row.kind === 'receiving'), filter('storage', 'Storage bins', row => row.kind === 'storage')],
    lots: [
      filter('available', 'Server availability: available', row => row.availability === 'available'),
      filter('quarantined', 'Quarantined lots', row => row.availability === 'quarantined'),
      filter('expiry', 'Expired / missing expiry', row => ['expired', 'expiry_missing'].includes(row.availability)),
      filter('physical', 'Positive physical stock', row => row.physical > 0),
    ],
    transfers: [
      filter('inbound', 'Inbound to this facility', row => row.destination_warehouse_id === snapshot.warehouseId),
      filter('outbound', 'Outbound from this facility', row => row.source_warehouse_id === snapshot.warehouseId),
      filter('held', 'Source check held', row => row.ok !== true),
    ],
    shipments: [filter('open', 'Packed / awaiting handoff', row => row.closure === null), filter('closed', 'Handed off / unpacked', row => row.closure !== null)],
  }[kind] ?? [])];
}
export function sortsFor(kind) {
  return [sort('source', 'Original snapshot order', null), ...({
    stocks: [sort('identity', 'SKU / bin / lot', stockIdentity), sort('on_hand', 'On hand: highest first', (left, right) => compare(right.on_hand, left.on_hand) || stockIdentity(left, right))],
    orders: [sort('priority', 'Priority: urgent first', (left, right) => compare(priority(left), priority(right)) || text(left, right, 'reference')), sort('reference', 'Order reference', (left, right) => text(left, right, 'reference'))],
    receipts: [sort('reference', 'Receipt reference', (left, right) => text(left, right, 'reference')), sort('supplier', 'Supplier / reference', (left, right) => text(left, right, 'supplier') || text(left, right, 'reference'))],
    products: [sort('sku', 'SKU', (left, right) => text(left, right, 'sku')), sort('name', 'Product name', (left, right) => text(left, right, 'name'))],
    locations: [sort('code', 'Bin code', (left, right) => text(left, right, 'code')), sort('zone', 'Zone / bin', (left, right) => text(left, right, 'zone') || text(left, right, 'code'))],
    exceptions: [sort('category', 'Category', (left, right) => text(left, right, 'category'))],
    lots: [sort('identity', 'SKU / lot', (left, right) => text(left, right, 'sku') || text(left, right, 'lot')), sort('physical', 'Physical: highest first', (left, right) => compare(right.physical, left.physical) || text(left, right, 'sku') || text(left, right, 'lot'))],
    transfers: [sort('reference', 'Transfer reference', (left, right) => text(left, right, 'reference')), sort('dispatched', 'Dispatched: newest first', (left, right) => text(right, left, 'dispatched_at'))],
    shipments: [sort('reference', 'Package reference', (left, right) => text(left, right, 'reference'))],
  }[kind] ?? [])];
}
export function tableView(kind, snapshot, { filter: filterId = 'all', sort: sortId = 'source', query = '' } = {}) {
  const selectedFilter = filtersFor(kind, snapshot).find(filter => filter.id === filterId);
  const selectedSort = sortsFor(kind).find(sort => sort.id === sortId);
  if (!selectedFilter || !selectedSort) throw new Error('Unknown table filter or sort');
  let rows = snapshot.rows.filter(row => selectedFilter.match(row) && (!query || JSON.stringify(row).toLowerCase().includes(query.toLowerCase())));
  if (selectedSort.compareRows) rows.sort(selectedSort.compareRows);
  return { rows, filter: selectedFilter, sort: selectedSort };
}

const queues = [
  { label: 'Open orders', kind: 'orders', filter: 'open', sort: 'priority' },
  { label: 'Pending allocation', kind: 'orders', filter: 'pending', sort: 'priority' },
  { label: 'Allocated / picking', kind: 'orders', filter: 'picking', sort: 'priority' },
  { label: 'Picked orders', kind: 'orders', filter: 'picked', sort: 'priority' },
  { label: 'Packed orders', kind: 'orders', filter: 'packed', sort: 'priority' },
  { label: 'High / urgent open orders', kind: 'orders', filter: 'attention', sort: 'priority' },
  { label: 'Incoming receipts', kind: 'receipts', filter: 'incoming', sort: 'reference' },
  { label: 'Stock in receiving bins', kind: 'stocks', filter: 'receiving', sort: 'identity' },
  { label: 'Open exceptions', kind: 'exceptions', filter: 'open', sort: 'category' },
  { label: 'Open partial / package fulfillment', kind: 'orders', filter: 'partial', sort: 'priority' },
];
export function workQueues(snapshot) {
  return queues.map(queue => {
    let count = null;
    if (!snapshot.error && Array.isArray(snapshot[queue.kind]) && (queue.filter !== 'receiving' || Array.isArray(snapshot.locations))) {
      const selected = filtersFor(queue.kind, snapshot).find(filter => filter.id === queue.filter);
      count = snapshot[queue.kind].filter(selected.match).length;
    }
    return { ...queue, count };
  });
}
