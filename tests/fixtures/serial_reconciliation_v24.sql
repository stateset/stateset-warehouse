-- Rebuild source validity from original facts, not current stock/UID totals.
-- Identity is tenant-global; physical custody is checked at this warehouse.
-- A valid original event proves every one of its exact steps/observations.
-- Retain separate projection, orphan/scope, mode, batch and global-head checks.
-- Materialization is confined to this read-only statement's snapshot.
WITH event_sources AS MATERIALIZED (
 SELECT e.id,e.warehouse_id,wms_serial_event_valid(e.tenant,e.warehouse_id,e.id) AS ok
 FROM wms_events e WHERE e.tenant=$1
  AND (EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=$1 AND m.controlled)
   OR e.payload ?| ARRAY['serial_heads','serial_steps','serial_inputs'])
), original AS (
 SELECT
  NOT EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=$1 AND NOT wms_serial_mode_original(m))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=$1 AND m.controlled
   AND NOT EXISTS(SELECT 1 FROM wms_serial_heads h WHERE h.tenant=m.tenant AND h.sku=m.sku))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_heads h WHERE h.tenant=$1 AND NOT wms_serial_head_bound(h))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_batches b WHERE b.tenant=$1 AND NOT wms_serial_batch_original(b))
  AND NOT EXISTS(SELECT 1 FROM event_sources WHERE NOT ok)
  AND NOT EXISTS(SELECT 1 FROM wms_serial_steps s
   LEFT JOIN event_sources e ON e.id=s.event_id AND e.warehouse_id=s.warehouse_id
   WHERE s.tenant=$1 AND NOT COALESCE(e.ok,false))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_observations o
   LEFT JOIN event_sources e ON e.id=o.event_id AND e.warehouse_id=o.warehouse_id
   WHERE o.tenant=$1 AND NOT COALESCE(e.ok,false))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_units u WHERE u.tenant=$1 AND (
   NOT EXISTS(SELECT 1 FROM wms_serial_steps s WHERE s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial
    AND s.lot=u.lot AND s.revision=u.revision AND s.event_id=u.last_event_id AND s.after_place=u.place)
   OR EXISTS(SELECT 1 FROM wms_serial_steps s WHERE s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial AND s.revision>u.revision)
  )) AS ok
), physical AS (
 SELECT wms_serial_scope_current_valid($1,$2) AND wms_serial_scope_transit_valid($1,$2) AS ok
)
SELECT jsonb_build_object(
 'ok',original.ok AND physical.ok,'source_ok',original.ok,'physical_ok',physical.ok,
 'identity_scope','tenant','warehouse_id',$2,
 'controlled_skus',(SELECT count(*) FROM wms_serial_modes WHERE tenant=$1 AND controlled),
 'registered_units',(SELECT count(*) FROM wms_serial_units WHERE tenant=$1),
 'sku_reviews',(SELECT count(*) FROM wms_serial_heads WHERE tenant=$1),
 'batches',(SELECT count(*) FROM wms_serial_batches WHERE tenant=$1),
 'unit_steps',(SELECT count(*) FROM wms_serial_steps WHERE tenant=$1),
 'observations',(SELECT count(*) FROM wms_serial_observations WHERE tenant=$1),
 'method','original_serial_modes_reviews_history_observations_and_physical_ownership')
FROM original,physical;
