-- Check serial custody per (warehouse, SKU) at commit instead of rescanning
-- every serial unit in the warehouse. Each custody equation (stock, staging,
-- packages, transit, loss) and each unit place reference involves exactly one
-- SKU, so the warehouse-wide predicate equals the conjunction of these
-- SKU-scoped predicates. Every mutation that can change one of them queues
-- its SKU: SKU-bearing rows directly, serial units for the warehouses of
-- their old and new places, package closures and receipts through the SKUs
-- that can reference them, and transfer losses through their original line.
-- Location changes keep the complete warehouse check. Event insertion no
-- longer queues a warehouse-wide custody scan; event source, mode and history
-- checks are unchanged.
--
-- Intended behavior change: custody damage confined to an unrelated SKU,
-- which only a guard bypass can create, no longer blocks commits for other
-- SKUs in the warehouse. Full reconciliation still reports it, and any
-- commit touching the damaged SKU is still rejected.
ALTER TABLE wms_serial_pending_checks DROP CONSTRAINT wms_serial_pending_checks_kind_check;
ALTER TABLE wms_serial_pending_checks ADD CONSTRAINT wms_serial_pending_checks_kind_check
 CHECK(kind IN('event','scope','sku_scope','mode','modes'));

-- Migration 030's predicate with every source restricted to one SKU.
CREATE FUNCTION wms_serial_scope_current_valid_sku(t TEXT,w TEXT,k_sku TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 WITH reference_classes AS MATERIALIZED (
  SELECT min(tenant) AS tenant,min(sku) AS sku,min(lot) AS lot,place,
   (place::text) COLLATE "C" AS place_key,count(*)::bigint AS quantity FROM wms_serial_units
  WHERE tenant=t AND sku=k_sku AND (place->>'warehouse_id'=w OR place->>'source_warehouse_id'=w OR place->>'destination_warehouse_id'=w)
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
  WHERE s.tenant=t AND s.warehouse_id=w AND s.sku=k_sku
 ), actual_stock AS (
  SELECT sku,lot,CASE WHEN place->>'status'='staged' THEN 'STAGE-01' ELSE place->>'location' END AS location,sum(quantity)::bigint AS quantity
  FROM reference_classes WHERE place->>'warehouse_id'=w AND place->>'status' IN('on_hand','staged')
  GROUP BY sku,lot,CASE WHEN place->>'status'='staged' THEN 'STAGE-01' ELSE place->>'location' END
 ), expected_staged AS (
  SELECT a.order_id,a.line_id,a.location,a.sku,a.lot,(a.picked-a.shipped)::bigint AS quantity FROM wms_allocations a
  JOIN wms_serial_modes m ON m.tenant=a.tenant AND m.sku=a.sku AND m.controlled WHERE a.tenant=t AND a.warehouse_id=w AND a.sku=k_sku
 ), actual_staged AS (
  SELECT place->>'order_id' AS order_id,place->>'line_id' AS line_id,place->>'allocation_location' AS location,sku,lot,sum(quantity)::bigint AS quantity
  FROM reference_classes WHERE place->>'warehouse_id'=w AND place->>'status'='staged'
  GROUP BY place->>'order_id',place->>'line_id',place->>'allocation_location',sku,lot
 ), expected_packages AS (
  SELECT line.package_id,line.line_id,line.location,line.sku,line.lot,line.quantity FROM wms_package_lines line
  JOIN wms_serial_modes m ON m.tenant=line.tenant AND m.sku=line.sku AND m.controlled WHERE line.tenant=t AND line.warehouse_id=w AND line.sku=k_sku
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

-- Migration 021's transit predicate restricted to one SKU.
CREATE FUNCTION wms_serial_scope_transit_valid_sku(t TEXT,w TEXT,k_sku TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT NOT EXISTS(SELECT 1 FROM wms_transfer_lines line JOIN wms_serial_modes mode ON mode.tenant=line.tenant AND mode.sku=line.sku AND mode.controlled
  WHERE line.tenant=t AND line.sku=k_sku AND (line.source_warehouse_id=w OR line.destination_warehouse_id=w)
  AND line.quantity - COALESCE((SELECT sum(m.quantity) FROM wms_movements m WHERE m.tenant=line.tenant AND m.warehouse_id=line.destination_warehouse_id
   AND m.transfer_id=line.transfer_id AND m.transfer_line_id=line.id AND m.kind='transfer_receive'),0)
  <> (SELECT count(*) FROM wms_serial_units unit WHERE unit.tenant=line.tenant AND unit.sku=line.sku AND unit.lot=line.lot
   AND unit.place->>'transfer_id'=line.transfer_id AND unit.place->>'line_id'=line.id AND unit.place->>'status' IN('in_transit','lost')))
 AND NOT EXISTS(SELECT 1 FROM wms_transfer_loss_records loss JOIN wms_transfer_lines line ON line.tenant=loss.tenant AND line.transfer_id=loss.transfer_id AND line.id=loss.line_id
  JOIN wms_serial_modes mode ON mode.tenant=line.tenant AND mode.sku=line.sku AND mode.controlled
  WHERE loss.tenant=t AND loss.kind='loss' AND line.sku=k_sku AND (line.source_warehouse_id=w OR line.destination_warehouse_id=w)
  AND loss.quantity-COALESCE((SELECT sum(reversal.quantity) FROM wms_transfer_loss_records reversal WHERE reversal.tenant=loss.tenant AND reversal.original_loss_id=loss.id AND reversal.kind='reversal'),0)
  <> (SELECT count(*) FROM wms_serial_units unit WHERE unit.tenant=line.tenant AND unit.sku=line.sku AND unit.lot=line.lot AND unit.place->>'status'='lost'
   AND unit.place->>'loss_id'=loss.id AND unit.place->>'transfer_id'=line.transfer_id AND unit.place->>'line_id'=line.id));
$$;

CREATE FUNCTION wms_serial_enqueue_sku_scope(t TEXT,w TEXT,k_sku TEXT) RETURNS VOID LANGUAGE SQL AS $$
 INSERT INTO wms_serial_pending_checks
  SELECT t,'sku_scope',jsonb_build_array(w,k_sku)::text WHERE w IS NOT NULL AND k_sku IS NOT NULL
 ON CONFLICT DO NOTHING;
$$;

CREATE OR REPLACE FUNCTION wms_serial_scope_commit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE d JSONB; t TEXT; w TEXT; source_sku TEXT; derived TEXT;
BEGIN
 IF TG_OP='DELETE' THEN d=to_jsonb(OLD); ELSE d=to_jsonb(NEW); END IF;
 t=d->>'tenant'; w=COALESCE(d->>'warehouse_id',d->>'source_warehouse_id',d->>'destination_warehouse_id'); source_sku=d->>'sku';
 IF source_sku IS NOT NULL AND NOT EXISTS(SELECT 1 FROM wms_serial_modes WHERE tenant=t AND sku=source_sku AND controlled) THEN RETURN NULL; END IF;
 IF NOT EXISTS(SELECT 1 FROM wms_serial_modes WHERE tenant=t AND controlled) THEN RETURN NULL; END IF;
 IF source_sku IS NOT NULL THEN
  PERFORM wms_serial_enqueue_sku_scope(t,w,source_sku);
  IF TG_OP='UPDATE' AND to_jsonb(OLD)->>'sku' IS DISTINCT FROM source_sku THEN
   PERFORM wms_serial_enqueue_sku_scope(t,COALESCE(to_jsonb(OLD)->>'warehouse_id',w),to_jsonb(OLD)->>'sku');
  END IF;
 ELSIF TG_TABLE_NAME='wms_package_closures' THEN
  FOR derived IN SELECT DISTINCT line.sku FROM wms_package_lines line
   WHERE line.tenant=t AND line.warehouse_id=w AND line.package_id=d->>'package_id' LOOP
   PERFORM wms_serial_enqueue_sku_scope(t,w,derived);
  END LOOP;
 ELSIF TG_TABLE_NAME='wms_receipts' THEN
  FOR derived IN SELECT DISTINCT r.sku FROM wms_return_lines r
   WHERE r.tenant=t AND r.warehouse_id=w AND r.receipt_id=d->>'id' LOOP
   PERFORM wms_serial_enqueue_sku_scope(t,w,derived);
  END LOOP;
 ELSIF TG_TABLE_NAME='wms_transfer_loss_records' THEN
  SELECT line.sku INTO derived FROM wms_transfer_lines line
   WHERE line.tenant=t AND line.transfer_id=d->>'transfer_id' AND line.id=d->>'line_id';
  IF FOUND THEN PERFORM wms_serial_enqueue_sku_scope(t,w,derived);
  ELSE INSERT INTO wms_serial_pending_checks VALUES(t,'scope',w) ON CONFLICT DO NOTHING; END IF;
 ELSIF TG_TABLE_NAME<>'wms_events' THEN
  INSERT INTO wms_serial_pending_checks VALUES(t,'scope',w) ON CONFLICT DO NOTHING;
 END IF;
 -- Mode-source selection is unchanged from migration 032.
 INSERT INTO wms_serial_pending_checks VALUES(t,CASE WHEN source_sku IS NULL THEN 'modes' ELSE 'mode' END,
  CASE WHEN source_sku IS NULL THEN w ELSE jsonb_build_array(w,source_sku)::text END) ON CONFLICT DO NOTHING;
 IF TG_TABLE_NAME='wms_events' THEN
  INSERT INTO wms_serial_pending_checks VALUES(t,'event',d->>'id') ON CONFLICT DO NOTHING;
 END IF;
 RETURN NULL;
END $$;

-- Serial units keep their event check and also queue custody for every
-- warehouse named by their previous and current places.
CREATE FUNCTION wms_serial_enqueue_unit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE place JSONB; w TEXT;
BEGIN
 INSERT INTO wms_serial_pending_checks VALUES(NEW.tenant,'event',NEW.last_event_id) ON CONFLICT DO NOTHING;
 FOREACH place IN ARRAY CASE WHEN TG_OP='UPDATE' THEN ARRAY[OLD.place,NEW.place] ELSE ARRAY[NEW.place] END LOOP
  FOREACH w IN ARRAY ARRAY[place->>'warehouse_id',place->>'source_warehouse_id',place->>'destination_warehouse_id'] LOOP
   PERFORM wms_serial_enqueue_sku_scope(NEW.tenant,w,NEW.sku);
  END LOOP;
 END LOOP;
 RETURN NULL;
END $$;
DROP TRIGGER wms_serial_unit_bound ON wms_serial_units;
CREATE TRIGGER wms_serial_unit_bound AFTER INSERT OR UPDATE ON wms_serial_units
 FOR EACH ROW EXECUTE FUNCTION wms_serial_enqueue_unit();

-- Unshipped order lines are mutable; shipped serial places reference them.
CREATE TRIGGER wms_serial_order_line_proof AFTER UPDATE OR DELETE ON wms_order_lines
 FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();

CREATE OR REPLACE FUNCTION wms_serial_validate_pending() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_warehouse TEXT; valid BOOLEAN;
BEGIN
 -- Removal never means acceptance. Any failed check aborts this transaction,
 -- restoring its dirty keys along with the original physical/source mutations.
 DELETE FROM wms_serial_pending_checks WHERE tenant=NEW.tenant AND kind=NEW.kind AND key=NEW.key;
 IF NEW.kind IN('mode','modes') THEN
  IF wms_serial_modes_original_for_scope(NEW.tenant,CASE WHEN NEW.kind='mode' THEN (NEW.key::jsonb)->>1 END) IS DISTINCT FROM true THEN
   RAISE EXCEPTION 'Original serial mode source disagrees' USING ERRCODE='23514';
  END IF;
 ELSIF NEW.kind='scope' THEN
  IF wms_serial_scope_current_valid(NEW.tenant,NEW.key) IS DISTINCT FROM true
   OR wms_serial_scope_transit_valid(NEW.tenant,NEW.key) IS DISTINCT FROM true THEN
   RAISE EXCEPTION 'Serialized stock, staging, package, transit or original loss ownership disagrees' USING ERRCODE='23514';
  END IF;
 ELSIF NEW.kind='sku_scope' THEN
  IF wms_serial_scope_current_valid_sku(NEW.tenant,(NEW.key::jsonb)->>0,(NEW.key::jsonb)->>1) IS DISTINCT FROM true
   OR wms_serial_scope_transit_valid_sku(NEW.tenant,(NEW.key::jsonb)->>0,(NEW.key::jsonb)->>1) IS DISTINCT FROM true THEN
   RAISE EXCEPTION 'Serialized stock, staging, package, transit or original loss ownership disagrees' USING ERRCODE='23514';
  END IF;
 ELSE
  SELECT warehouse_id INTO source_warehouse FROM wms_events WHERE tenant=NEW.tenant AND id=NEW.key;
  IF NOT FOUND OR wms_serial_event_valid_read_source(NEW.tenant,source_warehouse,NEW.key) IS DISTINCT FROM true THEN
   RAISE EXCEPTION 'Original serial source event lacks complete exact inputs, effects or source relationships' USING ERRCODE='23514';
  END IF;
  WITH affected AS MATERIALIZED (
   SELECT sku,serial FROM wms_serial_steps WHERE tenant=NEW.tenant AND event_id=NEW.key
   UNION SELECT sku,serial FROM wms_serial_units WHERE tenant=NEW.tenant AND last_event_id=NEW.key
  ), current_units AS MATERIALIZED (
   SELECT u.* FROM affected k LEFT JOIN wms_serial_units u ON u.tenant=NEW.tenant AND u.sku=k.sku AND u.serial=k.serial
  ), latest_events AS MATERIALIZED (
   SELECT e.id,e.warehouse_id,
    CASE WHEN e.id=NEW.key THEN true ELSE wms_serial_event_valid_read_source(e.tenant,e.warehouse_id,e.id) END AS ok
   FROM wms_events e JOIN (SELECT DISTINCT last_event_id FROM current_units) k ON k.last_event_id=e.id
   WHERE e.tenant=NEW.tenant
  ) SELECT NOT EXISTS(
   SELECT 1 FROM current_units u LEFT JOIN latest_events e ON e.id=u.last_event_id
   WHERE u.tenant IS NULL OR e.ok IS DISTINCT FROM true
    OR NOT EXISTS(SELECT 1 FROM wms_serial_steps s WHERE s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial
     AND s.lot=u.lot AND s.revision=u.revision AND s.event_id=u.last_event_id AND s.after_place=u.place AND s.warehouse_id=e.warehouse_id)
    OR EXISTS(SELECT 1 FROM wms_serial_steps later WHERE later.tenant=u.tenant AND later.sku=u.sku AND later.serial=u.serial AND later.revision>u.revision)
  ) INTO valid;
  IF valid IS DISTINCT FROM true THEN
   RAISE EXCEPTION 'Original serial source, revision chain or current projection disagrees' USING ERRCODE='23514';
  END IF;
 END IF;
 RETURN NULL;
END $$;
