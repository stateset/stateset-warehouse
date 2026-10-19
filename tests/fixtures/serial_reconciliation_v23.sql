-- Rebuild source validity from original facts, not current stock/UID totals.
-- Identity is tenant-global; physical custody is checked at this warehouse.
WITH observation_events AS MATERIALIZED (
 SELECT scope.tenant,scope.warehouse_id,scope.event_id,
  wms_serial_event_inputs_bound(scope.tenant,scope.warehouse_id,scope.event_id) AS inputs_ok
 FROM (SELECT DISTINCT tenant,warehouse_id,event_id FROM wms_serial_observations WHERE tenant=$1) scope
), original AS (
 SELECT
  NOT EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=$1 AND NOT wms_serial_mode_original(m))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=$1 AND m.controlled
   AND NOT EXISTS(SELECT 1 FROM wms_serial_heads h WHERE h.tenant=m.tenant AND h.sku=m.sku))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_heads h WHERE h.tenant=$1 AND NOT wms_serial_head_bound(h))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_batches b WHERE b.tenant=$1 AND NOT wms_serial_batch_original(b))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_units u WHERE u.tenant=$1 AND NOT wms_serial_unit_bound(u))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_steps s WHERE s.tenant=$1 AND NOT wms_serial_step_bound(s))
  AND NOT EXISTS(SELECT 1 FROM wms_serial_observations o
   LEFT JOIN observation_events checked ON checked.tenant=o.tenant AND checked.warehouse_id=o.warehouse_id AND checked.event_id=o.event_id
   WHERE o.tenant=$1 AND (NOT COALESCE(checked.inputs_ok,false) OR NOT EXISTS(
    SELECT 1 FROM wms_events e JOIN wms_serial_batches b ON b.tenant=o.tenant AND b.sku=o.sku
     AND b.version=o.sku_version AND b.event_id=o.event_id AND b.warehouse_id=o.warehouse_id
    WHERE e.id=o.event_id AND e.tenant=o.tenant AND e.warehouse_id=o.warehouse_id AND wms_serial_batch_original(b)
     AND e.payload->'serial_inputs'->(o.ordinal-1)=jsonb_build_object('target',o.target,'sku',o.sku,'serials',o.serials)
   )))
  AND NOT EXISTS(SELECT 1 FROM wms_events e WHERE e.tenant=$1
   AND (EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=$1 AND m.controlled)
    OR e.payload ?| ARRAY['serial_heads','serial_steps','serial_inputs'])
   AND NOT wms_serial_event_valid(e.tenant,e.warehouse_id,e.id)) AS ok
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
