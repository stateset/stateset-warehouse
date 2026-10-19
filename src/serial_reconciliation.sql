-- Rebuild complete original validity in one read-only snapshot.
-- Ordinary events retain every input, head/batch, scoped-step, source/effect
-- and label-membership predicate. Counts, derived historical closures and
-- malformed envelopes use its complete read-only counterpart, with actual
-- event/receipt/batch facts shared only within the current snapshot.
-- Input/command classes are total maps over the selected ordinary events.
-- LEFT JOIN preserves that cardinality estimate; null-safe hash keys preserve
-- the distinction between an absent command and JSON null. No verdict lives
-- beyond this statement, and standalone/write guards are unchanged.
WITH required_events AS MATERIALIZED (
 SELECT e.tenant,e.id,e.warehouse_id,e.payload->'command' AS command,
  COALESCE(e.payload->'serial_inputs','[]'::jsonb) AS inputs,COALESCE(e.payload->'serial_heads','{}'::jsonb) AS heads,
  CASE WHEN jsonb_typeof(COALESCE(e.payload->'serial_steps','[]'::jsonb))='array'
   THEN jsonb_array_length(COALESCE(e.payload->'serial_steps','[]'::jsonb)) ELSE 0 END AS step_count,
  CASE WHEN jsonb_typeof(COALESCE(e.payload->'serial_steps','[]'::jsonb)) IS DISTINCT FROM 'array' THEN false
   WHEN jsonb_array_length(COALESCE(e.payload->'serial_steps','[]'::jsonb))>10000 THEN false
   WHEN jsonb_typeof(COALESCE(e.payload->'serial_heads','{}'::jsonb)) IS DISTINCT FROM 'object' THEN false
   ELSE COALESCE(e.payload#>>'{command,type}','') NOT IN('count_stock','ship','unpack_shipment','handoff_shipment','cancel_receipt') END AS direct
 FROM wms_events e WHERE e.tenant=$1
  AND (EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=$1 AND m.controlled)
   OR e.payload ?| ARRAY['serial_heads','serial_steps','serial_inputs'])
), input_classes AS MATERIALIZED (
 SELECT DISTINCT warehouse_id,command,inputs FROM required_events WHERE direct
), input_sources AS MATERIALIZED (
 SELECT warehouse_id,command,inputs,wms_serial_inputs_match_command($1,warehouse_id,command,inputs) AS ok FROM input_classes
), command_sources AS MATERIALIZED (
 SELECT warehouse_id,command,CASE WHEN bool_or(ok) THEN
  (SELECT COALESCE(array_agg(sku ORDER BY sku),ARRAY[]::text[]) FROM wms_serial_event_skus($1,warehouse_id,command))
  ELSE ARRAY[]::text[] END AS expected FROM input_sources GROUP BY warehouse_id,command
), event_contexts AS MATERIALIZED (
 SELECT e.*,c.expected,
  CASE WHEN i.ok THEN
   (SELECT count(*) FROM wms_serial_observations WHERE tenant=e.tenant AND warehouse_id=e.warehouse_id AND event_id=e.id)=jsonb_array_length(e.inputs)
   AND NOT EXISTS(
    SELECT 1 FROM jsonb_array_elements(e.inputs) WITH ORDINALITY AS item(doc,ordinal)
    LEFT JOIN wms_serial_observations o ON o.tenant=e.tenant AND o.warehouse_id=e.warehouse_id AND o.event_id=e.id AND o.ordinal=item.ordinal
     AND o.sku=item.doc->>'sku' AND o.target=item.doc->'target' AND o.serials=item.doc->'serials'
     AND e.heads->o.sku=to_jsonb(o.sku_version)
    WHERE o.event_id IS NULL)
   ELSE false END AS inputs_bound
 FROM required_events e
 LEFT JOIN input_sources i ON i.warehouse_id=e.warehouse_id AND (i.command IS NULL)=(e.command IS NULL) AND COALESCE(i.command,'null'::jsonb)=COALESCE(e.command,'null'::jsonb) AND i.inputs=e.inputs
 LEFT JOIN command_sources c ON c.warehouse_id=e.warehouse_id AND (c.command IS NULL)=(e.command IS NULL) AND COALESCE(c.command,'null'::jsonb)=COALESCE(e.command,'null'::jsonb)
 WHERE e.direct
), step_sources AS MATERIALIZED (
 SELECT step.event_id,step.warehouse_id=e.warehouse_id AS in_scope,
  wms_serial_step_source_for_event(step,
   EXISTS(SELECT 1 FROM wms_serial_observations o WHERE o.tenant=e.tenant AND o.warehouse_id=e.warehouse_id AND o.event_id=e.id
    AND o.sku=step.sku AND o.target=step.target AND o.serials @> jsonb_build_array(step.serial)),
   EXISTS(SELECT 1 FROM wms_serial_observations o WHERE o.tenant=e.tenant AND o.warehouse_id=e.warehouse_id AND o.event_id=e.id
    AND o.sku=step.sku AND o.target='[]'::jsonb)) AS ok
 FROM wms_serial_steps step JOIN event_contexts e ON e.tenant=step.tenant AND e.id=step.event_id
 WHERE e.inputs_bound
), step_summaries AS MATERIALIZED (
 SELECT event_id,count(*) FILTER(WHERE in_scope) AS scoped_steps,NOT COALESCE(bool_or(NOT ok),false) AS source_ok
 FROM step_sources GROUP BY event_id
), batch_summaries AS MATERIALIZED (
 SELECT b.event_id,count(*) AS scoped_batches FROM wms_serial_batches b
 JOIN event_contexts e ON e.tenant=b.tenant AND e.warehouse_id=b.warehouse_id AND e.id=b.event_id
 WHERE e.inputs_bound GROUP BY b.event_id
), event_sources AS MATERIALIZED (
 SELECT e.id,e.warehouse_id,CASE WHEN NOT e.inputs_bound THEN false ELSE COALESCE((
  e.expected=ARRAY(SELECT key FROM jsonb_object_keys(e.heads) key ORDER BY key)
  AND COALESCE(s.scoped_steps,0)=e.step_count AND COALESCE(s.source_ok,true)
  AND COALESCE(b.scoped_batches,0)=cardinality(e.expected)
  AND NOT EXISTS(
   SELECT 1 FROM jsonb_each(e.heads) head(sku,version) WHERE
    CASE WHEN jsonb_typeof(head.version) IS DISTINCT FROM 'number' THEN true
     WHEN head.version#>>'{}'!~'^[1-9][0-9]*$' THEN true
     ELSE NOT EXISTS(SELECT 1 FROM wms_serial_batches batch
      WHERE batch.tenant=e.tenant AND batch.sku=head.sku AND batch.warehouse_id=e.warehouse_id AND batch.event_id=e.id
       AND to_jsonb(batch.version)=head.version AND wms_serial_batch_original(batch)) END
  )
  AND NOT EXISTS(
   SELECT 1 FROM wms_serial_observations o CROSS JOIN LATERAL jsonb_array_elements_text(o.serials) AS unit(label)
   WHERE o.tenant=e.tenant AND o.warehouse_id=e.warehouse_id AND o.event_id=e.id
    AND NOT EXISTS(SELECT 1 FROM wms_serial_steps step WHERE step.tenant=e.tenant AND step.event_id=e.id
     AND step.sku=o.sku AND step.serial=unit.label AND step.target=o.target)
  )),false) END AS ok
 FROM event_contexts e LEFT JOIN step_summaries s ON s.event_id=e.id LEFT JOIN batch_summaries b ON b.event_id=e.id
 UNION ALL
 SELECT e.id,e.warehouse_id,wms_serial_event_valid_read_source(e.tenant,e.warehouse_id,e.id)
 FROM required_events e WHERE NOT e.direct
-- The final JSON references each verdict twice. Evaluate these complete
-- STABLE proofs once in this statement, rather than letting CTE folding
-- duplicate their checks. Nothing survives the original read snapshot.
), original AS MATERIALIZED (
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
), physical AS MATERIALIZED (
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
