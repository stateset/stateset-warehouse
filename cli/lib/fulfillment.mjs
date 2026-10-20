const whole = value => Number.isSafeInteger(value) && value >= 0;
const fields = ['ordered', 'allocated', 'to_pick', 'staged', 'shipped', 'cancelled', 'unallocated', 'remaining'];

// Read-only arithmetic over original line/allocation facts. Staged units can
// already belong to packages; these figures never grant packing or pick rights.
export function orderProgress(order) {
  if (!Array.isArray(order.lines) || !order.lines.length || !Array.isArray(order.allocations)) return { error: 'Complete original lines and allocations are required' };
  const lines = new Map(); const tuples = new Set();
  for (const line of order.lines) {
    if (typeof line.id !== 'string' || !line.id || typeof line.sku !== 'string' || !line.sku || !whole(line.quantity) || !line.quantity || lines.has(line.id)) return { error: 'Original order line identities or quantities are invalid' };
    lines.set(line.id, { id: line.id, sku: line.sku, ordered: BigInt(line.quantity), allocated: 0n, to_pick: 0n, staged: 0n, shipped: 0n, cancelled: 0n });
  }
  for (const allocation of order.allocations) {
    const line = lines.get(allocation.line_id); const tuple = JSON.stringify([allocation.line_id, allocation.location, allocation.lot]);
    if (!line || allocation.sku !== line.sku || !['quantity', 'picked', 'shipped', 'released'].every(key => whole(allocation[key])) || !['location', 'lot'].every(key => typeof allocation[key] === 'string' && allocation[key]) || tuples.has(tuple)) return { error: 'Original allocation identity or quantity is invalid' };
    tuples.add(tuple);
    const q = BigInt(allocation.quantity), picked = BigInt(allocation.picked), shipped = BigInt(allocation.shipped), released = BigInt(allocation.released);
    if (shipped > picked || picked + released > q) return { error: 'Original allocation counters disagree' };
    line.allocated += q - released; line.to_pick += q - picked - released; line.staged += picked - shipped; line.shipped += shipped;
  }
  const totals = Object.fromEntries(fields.map(key => [key, 0n]));
  for (const line of lines.values()) {
    if (order.status === 'cancelled') line.cancelled = line.ordered;
    if (order.status === 'closed_partial') {
      const original = order.remainder_cancellation?.lines?.[line.id];
      if (!original || original.sku !== line.sku || !['ordered', 'shipped', 'cancelled'].every(key => whole(original[key])) || BigInt(original.ordered) !== line.ordered || BigInt(original.shipped) !== line.shipped || BigInt(original.cancelled) + line.shipped !== line.ordered) return { error: 'Original cancelled remainder disagrees with its exact line demand' };
      line.cancelled = BigInt(original.cancelled);
    }
    if (line.allocated + line.cancelled > line.ordered) return { error: 'Original allocation exceeds its exact active line demand' };
    if (['shipped', 'cancelled', 'closed_partial'].includes(order.status) && line.shipped + line.cancelled !== line.ordered) return { error: 'Terminal order retains unaccounted original demand' };
    line.unallocated = line.ordered - line.allocated - line.cancelled; line.remaining = line.ordered - line.shipped - line.cancelled;
    for (const key of fields) totals[key] += line[key];
  }
  const encode = row => Object.fromEntries(Object.entries(row).map(([key, value]) => [key, typeof value === 'bigint' ? value.toString() : value]));
  return { totals: encode(totals), lines: [...lines.values()].map(encode) };
}

export function remainingOrderLines(order, type) {
  const progress = orderProgress(order);
  if (progress.error || !['pick', 'unpick'].includes(type)) return [];
  const remaining = new Set(progress.lines.filter(line => BigInt(line[type === 'pick' ? 'to_pick' : 'staged']) > 0n).map(line => line.id));
  return order.lines.filter(line => remaining.has(line.id));
}

export function fulfillmentSummary(scope, review) {
  const { order, shipments, version } = review; const progress = orderProgress(order);
  if (progress.error) throw new Error(`Fulfillment review held: ${progress.error}`);
  const total = progress.totals; const open = shipments.packages.filter(pack => pack.closure === null);
  return [
    `Facility: ${scope.warehouse} | reviewed v${version}`,
    `Original order: ${order.reference} | ${order.id}`,
    `Status: ${order.status} | Mode: ${order.shipment_mode ?? 'whole order'}`,
    `Ordered ${total.ordered} each | Handed off ${total.shipped} each`,
    `Cancelled original demand: ${total.cancelled} each`,
    `Remaining active demand: ${total.remaining} each`,
    `Unallocated: ${total.unallocated} each | To pick: ${total.to_pick} each`,
    `Picked / not handed off: ${total.staged} each`,
    `Open original packages: ${open.length}`,
    'Picked units may already be packed. Review original packages before packing or returning staged stock.',
    ...progress.lines.flatMap(line => ['', `Original line: ${line.id} | ${line.sku}`, `Ordered ${line.ordered} | Handed off ${line.shipped} | Cancelled ${line.cancelled} | Remaining ${line.remaining}`, `Unallocated ${line.unallocated} | To pick ${line.to_pick} | Picked / unshipped ${line.staged}`]),
    ...open.flatMap(pack => ['', `Open package: ${pack.reference} | ${pack.id}`, ...pack.lines.map(line => `${line.line_id} | ${line.location} | ${line.lot} | ${line.quantity} each`)]),
    '', 'Read-only review. Availability, restrictions and actual work require fresh server checks and explicit observations.',
  ].join('\n');
}
