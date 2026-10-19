SELECT s.line_id,s.sku,s.lot,s.quantity AS shipped,
 COALESCE((SELECT sum(r.quantity-COALESCE(c.cancelled,0))
 FROM wms_return_lines r LEFT JOIN wms_receipt_cancelled_lines c
 ON c.tenant=r.tenant AND c.warehouse_id=r.warehouse_id AND c.receipt_id=r.receipt_id AND c.line_id=r.line_id
 WHERE r.tenant=$1 AND r.warehouse_id=$2 AND r.order_id=$3 AND r.order_line_id=s.line_id AND r.lot=s.lot),0)::bigint AS authorized
FROM wms_actual_shipped($1,$2,$3) s ORDER BY s.line_id,s.lot
