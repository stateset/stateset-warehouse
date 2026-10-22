-- Rebuild complete original validity in one read-only snapshot.
-- Ordinary and count events retain every input, head/batch, scoped-step,
-- source/effect, observed-label and complete historical-count predicate.
-- Derived closures and malformed envelopes use the complete read-only
-- counterpart. Actual event/receipt/batch facts are shared only in this snapshot.
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
   ELSE COALESCE(e.payload#>>'{command,type}','') NOT IN('ship','unpack_shipment','handoff_shipment','cancel_receipt') END AS direct
 FROM wms_events e WHERE e.tenant=$1
  AND (EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=$1 AND m.controlled)
   OR e.payload ?| ARRAY['serial_heads','serial_steps','serial_inputs'])
), input_classes AS MATERIALIZED (
 SELECT DISTINCT warehouse_id,command,inputs FROM required_events WHERE direct
), input_sources AS MATERIALIZED (
 -- The exact target-command set is defined by wms_serial_targets in migration
 -- 021. Outside that set an empty observation has no target to validate; all
 -- nonempty or target-bearing inputs still use the complete original matcher.
 SELECT warehouse_id,command,inputs,CASE
  WHEN inputs='[]'::jsonb AND COALESCE(command->>'type','') NOT IN(
   'count_stock','move_stock','receive','pick','unpick','create_return','pack_shipment',
   'dispatch_transfer','receive_transfer','record_transfer_loss','reverse_transfer_loss') THEN true
  ELSE wms_serial_inputs_match_command($1,warehouse_id,command,inputs) END AS ok FROM input_classes
), command_sources AS MATERIALIZED (
 SELECT warehouse_id,command,CASE WHEN bool_or(ok) THEN
  (SELECT COALESCE(array_agg(sku ORDER BY sku),ARRAY[]::text[]) FROM wms_serial_event_skus($1,warehouse_id,command))
  ELSE ARRAY[]::text[] END AS expected FROM input_sources GROUP BY warehouse_id,command
), event_contexts AS MATERIALIZED (
 SELECT e.*,c.expected,
  CASE WHEN e.command->>'type'='count_stock' THEN
   (SELECT batch.version FROM wms_serial_batches batch WHERE batch.tenant=e.tenant AND batch.sku=e.command->>'sku'
    AND batch.warehouse_id=e.warehouse_id AND batch.event_id=e.id AND to_jsonb(batch.version)=e.heads->(e.command->>'sku')) END AS count_version,
  wms_serial_on_hand(e.warehouse_id,e.command->>'location') AS count_place,
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
), event_facts AS MATERIALIZED (
 -- Resolve original receipt/envelope facts once per event. LEFT JOINs below
 -- keep missing or wrongly scoped facts visible as a failed step source.
 SELECT e.tenant,e.id,e.warehouse_id,src.version,src.payload->>'actor' AS actor,src.payload->>'occurred_at' AS recorded_at,
  src.payload->'result' AS result,src.payload->'serial_steps' AS steps,c.actor AS receipt_actor,c.result AS receipt_result,
  -- The immutable effect predicate reads exactly these four top-level fields.
  -- Decode them once instead of passing the full, potentially TOASTed count
  -- document to a PL/pgSQL call for every historical step. Exact original step
  -- bytes, receipt attribution and all other envelope checks remain separate.
  jsonb_build_object('command',src.payload->'command','type',src.payload->'type',
   'result',src.payload->'result','receipt',src.payload->'receipt') AS effect_payload
 FROM event_contexts e JOIN wms_events src ON src.tenant=e.tenant AND src.id=e.id AND src.warehouse_id=e.warehouse_id
 LEFT JOIN wms_commands c ON c.tenant=src.tenant AND c.warehouse_id=src.warehouse_id AND c.request_id=src.payload->>'request_id'
 WHERE e.inputs_bound
), batch_facts AS MATERIALIZED (
 SELECT b.*,wms_serial_batch_original(b) AS source_ok FROM wms_serial_batches b WHERE b.tenant=$1
), event_step_docs AS MATERIALIZED (
 -- Expand each original steps array once per event. Indexing the stored
 -- payload per step (steps->(ordinal-1)) detoasts and rescans the whole
 -- array for every step, which is quadratic on large count/move envelopes.
 -- The array-type guard preserves the previous NULL result for malformed
 -- non-array payloads; a missing ordinal still compares NULL and fails.
 SELECT f.id AS event_id,item.ordinal::integer AS ordinal,item.doc
 FROM event_facts f
 CROSS JOIN LATERAL jsonb_array_elements(
  CASE WHEN jsonb_typeof(f.steps)='array' THEN f.steps ELSE '[]'::jsonb END
 ) WITH ORDINALITY AS item(doc,ordinal)
), observation_labels AS MATERIALIZED (
 -- Expand observed labels once so per-step and per-event coverage checks
 -- join this narrow set instead of re-probing observation arrays.
 -- The text form matches the original uncovered-label probe; is_string keeps
 -- step coverage identical to serials @> jsonb_build_array(label), which only
 -- matches string elements.
 SELECT o.tenant,o.warehouse_id,o.event_id,o.sku,o.target,unit.doc#>>'{}' AS serial,
  jsonb_typeof(unit.doc)='string' AS is_string
 FROM wms_serial_observations o
 CROSS JOIN LATERAL jsonb_array_elements(o.serials) AS unit(doc)
 WHERE o.tenant=$1
), observed_steps AS MATERIALIZED (
 SELECT DISTINCT tenant,warehouse_id,event_id,sku,target,serial FROM observation_labels WHERE is_string
), count_observations AS MATERIALIZED (
 SELECT DISTINCT tenant,warehouse_id,event_id,sku FROM wms_serial_observations
 WHERE tenant=$1 AND target='[]'::jsonb
), step_sources AS MATERIALIZED (
 SELECT step.event_id,step.warehouse_id=e.warehouse_id AS in_scope,
  COALESCE(b.source_ok AND b.warehouse_version=step.warehouse_version AND f.version=step.warehouse_version
   AND f.actor=step.actor AND f.receipt_actor=step.actor AND f.recorded_at=step.recorded_at
   AND f.result=f.receipt_result
   AND sd.doc=to_jsonb(step)-'tenant'
   AND wms_serial_step_effect_valid(step,f.effect_payload,
    obs.serial IS NOT NULL,
    cnt.sku IS NOT NULL)
   AND (step.revision=1 OR EXISTS(SELECT 1 FROM wms_serial_steps previous
    WHERE previous.tenant=step.tenant AND previous.sku=step.sku AND previous.serial=step.serial AND previous.revision=step.previous_revision
     AND previous.lot=step.lot AND previous.after_place=step.before_place AND previous.sku_version<step.sku_version)),false) AS ok
 FROM wms_serial_steps step JOIN event_contexts e ON e.tenant=step.tenant AND e.id=step.event_id
 LEFT JOIN event_facts f ON f.tenant=step.tenant AND f.id=step.event_id AND f.warehouse_id=step.warehouse_id
 LEFT JOIN event_step_docs sd ON sd.event_id=step.event_id AND sd.ordinal=step.ordinal
 LEFT JOIN observed_steps obs ON obs.tenant=e.tenant AND obs.warehouse_id=e.warehouse_id AND obs.event_id=e.id AND obs.sku=step.sku AND obs.target=step.target AND obs.serial=step.serial
 LEFT JOIN count_observations cnt ON cnt.tenant=e.tenant AND cnt.warehouse_id=e.warehouse_id AND cnt.event_id=e.id AND cnt.sku=step.sku
 LEFT JOIN batch_facts b ON b.tenant=step.tenant AND b.sku=step.sku AND b.version=step.sku_version
  AND b.event_id=step.event_id AND b.warehouse_id=step.warehouse_id
 WHERE e.inputs_bound
), step_summaries AS MATERIALIZED (
 SELECT event_id,count(*) FILTER(WHERE in_scope) AS scoped_steps,NOT COALESCE(bool_or(NOT ok),false) AS source_ok
 FROM step_sources GROUP BY event_id
), batch_summaries AS MATERIALIZED (
 SELECT b.event_id,count(*) AS scoped_batches FROM wms_serial_batches b
 JOIN event_contexts e ON e.tenant=b.tenant AND e.warehouse_id=b.warehouse_id AND e.id=b.event_id
 WHERE e.inputs_bound GROUP BY b.event_id
), stepped_labels AS MATERIALIZED (
 SELECT DISTINCT tenant,event_id,sku,serial,target FROM wms_serial_steps WHERE tenant=$1
), uncovered_labels AS MATERIALIZED (
 -- Observed labels with no matching original step, resolved for every event
 -- in one anti-join instead of a correlated probe per label. Kept separate
 -- from the verdict branches below so those point-in-time checks still
 -- evaluate only for labels that have no step.
 SELECT o.tenant,o.warehouse_id,o.event_id,o.sku,o.serial
 FROM observation_labels o
 LEFT JOIN stepped_labels step ON step.tenant=o.tenant AND step.event_id=o.event_id
  AND step.sku=o.sku AND step.serial=o.serial AND step.target=o.target
 WHERE step.serial IS NULL
), uncovered_count_labels AS MATERIALIZED (
 -- Unchanged labels re-observed by a count have no new step. Their prior
 -- version comes from the event's own matching batch, exactly as before.
 SELECT o.event_id,o.sku,o.serial,e.count_place,e.command->>'lot' AS lot,
  (SELECT batch.version-1 FROM wms_serial_batches batch
   WHERE batch.tenant=e.tenant AND batch.sku=o.sku AND batch.warehouse_id=e.warehouse_id AND batch.event_id=e.id
    AND to_jsonb(batch.version)=e.heads->o.sku) AS prior_version
 FROM uncovered_labels o
 JOIN event_contexts e ON e.tenant=o.tenant AND e.warehouse_id=o.warehouse_id AND e.id=o.event_id
 WHERE e.command->>'type'='count_stock'
), uncovered_count_places AS MATERIALIZED (
 -- wms_serial_at_version for every uncovered count label in one pass: the
 -- highest revision at or below the prior version, or NULL when none exists
 -- (including a NULL prior version), compared with the same count place.
 SELECT DISTINCT ON (c.event_id,c.sku,c.serial) c.event_id,c.sku,c.serial,c.count_place,c.lot,step.after_place
 FROM uncovered_count_labels c
 LEFT JOIN wms_serial_steps step ON step.tenant=$1 AND step.sku=c.sku AND step.serial=c.serial AND step.sku_version<=c.prior_version
 ORDER BY c.event_id,c.sku,c.serial,step.revision DESC NULLS LAST
), bad_label_events AS MATERIALIZED (
 SELECT o.event_id
 FROM uncovered_labels o
 JOIN event_contexts e ON e.tenant=o.tenant AND e.warehouse_id=o.warehouse_id AND e.id=o.event_id
 WHERE e.command->>'type' IS DISTINCT FROM 'count_stock'
 UNION
 SELECT p.event_id FROM uncovered_count_places p
 WHERE p.after_place IS DISTINCT FROM p.count_place
  OR NOT EXISTS(SELECT 1 FROM wms_serial_units u WHERE u.tenant=$1 AND u.sku=p.sku AND u.serial=p.serial AND u.lot=p.lot)
), head_summaries AS MATERIALIZED (
 -- Every declared head needs a canonical positive JSON number and its own
 -- original batch. One hash join per head replaces a per-event scan of the
 -- unindexed batch_facts CTE, which grew with events times batches.
 SELECT e.id AS event_id,bool_or(
  CASE WHEN jsonb_typeof(head.version) IS DISTINCT FROM 'number' THEN true
   WHEN head.version#>>'{}'!~'^[1-9][0-9]*$' THEN true
   ELSE batch.event_id IS NULL END) AS bad
 FROM event_contexts e CROSS JOIN LATERAL jsonb_each(e.heads) head(sku,version)
 LEFT JOIN batch_facts batch ON batch.tenant=e.tenant AND batch.sku=head.sku AND batch.warehouse_id=e.warehouse_id
  AND batch.event_id=e.id AND to_jsonb(batch.version)=head.version AND batch.source_ok
 GROUP BY e.id
), event_sources AS MATERIALIZED (
 SELECT e.id,e.warehouse_id,CASE WHEN NOT e.inputs_bound THEN false ELSE COALESCE((
  e.expected=ARRAY(SELECT key FROM jsonb_object_keys(e.heads) key ORDER BY key)
  AND COALESCE(s.scoped_steps,0)=e.step_count AND COALESCE(s.source_ok,true)
  AND COALESCE(b.scoped_batches,0)=cardinality(e.expected)
  AND NOT COALESCE(h.bad,false)
  AND bad.event_id IS NULL
  -- A count must account for every literal present at its original SKU
  -- version, including labels omitted from an otherwise aligned observation.
  -- The actual matching batch supplies the version, avoiding unsafe casts of
  -- damaged head JSON. Later moves cannot rewrite the original count point.
  -- The latest place at the count version is resolved for every candidate in
  -- one pass (DISTINCT ON max revision at or below the version), which is
  -- exactly wms_serial_at_version evaluated per candidate.
  AND CASE WHEN e.command->>'type'='count_stock' AND cardinality(e.expected)>0 THEN
   (SELECT to_jsonb(count(*)) FROM (
    SELECT DISTINCT ON (step.serial) step.after_place FROM wms_serial_steps step
    WHERE step.tenant=e.tenant AND step.sku=e.command->>'sku'
     AND step.sku_version<=e.count_version
     AND step.serial IN (
      SELECT DISTINCT c.serial FROM wms_serial_steps c
      WHERE c.tenant=e.tenant AND c.sku=e.command->>'sku' AND c.lot=e.command->>'lot'
       AND c.sku_version<=e.count_version
       AND c.after_place->>'status'='on_hand' AND c.after_place->>'warehouse_id'=e.warehouse_id AND c.after_place->>'location'=e.command->>'location')
    ORDER BY step.serial,step.revision DESC
   ) latest WHERE latest.after_place=e.count_place) IS NOT DISTINCT FROM e.command->'counted'
   ELSE true END
  ),false) END AS ok
 FROM event_contexts e LEFT JOIN step_summaries s ON s.event_id=e.id LEFT JOIN batch_summaries b ON b.event_id=e.id
 LEFT JOIN head_summaries h ON h.event_id=e.id LEFT JOIN bad_label_events bad ON bad.event_id=e.id
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
  AND NOT EXISTS(SELECT 1 FROM batch_facts b WHERE NOT b.source_ok)
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
