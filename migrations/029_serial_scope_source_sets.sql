-- Statement-local serial source sets. Group on byte-exact text including
-- place::text (JSON numbers 1 and 1.0 remain separate). MIN returns the identical
-- original strings with their original column collations. Hash aggregation
-- avoids sorting every wide unit row; no inventory row or stored result changes.
-- Ordinary places share shape checks and join unique mode/product/bin keys.
-- Other custody states retain the original reference validator. Keep the CASE
-- behavior of the original PL/pgSQL IF, including nullable malformed inputs.
-- Every stock, staging, package and ownership equation remains complete.
CREATE OR REPLACE FUNCTION wms_serial_scope_current_valid(t TEXT,w TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 WITH reference_classes AS MATERIALIZED (
  SELECT min(tenant) AS tenant,min(sku) AS sku,min(lot) AS lot,place,
   (place::text) COLLATE "C" AS place_key FROM wms_serial_units
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
 AND NOT EXISTS(SELECT 1 FROM reference_sources WHERE NOT ok);
$$;

-- Preserve the original caller's controlled-mode and optional SKU selection.
-- The source predicate is the original wms_serial_mode_original expression;
-- WHERE m.controlled proves to_jsonb(m.controlled) = 'true'::jsonb here.
-- Expose the complete selection to the planner without per-mode SQL calls.
-- The standalone original validator remains unchanged for independent proofs.
CREATE FUNCTION wms_serial_modes_original_for_scope(t TEXT,source_sku TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT NOT EXISTS(
  SELECT 1 FROM wms_serial_modes m WHERE m.tenant=t AND m.controlled AND(source_sku IS NULL OR m.sku=source_sku)
   AND NOT(COALESCE(NOT EXISTS(SELECT 1 FROM wms_products p WHERE p.tenant=m.tenant AND p.sku=m.sku
  AND COALESCE(p.data->'serial_controlled','false'::jsonb)<>'true'::jsonb)
 AND (NOT m.controlled OR EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c
  ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
  WHERE e.id=m.source_event_id AND e.tenant=m.tenant AND e.warehouse_id=m.source_warehouse_id
  AND e.payload->>'id'=e.id AND e.payload->>'tenant'=m.tenant AND e.payload->>'warehouse_id'=m.source_warehouse_id
  AND e.payload->'version'=to_jsonb(e.version) AND e.payload->>'actor'=c.actor
  AND e.payload->>'type'='product.created' AND e.payload->'result'=c.result
  AND c.result=jsonb_build_object('entity_id',m.sku,'version',e.version,'type','product.created')
  AND e.payload->'command'->>'type'='create_product' AND e.payload->'command'->>'sku'=m.sku
  AND e.payload->'command'->'serial_controlled'='true'::jsonb
  AND EXISTS(SELECT 1 FROM wms_products p WHERE p.tenant=m.tenant AND p.warehouse_id=m.source_warehouse_id AND p.sku=m.sku))),false))
 );
$$;

CREATE OR REPLACE FUNCTION wms_serial_scope_commit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE d JSONB; t TEXT; w TEXT; source_sku TEXT;
BEGIN
 IF TG_OP='DELETE' THEN d=to_jsonb(OLD); ELSE d=to_jsonb(NEW); END IF;
 t=d->>'tenant'; w=COALESCE(d->>'warehouse_id',d->>'source_warehouse_id',d->>'destination_warehouse_id'); source_sku=d->>'sku';
 IF source_sku IS NOT NULL AND NOT EXISTS(SELECT 1 FROM wms_serial_modes WHERE tenant=t AND sku=source_sku AND controlled) THEN RETURN NULL; END IF;
 IF NOT EXISTS(SELECT 1 FROM wms_serial_modes WHERE tenant=t AND controlled) THEN RETURN NULL; END IF;
 IF NOT wms_serial_modes_original_for_scope(t,source_sku)
  OR NOT wms_serial_scope_current_valid(t,w) OR NOT wms_serial_scope_transit_valid(t,w) THEN
  RAISE EXCEPTION 'Serialized stock, staging, package, transit or original loss ownership disagrees' USING ERRCODE='23514';
 END IF;
 IF TG_TABLE_NAME='wms_events' AND NOT wms_serial_event_valid(t,w,d->>'id') THEN
  RAISE EXCEPTION 'Original serial event lacks complete exact inputs, effects or source relationships' USING ERRCODE='23514';
 END IF;
 RETURN NULL;
END $$;
