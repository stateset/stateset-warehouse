import { boundSession } from './journal.mjs';
import { shipmentReview } from './workflows.mjs';
import { orderProgress } from './fulfillment.mjs';

export async function readFulfillmentReview(client, scope, id, version) {
  await boundSession(client, scope);
  const snapshot = await client.read(client.warehousePath(scope.warehouse));
  if (snapshot.warehouse.id !== scope.warehouse || snapshot.version !== version) throw new Error('Fulfillment view is stale or belongs to another facility; refresh and select again');
  const order = snapshot.orders.find(row => row.id === id);
  if (!order) throw new Error('Original fulfillment order is missing; refresh and select again');
  const progress = orderProgress(order);
  if (progress.error) throw new Error(`Fulfillment review held: ${progress.error}`);
  const shipments = await shipmentReview(client, scope, id, version);
  return { order, shipments, version, progress };
}

export async function readOrderReview(client, warehouse, id) {
  const base = client.warehousePath(warehouse, `/orders/${encodeURIComponent(id)}`);
  const [record, shipments, eligibility] = await Promise.all([
    client.read(base), client.read(`${base}/shipments`), client.call('GET', `${base}/return-eligibility`),
  ]);
  if (!eligibility.ok && eligibility.status !== 409) throw new Error(`HTTP ${eligibility.status}: ${eligibility.data?.error ?? eligibility.data?.message ?? 'Return eligibility read failed'}`);
  return {
    record, shipments,
    returnEligibility: eligibility.ok ? eligibility.data : { unavailable: true, http_status: eligibility.status, response: eligibility.data },
  };
}
