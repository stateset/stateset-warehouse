-- Reuse the complete statement-local reference population for custody counts.
-- Each byte-exact metadata class retains its full multiplicity. Stock, staging
-- and package equations sum those counts under their original filters; no
-- projection, persisted verdict, source check or lock is removed.
CREATE OR REPLACE FUNCTION wms_serial_scope_current_valid(t TEXT,w TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 WITH reference_classes AS MATERIALIZED (
  SELECT min(tenant) AS tenant,min(sku) AS sku,min(lot) AS lot,place,
   (place::text) COLLATE "C" AS place_key,count(*)::bigint AS quantity FROM wms_serial_units
  WHERE tenant=t AND (place->>'warehouse_id'=w OR place->>'source_warehouse_id'=w OR place->>'destination_warehouse_id'=w)
  GROUP BY tenant COLLATE "C",sku COLLATE "C",lot COLLATE "C",place,(place::text) COLLATE "C"
 ), reference_places AS MATERIALIZED (
  SELECT DISTINCT ON(place_key) place_key,place FROM reference_classes
  WHERE place->>'status' IN('on_hand','missing')
 ), reference_shapes AS MATERIALIZED (
  SELECT place_key,wms_serial_place_valid(place) AS shape FROM reference_places
 ), reference_sources AS MATERIALIZED (
  SELECT u.tenant,u.sku,u.lot,u.place,u.place_key,
   CASE WHEN u.place->>'status' IN('on_hand','missing') THEN
    CASE WHEN NOT shape.shape OR mode.sku IS NULL THEN false ELSE bin.code IS NOT NULL END
   ELSE wms_serial_place_references_valid(u.tenant,u.sku,u.lot,u.place) END AS ok
  FROM reference_classes u LEFT JOIN reference_shapes shape ON shape.place_key=u.place_key
  LEFT JOIN wms_serial_modes mode ON mode.tenant=u.tenant AND mode.sku=u.sku AND mode.controlled
  LEFT JOIN wms_products product ON product.tenant=u.tenant AND product.sku=u.sku
   AND product.warehouse_id=u.place->>'warehouse_id' AND product.data->'serial_controlled'='true'::jsonb
  LEFT JOIN wms_locations bin ON bin.tenant=product.tenant AND bin.warehouse_id=product.warehouse_id
   AND bin.code=u.place->>'location' AND bin.kind IN('storage','receiving')
 ), expected_stock AS (
  SELECT s.sku,s.lot,s.location,s.on_hand AS quantity FROM wms_stock s JOIN wms_serial_modes m ON m.tenant=s.tenant AND m.sku=s.sku AND m.controlled
  WHERE s.tenant=t AND s.warehouse_id=w
 ), actual_stock AS (
  SELECT sku,lot,CASE WHEN place->>'status'='staged' THEN 'STAGE-01' ELSE place->>'location' END AS location,sum(quantity)::bigint AS quantity
  FROM reference_classes WHERE place->>'warehouse_id'=w AND place->>'status' IN('on_hand','staged')
  GROUP BY sku,lot,CASE WHEN place->>'status'='staged' THEN 'STAGE-01' ELSE place->>'location' END
 ), expected_staged AS (
  SELECT a.order_id,a.line_id,a.location,a.sku,a.lot,(a.picked-a.shipped)::bigint AS quantity FROM wms_allocations a
  JOIN wms_serial_modes m ON m.tenant=a.tenant AND m.sku=a.sku AND m.controlled WHERE a.tenant=t AND a.warehouse_id=w
 ), actual_staged AS (
  SELECT place->>'order_id' AS order_id,place->>'line_id' AS line_id,place->>'allocation_location' AS location,sku,lot,sum(quantity)::bigint AS quantity
  FROM reference_classes WHERE place->>'warehouse_id'=w AND place->>'status'='staged'
  GROUP BY place->>'order_id',place->>'line_id',place->>'allocation_location',sku,lot
 ), expected_packages AS (
  SELECT line.package_id,line.line_id,line.location,line.sku,line.lot,line.quantity FROM wms_package_lines line
  JOIN wms_serial_modes m ON m.tenant=line.tenant AND m.sku=line.sku AND m.controlled WHERE line.tenant=t AND line.warehouse_id=w
  AND NOT EXISTS(SELECT 1 FROM wms_package_closures c WHERE c.tenant=line.tenant AND c.warehouse_id=line.warehouse_id AND c.package_id=line.package_id)
 ), actual_packages AS (
  SELECT place->>'package_id' AS package_id,place->>'line_id' AS line_id,place->>'allocation_location' AS location,sku,lot,sum(quantity)::bigint AS quantity
  FROM reference_classes WHERE place->>'warehouse_id'=w AND place->>'status'='staged' AND place->>'package_id' IS NOT NULL
  GROUP BY place->>'package_id',place->>'line_id',place->>'allocation_location',sku,lot
 )
 SELECT NOT EXISTS(SELECT 1 FROM expected_stock e FULL JOIN actual_stock a USING(sku,lot,location) WHERE COALESCE(e.quantity,0)<>COALESCE(a.quantity,0))
 AND NOT EXISTS(SELECT 1 FROM expected_staged e FULL JOIN actual_staged a USING(order_id,line_id,location,sku,lot) WHERE COALESCE(e.quantity,0)<>COALESCE(a.quantity,0))
 AND NOT EXISTS(SELECT 1 FROM expected_packages e FULL JOIN actual_packages a USING(package_id,line_id,location,sku,lot) WHERE COALESCE(e.quantity,0)<>COALESCE(a.quantity,0))
 AND NOT EXISTS(SELECT 1 FROM reference_sources WHERE NOT ok);
$$;
