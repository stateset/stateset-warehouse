-- Candidate identities include zero-balance history and the not-yet-arrived
-- destination of an actual transfer. A shared text label is not batch genealogy.
WITH owners AS (
 SELECT warehouse_id FROM wms_stock WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT warehouse_id FROM wms_allocations WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT warehouse_id FROM wms_receipt_lines WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT warehouse_id FROM wms_movements WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT source_warehouse_id FROM wms_transfer_lines WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT destination_warehouse_id FROM wms_transfer_lines WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT warehouse_id FROM wms_package_lines WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT warehouse_id FROM wms_lot_changes WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT warehouse_id FROM wms_lot_decisions WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT warehouse_id FROM wms_shipment_evidence WHERE tenant=$1
  AND document @> jsonb_build_object('allocations',jsonb_build_array(jsonb_build_object('sku',$2::text,'lot',$3::text)))
)
SELECT w.warehouse_id,w.version,w.metadata FROM wms_warehouses w
JOIN owners o ON o.warehouse_id=w.warehouse_id WHERE w.tenant=$1 ORDER BY w.warehouse_id COLLATE "C"
