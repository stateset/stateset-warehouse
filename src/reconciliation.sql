WITH movements AS (
 SELECT sku,lot,from_location AS location,-quantity AS delta FROM wms_movements WHERE tenant=$1 AND warehouse_id=$2 AND from_location IS NOT NULL
 UNION ALL
 SELECT sku,lot,to_location AS location,quantity AS delta FROM wms_movements WHERE tenant=$1 AND warehouse_id=$2 AND to_location IS NOT NULL
), ledger AS(SELECT sku,lot,location,sum(delta)::bigint AS quantity FROM movements GROUP BY sku,lot,location),
deltas AS(
 SELECT sku,lot,location,quantity-picked-released AS quantity FROM wms_allocations WHERE tenant=$1 AND warehouse_id=$2
 UNION ALL SELECT sku,lot,'STAGE-01',picked-shipped FROM wms_allocations WHERE tenant=$1 AND warehouse_id=$2
), allocations AS(SELECT sku,lot,location,sum(quantity)::bigint AS quantity FROM deltas GROUP BY sku,lot,location),
keys AS(SELECT sku,lot,location FROM wms_stock WHERE tenant=$1 AND warehouse_id=$2 UNION SELECT sku,lot,location FROM ledger UNION SELECT sku,lot,location FROM allocations)
SELECT k.sku,k.lot,k.location,COALESCE(s.on_hand,0)::bigint AS on_hand,COALESCE(s.reserved,0)::bigint AS reserved,
COALESCE(l.quantity,0)::bigint AS ledger,COALESCE(a.quantity,0)::bigint AS allocation_reserved
FROM keys k LEFT JOIN wms_stock s ON s.tenant=$1 AND s.warehouse_id=$2 AND s.sku=k.sku AND s.lot=k.lot AND s.location=k.location
LEFT JOIN ledger l ON l.sku=k.sku AND l.lot=k.lot AND l.location=k.location
LEFT JOIN allocations a ON a.sku=k.sku AND a.lot=k.lot AND a.location=k.location
ORDER BY k.sku,k.lot,k.location
