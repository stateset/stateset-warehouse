-- Recompute terminal state and frozen remainder identity for all receipts,
-- independently of cached receipt JSON and the operational history window.
SELECT jsonb_build_object('receipt_id',r.id,'status',r.status,
 'cancelled',COALESCE(sum(c.cancelled),0),
 'ok', (CASE WHEN h.receipt_id IS NOT NULL THEN
   r.status='cancelled' AND count(l.id)>0 AND count(l.id)=count(c.line_id)
   AND COALESCE(sum(c.cancelled),0)>0
   AND COALESCE(bool_and((c.sku,c.lot,c.quantity,c.received,c.cancelled)
      IS NOT DISTINCT FROM (l.sku,l.lot,l.quantity,l.received,l.quantity-l.received)),false)
 ELSE count(l.id)>0 AND r.status=CASE WHEN bool_and(l.received=0) THEN 'expected'
   WHEN bool_and(l.received=l.quantity) THEN 'received' ELSE 'partial' END END)
 AND (NOT EXISTS(SELECT 1 FROM wms_return_lines o WHERE o.tenant=$1 AND o.warehouse_id=$2 AND o.receipt_id=r.id)
   OR (count(o.line_id)=count(l.id) AND COALESCE(bool_and((o.sku,o.lot,o.quantity)
     IS NOT DISTINCT FROM (l.sku,l.lot,l.quantity)),false)))) AS proof
FROM wms_receipts r
LEFT JOIN wms_receipt_lines l ON l.tenant=r.tenant AND l.warehouse_id=r.warehouse_id AND l.receipt_id=r.id
LEFT JOIN wms_receipt_cancellations h ON h.tenant=r.tenant AND h.warehouse_id=r.warehouse_id AND h.receipt_id=r.id
LEFT JOIN wms_receipt_cancelled_lines c ON c.tenant=l.tenant AND c.warehouse_id=l.warehouse_id AND c.receipt_id=l.receipt_id AND c.line_id=l.id
LEFT JOIN wms_return_lines o ON o.tenant=l.tenant AND o.warehouse_id=l.warehouse_id AND o.receipt_id=l.receipt_id AND o.line_id=l.id
WHERE r.tenant=$1 AND r.warehouse_id=$2
GROUP BY r.id,r.status,h.receipt_id ORDER BY r.id
