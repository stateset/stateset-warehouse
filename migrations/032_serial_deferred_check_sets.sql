-- Dirty keys, never saved validity. Every queued key is checked against the
-- complete final transaction snapshot and removed by its deferred constraint.
-- Repeated mutations share a check; mutations after SET CONSTRAINTS enqueue
-- again. Ordinary DML cannot insert, alter, remove or truncate these keys.
CREATE TABLE wms_serial_pending_checks (
 tenant TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN('event','scope','mode','modes')),
 key TEXT NOT NULL,
 PRIMARY KEY(tenant,kind,key)
);

CREATE FUNCTION wms_serial_pending_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP IN('UPDATE','TRUNCATE') OR pg_trigger_depth()<2 THEN
  RAISE EXCEPTION 'Serial pending checks are owned by custody constraints' USING ERRCODE='23514';
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_serial_pending_guard BEFORE INSERT OR UPDATE OR DELETE ON wms_serial_pending_checks
 FOR EACH ROW EXECUTE FUNCTION wms_serial_pending_guard();
CREATE TRIGGER wms_serial_pending_no_truncate BEFORE TRUNCATE ON wms_serial_pending_checks
 FOR EACH STATEMENT EXECUTE FUNCTION wms_serial_pending_guard();

CREATE FUNCTION wms_serial_enqueue_fact() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_event TEXT;
BEGIN
 IF TG_TABLE_NAME='wms_serial_units' THEN source_event=NEW.last_event_id;
 ELSE source_event=NEW.event_id; END IF;
 INSERT INTO wms_serial_pending_checks VALUES(NEW.tenant,'event',source_event) ON CONFLICT DO NOTHING;
 RETURN NULL;
END $$;

-- Projection, step and observation sources share complete event validation.
-- Mode/head/batch constraints retain their standalone original predicates.
DROP TRIGGER wms_serial_unit_bound ON wms_serial_units;
CREATE TRIGGER wms_serial_unit_bound AFTER INSERT OR UPDATE ON wms_serial_units
 FOR EACH ROW EXECUTE FUNCTION wms_serial_enqueue_fact();
DROP TRIGGER wms_serial_step_bound ON wms_serial_steps;
CREATE TRIGGER wms_serial_step_bound AFTER INSERT ON wms_serial_steps
 FOR EACH ROW EXECUTE FUNCTION wms_serial_enqueue_fact();
DROP TRIGGER wms_serial_observation_bound ON wms_serial_observations;
CREATE TRIGGER wms_serial_observation_bound AFTER INSERT ON wms_serial_observations
 FOR EACH ROW EXECUTE FUNCTION wms_serial_enqueue_fact();

CREATE OR REPLACE FUNCTION wms_serial_scope_commit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE d JSONB; t TEXT; w TEXT; source_sku TEXT;
BEGIN
 IF TG_OP='DELETE' THEN d=to_jsonb(OLD); ELSE d=to_jsonb(NEW); END IF;
 t=d->>'tenant'; w=COALESCE(d->>'warehouse_id',d->>'source_warehouse_id',d->>'destination_warehouse_id'); source_sku=d->>'sku';
 IF source_sku IS NOT NULL AND NOT EXISTS(SELECT 1 FROM wms_serial_modes WHERE tenant=t AND sku=source_sku AND controlled) THEN RETURN NULL; END IF;
 IF NOT EXISTS(SELECT 1 FROM wms_serial_modes WHERE tenant=t AND controlled) THEN RETURN NULL; END IF;
 INSERT INTO wms_serial_pending_checks VALUES(t,'scope',w) ON CONFLICT DO NOTHING;
 -- Preserve the original mode-source selection: SKU-bearing raw mutations
 -- check that SKU; event and other warehouse-wide mutations check all modes.
 -- Include the physical warehouse in mode-check keys so independent
 -- facilities do not acquire a new tenant-wide pending-key write lock.
 INSERT INTO wms_serial_pending_checks VALUES(t,CASE WHEN source_sku IS NULL THEN 'modes' ELSE 'mode' END,
  CASE WHEN source_sku IS NULL THEN w ELSE jsonb_build_array(w,source_sku)::text END) ON CONFLICT DO NOTHING;
 IF TG_TABLE_NAME='wms_events' THEN
  INSERT INTO wms_serial_pending_checks VALUES(t,'event',d->>'id') ON CONFLICT DO NOTHING;
 END IF;
 RETURN NULL;
END $$;

DROP TRIGGER wms_serial_event_proof ON wms_events;
CREATE TRIGGER wms_serial_event_proof AFTER INSERT ON wms_events FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
DROP TRIGGER wms_serial_stock_proof ON wms_stock;
CREATE TRIGGER wms_serial_stock_proof AFTER INSERT OR UPDATE OR DELETE ON wms_stock FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
DROP TRIGGER wms_serial_allocation_proof ON wms_allocations;
CREATE TRIGGER wms_serial_allocation_proof AFTER INSERT OR UPDATE OR DELETE ON wms_allocations FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
DROP TRIGGER wms_serial_package_line_proof ON wms_package_lines;
CREATE TRIGGER wms_serial_package_line_proof AFTER INSERT OR UPDATE OR DELETE ON wms_package_lines FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
DROP TRIGGER wms_serial_package_close_proof ON wms_package_closures;
CREATE TRIGGER wms_serial_package_close_proof AFTER INSERT ON wms_package_closures FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
DROP TRIGGER wms_serial_receipt_proof ON wms_receipts;
CREATE TRIGGER wms_serial_receipt_proof AFTER UPDATE OR DELETE ON wms_receipts FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
DROP TRIGGER wms_serial_catalog_proof ON wms_products;
CREATE TRIGGER wms_serial_catalog_proof AFTER INSERT OR UPDATE OR DELETE ON wms_products FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
DROP TRIGGER wms_serial_location_proof ON wms_locations;
CREATE TRIGGER wms_serial_location_proof AFTER UPDATE OR DELETE ON wms_locations FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
DROP TRIGGER wms_serial_transfer_line_proof ON wms_transfer_lines;
CREATE TRIGGER wms_serial_transfer_line_proof AFTER INSERT ON wms_transfer_lines FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
DROP TRIGGER wms_serial_loss_quantity_proof ON wms_transfer_loss_records;
CREATE TRIGGER wms_serial_loss_quantity_proof AFTER INSERT ON wms_transfer_loss_records FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();

CREATE FUNCTION wms_serial_validate_pending() RETURNS trigger LANGUAGE plpgsql AS $$
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
 ELSE
  SELECT warehouse_id INTO source_warehouse FROM wms_events WHERE tenant=NEW.tenant AND id=NEW.key;
  IF NOT FOUND OR wms_serial_event_valid_read_source(NEW.tenant,source_warehouse,NEW.key) IS DISTINCT FROM true THEN
   RAISE EXCEPTION 'Original serial source event lacks complete exact inputs, effects or source relationships' USING ERRCODE='23514';
  END IF;
  -- Every inserted historical step still needs its current projection. Also
  -- cover projections naming this event without a matching step. A later
  -- command in the same transaction may legitimately own the current unit;
  -- validate those complete latest events once each in this same snapshot.
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
CREATE CONSTRAINT TRIGGER wms_serial_validate_pending AFTER INSERT ON wms_serial_pending_checks
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_validate_pending();
