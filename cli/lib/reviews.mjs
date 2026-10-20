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
