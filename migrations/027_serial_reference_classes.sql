-- Keep the complete warehouse balance and ownership equations. The reference
-- predicate depends only on tenant, SKU, lot and place in the same snapshot;
-- identical arguments need one check. Preserve byte-exact argument classes,
-- including numeric JSON text distinctions, and the original selected values.
-- MATERIALIZED prevents predicate pushdown from restoring per-unit calls.
-- No validity result survives this statement, and no inventory row is changed.
CREATE OR REPLACE FUNCTION wms_serial_scope_current_valid(t TEXT,w TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 WITH reference_classes AS MATERIALIZED (
  SELECT DISTINCT ON (tenant COLLATE "C",sku COLLATE "C",lot COLLATE "C",(place::text) COLLATE "C")
   tenant,sku,lot,place FROM wms_serial_units
  WHERE tenant=t AND (place->>'warehouse_id'=w OR place->>'source_warehouse_id'=w OR place->>'destination_warehouse_id'=w)
 ), expected_stock AS (
  SELECT s.sku,s.lot,s.location,s.on_hand AS quantity FROM wms_stock s JOIN wms_serial_modes m ON m.tenant=s.tenant AND m.sku=s.sku AND m.controlled
  WHERE s.tenant=t AND s.warehouse_id=w
 ), actual_stock AS (
  SELECT sku,lot,CASE WHEN place->>'status'='staged' THEN 'STAGE-01' ELSE place->>'location' END AS location,count(*)::bigint AS quantity
  FROM wms_serial_units WHERE tenant=t AND place->>'warehouse_id'=w AND place->>'status' IN('on_hand','staged')
  GROUP BY sku,lot,CASE WHEN place->>'status'='staged' THEN 'STAGE-01' ELSE place->>'location' END
 ), expected_staged AS (
  SELECT a.order_id,a.line_id,a.location,a.sku,a.lot,(a.picked-a.shipped)::bigint AS quantity FROM wms_allocations a
  JOIN wms_serial_modes m ON m.tenant=a.tenant AND m.sku=a.sku AND m.controlled WHERE a.tenant=t AND a.warehouse_id=w
 ), actual_staged AS (
  SELECT place->>'order_id' AS order_id,place->>'line_id' AS line_id,place->>'allocation_location' AS location,sku,lot,count(*)::bigint AS quantity
  FROM wms_serial_units WHERE tenant=t AND place->>'warehouse_id'=w AND place->>'status'='staged'
  GROUP BY place->>'order_id',place->>'line_id',place->>'allocation_location',sku,lot
 ), expected_packages AS (
  SELECT line.package_id,line.line_id,line.location,line.sku,line.lot,line.quantity FROM wms_package_lines line
  JOIN wms_serial_modes m ON m.tenant=line.tenant AND m.sku=line.sku AND m.controlled WHERE line.tenant=t AND line.warehouse_id=w
  AND NOT EXISTS(SELECT 1 FROM wms_package_closures c WHERE c.tenant=line.tenant AND c.warehouse_id=line.warehouse_id AND c.package_id=line.package_id)
 ), actual_packages AS (
  SELECT place->>'package_id' AS package_id,place->>'line_id' AS line_id,place->>'allocation_location' AS location,sku,lot,count(*)::bigint AS quantity
  FROM wms_serial_units WHERE tenant=t AND place->>'warehouse_id'=w AND place->>'status'='staged' AND place->>'package_id' IS NOT NULL
  GROUP BY place->>'package_id',place->>'line_id',place->>'allocation_location',sku,lot
 )
 SELECT NOT EXISTS(SELECT 1 FROM expected_stock e FULL JOIN actual_stock a USING(sku,lot,location) WHERE COALESCE(e.quantity,0)<>COALESCE(a.quantity,0))
 AND NOT EXISTS(SELECT 1 FROM expected_staged e FULL JOIN actual_staged a USING(order_id,line_id,location,sku,lot) WHERE COALESCE(e.quantity,0)<>COALESCE(a.quantity,0))
 AND NOT EXISTS(SELECT 1 FROM expected_packages e FULL JOIN actual_packages a USING(package_id,line_id,location,sku,lot) WHERE COALESCE(e.quantity,0)<>COALESCE(a.quantity,0))
 AND NOT EXISTS(SELECT 1 FROM reference_classes u
  WHERE NOT wms_serial_place_references_valid(u.tenant,u.sku,u.lot,u.place));
$$;
