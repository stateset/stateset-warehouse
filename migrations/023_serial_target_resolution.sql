-- Resolve normalized original command targets once within each validation call.
-- The STABLE snapshot, complete target set and every original predicate remain
-- unchanged. This is local computation, never a persisted validity cache.
CREATE OR REPLACE FUNCTION wms_serial_inputs_match_command(t TEXT,w TEXT,c JSONB,inputs JSONB) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE n BIGINT; target_rows JSONB;
BEGIN
 IF jsonb_typeof(inputs) IS DISTINCT FROM 'array' OR jsonb_array_length(inputs)>100 THEN RETURN false; END IF;
 SELECT COALESCE(jsonb_agg(to_jsonb(q)),'[]'::jsonb) INTO target_rows FROM wms_serial_targets(t,w,c) q;
 -- Each original target is one declaration. A duplicated command line must
 -- not reuse a single observation merely because both entries name one SKU.
 IF EXISTS(SELECT 1 FROM jsonb_to_recordset(target_rows) AS q(target JSONB,sku TEXT,each_quantity BIGINT,allow_zero BOOLEAN) GROUP BY q.target HAVING count(*)>1) THEN RETURN false; END IF;
 -- Validate shapes before expanding any member arrays. CASE keeps malformed
 -- JSON from reaching array operations. Count the bounded groups before labels.
 IF EXISTS(SELECT 1 FROM jsonb_array_elements(inputs) AS entry(doc) WHERE
  CASE WHEN jsonb_typeof(doc) IS DISTINCT FROM 'object' THEN true
   WHEN NOT doc ?& ARRAY['target','sku','serials'] OR doc-ARRAY['target','sku','serials']<>'{}'::jsonb
    OR jsonb_typeof(doc->'target') IS DISTINCT FROM 'array'
    OR jsonb_typeof(doc->'sku') IS DISTINCT FROM 'string'
    OR jsonb_typeof(doc->'serials') IS DISTINCT FROM 'array' THEN true
   ELSE jsonb_array_length(doc->'target')>3 OR NOT wms_serial_text(doc->>'sku') END
 ) THEN RETURN false; END IF;
 SELECT COALESCE(sum(jsonb_array_length(doc->'serials')),0) INTO n FROM jsonb_array_elements(inputs) AS entry(doc);
 IF n>10000 OR EXISTS(SELECT 1 FROM jsonb_array_elements(inputs) AS entry(doc)
  CROSS JOIN LATERAL jsonb_array_elements(doc->'target') AS member(value)
  WHERE jsonb_typeof(value) IS DISTINCT FROM 'string' OR NOT wms_serial_text(value#>>'{}'))
  OR EXISTS(SELECT 1 FROM jsonb_array_elements(inputs) AS entry(doc)
  CROSS JOIN LATERAL jsonb_array_elements(doc->'serials') AS member(value)
  WHERE jsonb_typeof(value) IS DISTINCT FROM 'string' OR NOT wms_serial_text(value#>>'{}')) THEN RETURN false; END IF;
 IF EXISTS(SELECT 1 FROM jsonb_array_elements(inputs) AS entry(doc) GROUP BY entry.doc->'target' HAVING count(*)>1)
  OR EXISTS(SELECT 1 FROM jsonb_array_elements(inputs) AS entry(doc) CROSS JOIN LATERAL jsonb_array_elements(entry.doc->'serials') AS unit(label) GROUP BY entry.doc->>'sku',unit.label HAVING count(*)>1) THEN RETURN false; END IF;
 -- Resolve mode/product membership once and compare complete target sets.
 -- Materialized CTEs are statement-local computations under the same STABLE
 -- snapshot, so no saved validity flag can outlive its source snapshot.
 RETURN (
  WITH targets AS MATERIALIZED (
   SELECT q.*,EXISTS(SELECT 1 FROM wms_products p WHERE p.tenant=t AND p.warehouse_id=w
    AND p.sku=q.sku AND p.data->'serial_controlled'='true'::jsonb) AS product_valid
   FROM jsonb_to_recordset(target_rows) AS q(target JSONB,sku TEXT,each_quantity BIGINT,allow_zero BOOLEAN)
   JOIN wms_serial_modes m ON m.tenant=t AND m.sku=q.sku AND m.controlled
  ), observed AS MATERIALIZED (
   SELECT entry.doc->'target' AS target,entry.doc->>'sku' AS sku,jsonb_array_length(entry.doc->'serials') AS quantity
   FROM jsonb_array_elements(inputs) AS entry(doc)
  )
  SELECT NOT EXISTS(
   SELECT 1 FROM observed i LEFT JOIN targets q ON q.target=i.target AND q.sku=i.sku
    AND q.each_quantity=i.quantity AND (q.each_quantity>0 OR q.allow_zero)
   WHERE q.sku IS NULL
  ) AND NOT EXISTS(
   SELECT 1 FROM targets q LEFT JOIN observed i ON i.target=q.target AND i.sku=q.sku
   WHERE NOT q.product_valid OR i.sku IS NULL
  )
 );
EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range THEN RETURN false;
END $$;


-- Preserve every original group ordinal/head and reject missing/extra rows in
-- one comparison, rather than repeating a separate SQL lookup per group.
CREATE OR REPLACE FUNCTION wms_serial_event_inputs_bound(t TEXT,w TEXT,eid TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE e JSONB; inputs JSONB;
BEGIN
 SELECT payload INTO e FROM wms_events WHERE tenant=t AND warehouse_id=w AND id=eid;
 IF NOT FOUND THEN RETURN false; END IF;
 inputs=COALESCE(e->'serial_inputs','[]'::jsonb);
 IF NOT wms_serial_inputs_match_command(t,w,e->'command',inputs) THEN RETURN false; END IF;
 RETURN (SELECT count(*) FROM wms_serial_observations WHERE tenant=t AND warehouse_id=w AND event_id=eid)=jsonb_array_length(inputs)
 AND NOT EXISTS(
  SELECT 1 FROM jsonb_array_elements(inputs) WITH ORDINALITY AS item(doc,ordinal)
  LEFT JOIN wms_serial_observations o ON o.tenant=t AND o.warehouse_id=w AND o.event_id=eid AND o.ordinal=item.ordinal
   AND o.sku=item.doc->>'sku' AND o.target=item.doc->'target' AND o.serials=item.doc->'serials'
   AND e->'serial_heads'->o.sku=to_jsonb(o.sku_version)
  WHERE o.event_id IS NULL
 );
END $$;

-- Internal read-only components share only facts already checked within this
-- event validation call. Standalone step/unit/observation guards remain fully
-- source-bound. These helpers grant no write authority or cached validity.
CREATE FUNCTION wms_serial_step_effect_valid(s wms_serial_steps,e JSONB,observed BOOLEAN,count_observation BOOLEAN) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE c JSONB; command_type TEXT; expected_kind TEXT; original RECORD; matched BOOLEAN=false;
BEGIN
 c=e->'command'; command_type=c->>'type';
 expected_kind=CASE command_type
  WHEN 'count_stock' THEN 'inventory.counted' WHEN 'move_stock' THEN 'inventory.moved'
  WHEN 'receive' THEN 'receipt.received' WHEN 'pick' THEN 'fulfillment.picked' WHEN 'unpick' THEN 'fulfillment.unpicked'
  WHEN 'create_return' THEN 'return.authorized' WHEN 'cancel_receipt' THEN 'receipt.cancelled'
  WHEN 'pack_shipment' THEN 'shipment.packed' WHEN 'unpack_shipment' THEN 'shipment.unpacked'
  WHEN 'handoff_shipment' THEN 'shipment.handed_off' WHEN 'ship' THEN 'fulfillment.shipped'
  WHEN 'dispatch_transfer' THEN 'transfer.dispatched' WHEN 'receive_transfer' THEN 'transfer.received'
  WHEN 'record_transfer_loss' THEN 'transfer.loss_recorded' WHEN 'reverse_transfer_loss' THEN 'transfer.loss_reversed' END;
 IF expected_kind IS NULL OR e->>'type' IS DISTINCT FROM expected_kind OR e->'result'->>'type' IS DISTINCT FROM expected_kind
  OR NOT wms_serial_place_valid(s.after_place) OR (s.before_place IS NOT NULL AND NOT wms_serial_place_valid(s.before_place)) THEN RETURN false; END IF;

 IF command_type='count_stock' THEN
  RETURN COALESCE(s.target='[]'::jsonb AND s.sku=c->>'sku' AND s.lot=c->>'lot' AND (
   (observed AND s.after_place=wms_serial_on_hand(s.warehouse_id,c->>'location') AND (s.before_place IS NULL
    OR s.before_place=wms_serial_on_hand(s.warehouse_id,c->>'location')
    OR (s.before_place->>'status'='missing' AND s.before_place->>'warehouse_id'=s.warehouse_id)))
   OR (NOT observed AND s.before_place=wms_serial_on_hand(s.warehouse_id,c->>'location')
    AND s.after_place=jsonb_build_object('status','missing','warehouse_id',s.warehouse_id,'location',c->>'location')
    AND count_observation)),false);
 ELSIF command_type='move_stock' THEN
  RETURN COALESCE(observed AND s.target='[]'::jsonb AND s.sku=c->>'sku' AND s.lot=c->>'lot'
   AND s.before_place=wms_serial_on_hand(s.warehouse_id,c->>'from') AND s.after_place=wms_serial_on_hand(s.warehouse_id,c->>'to'),false);
 ELSIF command_type IN('pick','unpick') THEN
  IF NOT observed OR s.target<>'[]'::jsonb OR s.lot IS DISTINCT FROM c->>'lot'
   OR NOT EXISTS(SELECT 1 FROM wms_order_lines line WHERE line.tenant=s.tenant AND line.warehouse_id=s.warehouse_id
    AND line.order_id=c->>'order_id' AND line.id=c->>'line_id' AND line.sku=s.sku) THEN RETURN false; END IF;
  IF command_type='pick' THEN RETURN s.before_place=wms_serial_on_hand(s.warehouse_id,c->>'location')
   AND s.after_place=wms_serial_staged(s.warehouse_id,c->>'order_id',c->>'line_id',c->>'location',NULL);
  ELSE RETURN s.before_place=wms_serial_staged(s.warehouse_id,c->>'order_id',c->>'line_id',c->>'location',NULL)
   AND s.after_place=wms_serial_on_hand(s.warehouse_id,c->>'location'); END IF;
 ELSIF command_type='receive' THEN
  IF NOT observed OR s.target<>'[]'::jsonb OR s.after_place<>wms_serial_on_hand(s.warehouse_id,c->>'location') THEN RETURN false; END IF;
  SELECT line.*,r.order_id,r.order_line_id INTO original FROM wms_receipt_lines line LEFT JOIN wms_return_lines r
   ON r.tenant=line.tenant AND r.warehouse_id=line.warehouse_id AND r.receipt_id=line.receipt_id AND r.line_id=line.id
   WHERE line.tenant=s.tenant AND line.warehouse_id=s.warehouse_id AND line.receipt_id=c->>'receipt_id' AND line.id=c->>'line_id' AND line.sku=s.sku AND line.lot=s.lot;
  IF NOT FOUND THEN RETURN false; END IF;
  IF original.order_id IS NULL THEN RETURN s.before_place IS NULL; END IF;
  RETURN COALESCE(s.before_place->>'status'='shipped' AND s.before_place->>'warehouse_id'=s.warehouse_id
   AND s.before_place->>'order_id'=original.order_id AND s.before_place->>'line_id'=original.order_line_id
   AND s.before_place->>'return_receipt_id'=c->>'receipt_id',false);
 ELSIF command_type='create_return' THEN
  IF NOT observed OR jsonb_array_length(s.target)<>1 OR s.before_place->>'status' IS DISTINCT FROM 'shipped'
   OR s.before_place->'return_receipt_id' IS DISTINCT FROM 'null'::jsonb THEN RETURN false; END IF;
  SELECT r.* INTO original FROM wms_return_lines r
   JOIN LATERAL jsonb_array_elements(c->'lines') AS command_line(doc) ON command_line.doc->>'id'=r.line_id
   WHERE r.tenant=s.tenant AND r.warehouse_id=s.warehouse_id AND r.receipt_id=e->'result'->>'entity_id'
   AND r.line_id=s.target->>0 AND r.order_id=c->>'order_id' AND r.sku=s.sku AND r.lot=s.lot
   AND command_line.doc->>'order_line_id'=r.order_line_id AND command_line.doc->>'lot'=r.lot
   AND command_line.doc->'quantity'=to_jsonb(r.quantity) AND r.reason=c->>'reason'
   AND e#>>'{receipt,id}'=r.receipt_id AND e#>>'{receipt,reference}'=c->>'reference'
   AND e#>>'{receipt,return_source,order_id}'=r.order_id AND e#>>'{receipt,return_source,reason}'=r.reason
   AND e#>'{receipt,return_source,line_ids}'->>r.line_id=r.order_line_id
   AND EXISTS(SELECT 1 FROM jsonb_array_elements(e#>'{receipt,lines}') AS receipt_line(doc)
    WHERE receipt_line.doc=jsonb_build_object('id',r.line_id,'sku',r.sku,'lot',r.lot,'quantity',r.quantity,'received',0));
  RETURN COALESCE(FOUND AND s.before_place->>'warehouse_id'=s.warehouse_id AND s.before_place->>'order_id'=original.order_id
   AND s.before_place->>'line_id'=original.order_line_id
   AND s.after_place=s.before_place || jsonb_build_object('return_receipt_id',e->'result'->>'entity_id'),false);
 ELSIF command_type='cancel_receipt' THEN
  RETURN COALESCE(s.target='[]'::jsonb AND s.before_place->>'status'='shipped' AND s.before_place->>'warehouse_id'=s.warehouse_id
   AND s.before_place->>'return_receipt_id'=c->>'receipt_id' AND s.after_place=s.before_place || '{"return_receipt_id":null}'::jsonb
   AND EXISTS(SELECT 1 FROM wms_return_lines r JOIN wms_receipt_cancellations cancellation
    ON cancellation.tenant=r.tenant AND cancellation.warehouse_id=r.warehouse_id AND cancellation.receipt_id=r.receipt_id
    WHERE r.tenant=s.tenant AND r.warehouse_id=s.warehouse_id AND r.receipt_id=c->>'receipt_id' AND r.sku=s.sku AND r.lot=s.lot
    AND r.order_id=s.before_place->>'order_id' AND r.order_line_id=s.before_place->>'line_id'
    AND cancellation.actor=s.actor AND cancellation.reason=c->>'reason' AND cancellation.cancelled_at=e#>>'{receipt,cancellation,cancelled_at}'
    AND e#>>'{receipt,id}'=c->>'receipt_id' AND e#>>'{receipt,status}'='cancelled'),false);
 ELSIF command_type='pack_shipment' THEN
  IF NOT observed OR jsonb_array_length(s.target)<>3 THEN RETURN false; END IF;
  RETURN COALESCE(s.lot=s.target->>2 AND s.before_place=wms_serial_staged(s.warehouse_id,c->>'order_id',s.target->>0,s.target->>1,NULL)
   AND s.after_place=s.before_place || jsonb_build_object('package_id',e->'result'->>'entity_id')
   AND EXISTS(SELECT 1 FROM wms_package_lines line JOIN wms_packages p ON p.tenant=line.tenant AND p.warehouse_id=line.warehouse_id AND p.id=line.package_id
    WHERE line.tenant=s.tenant AND line.warehouse_id=s.warehouse_id AND line.package_id=e->'result'->>'entity_id' AND line.order_id=c->>'order_id'
    AND line.line_id=s.target->>0 AND line.location=s.target->>1 AND line.sku=s.sku AND line.lot=s.lot AND p.source_event_id=s.event_id),false);
 ELSIF command_type IN('unpack_shipment','handoff_shipment') THEN
  IF s.target<>'[]'::jsonb THEN RETURN false; END IF;
  SELECT line.* INTO original FROM wms_package_lines line JOIN wms_package_closures closure
   ON closure.tenant=line.tenant AND closure.warehouse_id=line.warehouse_id AND closure.package_id=line.package_id
   WHERE line.tenant=s.tenant AND line.warehouse_id=s.warehouse_id AND line.package_id=c->>'shipment_id' AND line.sku=s.sku AND line.lot=s.lot
   AND line.line_id=s.before_place->>'line_id' AND line.location=s.before_place->>'allocation_location'
   AND closure.event_id=s.event_id AND closure.kind=CASE WHEN command_type='unpack_shipment' THEN 'unpacked' ELSE 'handed_off' END;
  IF NOT FOUND OR s.before_place IS DISTINCT FROM wms_serial_staged(s.warehouse_id,original.order_id,original.line_id,original.location,original.package_id) THEN RETURN false; END IF;
  IF command_type='unpack_shipment' THEN RETURN s.after_place=s.before_place || '{"package_id":null}'::jsonb;
  ELSE RETURN s.after_place=wms_serial_shipped(s.warehouse_id,original.order_id,original.line_id,original.package_id,s.event_id); END IF;
 ELSIF command_type='ship' THEN
  RETURN COALESCE(s.target='[]'::jsonb AND s.before_place=wms_serial_staged(s.warehouse_id,c->>'order_id',s.before_place->>'line_id',s.before_place->>'allocation_location',NULL)
   AND s.after_place=wms_serial_shipped(s.warehouse_id,c->>'order_id',s.before_place->>'line_id',NULL,s.event_id)
   AND EXISTS(SELECT 1 FROM wms_shipment_evidence h JOIN wms_allocations a ON a.tenant=h.tenant AND a.warehouse_id=h.warehouse_id AND a.order_id=h.order_id
    WHERE h.tenant=s.tenant AND h.warehouse_id=s.warehouse_id AND h.order_id=c->>'order_id' AND h.source_kind='event' AND h.event_id=s.event_id
    AND a.line_id=s.before_place->>'line_id' AND a.location=s.before_place->>'allocation_location' AND a.sku=s.sku AND a.lot=s.lot),false);
 ELSIF command_type='dispatch_transfer' THEN
  IF NOT observed OR jsonb_array_length(s.target)<>1 THEN RETURN false; END IF;
  SELECT line.* INTO original FROM wms_transfer_lines line JOIN wms_transfers transfer ON transfer.tenant=line.tenant AND transfer.id=line.transfer_id
   WHERE line.tenant=s.tenant AND line.source_warehouse_id=s.warehouse_id AND line.transfer_id=e->'result'->>'entity_id' AND line.id=s.target->>0
   AND line.sku=s.sku AND line.lot=s.lot AND line.destination_warehouse_id=c->>'destination_warehouse_id' AND transfer.source_event_id=s.event_id;
  RETURN COALESCE(FOUND AND s.before_place=wms_serial_on_hand(s.warehouse_id,original.location)
   AND s.after_place=wms_serial_transit(s.warehouse_id,original.destination_warehouse_id,original.transfer_id,original.id),false);
 ELSIF command_type IN('receive_transfer','record_transfer_loss') THEN
  IF NOT observed OR s.target<>'[]'::jsonb THEN RETURN false; END IF;
  SELECT line.* INTO original FROM wms_transfer_lines line WHERE line.tenant=s.tenant AND line.destination_warehouse_id=s.warehouse_id
   AND line.transfer_id=c->>'transfer_id' AND line.id=c->>'line_id' AND line.sku=s.sku AND line.lot=s.lot;
  IF NOT FOUND OR s.before_place IS DISTINCT FROM wms_serial_transit(original.source_warehouse_id,s.warehouse_id,original.transfer_id,original.id) THEN RETURN false; END IF;
  IF command_type='receive_transfer' THEN RETURN s.after_place=wms_serial_on_hand(s.warehouse_id,c->>'location'); END IF;
  RETURN s.after_place=jsonb_build_object('status','lost','source_warehouse_id',original.source_warehouse_id,'destination_warehouse_id',s.warehouse_id,
   'transfer_id',original.transfer_id,'line_id',original.id,'loss_id',e->'result'->>'entity_id')
   AND EXISTS(SELECT 1 FROM wms_transfer_loss_records loss WHERE loss.tenant=s.tenant AND loss.warehouse_id=s.warehouse_id
    AND loss.id=e->'result'->>'entity_id' AND loss.kind='loss' AND loss.transfer_id=original.transfer_id AND loss.line_id=original.id AND loss.event_id=s.event_id);
 ELSIF command_type='reverse_transfer_loss' THEN
  IF NOT observed OR s.target<>'[]'::jsonb THEN RETURN false; END IF;
  SELECT line.*,loss.id AS original_loss_id INTO original FROM wms_transfer_lines line JOIN wms_transfer_loss_records loss
   ON loss.tenant=line.tenant AND loss.transfer_id=line.transfer_id AND loss.line_id=line.id
   WHERE line.tenant=s.tenant AND line.destination_warehouse_id=s.warehouse_id AND loss.warehouse_id=s.warehouse_id AND loss.kind='loss'
   AND loss.id=c->>'loss_id' AND line.sku=s.sku AND line.lot=s.lot;
  RETURN COALESCE(FOUND AND s.before_place=jsonb_build_object('status','lost','source_warehouse_id',original.source_warehouse_id,'destination_warehouse_id',s.warehouse_id,
    'transfer_id',original.transfer_id,'line_id',original.id,'loss_id',original.original_loss_id)
   AND s.after_place=wms_serial_transit(original.source_warehouse_id,s.warehouse_id,original.transfer_id,original.id)
   AND EXISTS(SELECT 1 FROM wms_transfer_loss_records reversal WHERE reversal.tenant=s.tenant AND reversal.warehouse_id=s.warehouse_id
    AND reversal.kind='reversal' AND reversal.original_loss_id=original.original_loss_id AND reversal.event_id=s.event_id AND reversal.id=e->'result'->>'entity_id'),false);
 END IF;
 RETURN false;
EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range THEN RETURN false;
END $$;

CREATE FUNCTION wms_serial_step_source_for_event(s wms_serial_steps,observed BOOLEAN,count_observation BOOLEAN) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
 JOIN wms_serial_batches b ON b.tenant=s.tenant AND b.sku=s.sku AND b.version=s.sku_version AND b.event_id=s.event_id AND b.warehouse_id=s.warehouse_id AND b.warehouse_version=s.warehouse_version
 WHERE e.id=s.event_id AND e.tenant=s.tenant AND e.warehouse_id=s.warehouse_id AND e.version=s.warehouse_version
 AND e.payload->>'actor'=s.actor AND c.actor=s.actor AND e.payload->>'occurred_at'=s.recorded_at
 AND e.payload->'result'=c.result AND wms_serial_batch_original(b)
 AND e.payload->'serial_steps'->(s.ordinal-1)=to_jsonb(s)-'tenant' AND wms_serial_step_effect_valid(s,e.payload,observed,count_observation)
 AND (s.revision=1 OR EXISTS(SELECT 1 FROM wms_serial_steps previous
  WHERE previous.tenant=s.tenant AND previous.sku=s.sku AND previous.serial=s.serial AND previous.revision=s.previous_revision
  AND previous.lot=s.lot AND previous.after_place=s.before_place AND previous.sku_version<s.sku_version)));
$$;

CREATE OR REPLACE FUNCTION wms_serial_event_valid(t TEXT,w TEXT,eid TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE e JSONB; inputs JSONB; steps JSONB; heads JSONB; item JSONB; uid JSONB; expected TEXT[]; declared TEXT[];
 source_sku TEXT; source_version BIGINT; point JSONB; actual_count BIGINT; source_unit RECORD; expected_kind TEXT;
BEGIN
 SELECT payload INTO e FROM wms_events WHERE tenant=t AND warehouse_id=w AND id=eid;
 IF NOT FOUND THEN RETURN false; END IF;
 inputs=COALESCE(e->'serial_inputs','[]'::jsonb); steps=COALESCE(e->'serial_steps','[]'::jsonb); heads=COALESCE(e->'serial_heads','{}'::jsonb);
 IF jsonb_typeof(steps) IS DISTINCT FROM 'array' OR jsonb_array_length(steps)>10000 OR jsonb_typeof(heads) IS DISTINCT FROM 'object'
  OR NOT wms_serial_event_inputs_bound(t,w,eid) THEN RETURN false; END IF;
 SELECT COALESCE(array_agg(sku ORDER BY sku),ARRAY[]::text[]) INTO expected FROM wms_serial_event_skus(t,w,e->'command');
 SELECT COALESCE(array_agg(key ORDER BY key),ARRAY[]::text[]) INTO declared FROM jsonb_object_keys(heads) key;
 IF expected<>declared THEN RETURN false; END IF;
 IF (SELECT count(*) FROM wms_serial_steps WHERE tenant=t AND warehouse_id=w AND event_id=eid)<>jsonb_array_length(steps)
  OR EXISTS(SELECT 1 FROM wms_serial_steps step WHERE step.tenant=t AND step.event_id=eid AND NOT wms_serial_step_source_for_event(step,
   EXISTS(SELECT 1 FROM wms_serial_observations o WHERE o.tenant=t AND o.warehouse_id=w AND o.event_id=eid
    AND o.sku=step.sku AND o.target=step.target AND o.serials @> jsonb_build_array(step.serial)),
   EXISTS(SELECT 1 FROM wms_serial_observations o WHERE o.tenant=t AND o.warehouse_id=w AND o.event_id=eid
    AND o.sku=step.sku AND o.target='[]'::jsonb)))
  OR (SELECT count(*) FROM wms_serial_batches WHERE tenant=t AND warehouse_id=w AND event_id=eid)<>cardinality(expected) THEN RETURN false; END IF;
 FOREACH source_sku IN ARRAY expected LOOP
  IF jsonb_typeof(heads->source_sku) IS DISTINCT FROM 'number' OR (heads->>source_sku)!~'^[1-9][0-9]*$' THEN RETURN false; END IF;
  source_version=(heads->>source_sku)::bigint;
  IF NOT EXISTS(SELECT 1 FROM wms_serial_batches batch WHERE batch.tenant=t AND batch.sku=source_sku AND batch.version=source_version
   AND batch.warehouse_id=w AND batch.event_id=eid AND wms_serial_batch_original(batch)) THEN RETURN false; END IF;
 END LOOP;
 FOR item IN SELECT value FROM jsonb_array_elements(inputs) LOOP
  source_sku=item->>'sku'; source_version=(heads->>source_sku)::bigint;
  FOR uid IN SELECT value FROM jsonb_array_elements(item->'serials') LOOP
   IF EXISTS(SELECT 1 FROM wms_serial_steps step WHERE step.tenant=t AND step.event_id=eid AND step.sku=source_sku AND step.serial=uid#>>'{}' AND step.target=item->'target') THEN CONTINUE; END IF;
   point=wms_serial_at_version(t,source_sku,uid#>>'{}',source_version-1);
   IF e->'command'->>'type' IS DISTINCT FROM 'count_stock' OR point IS DISTINCT FROM wms_serial_on_hand(w,e->'command'->>'location')
    OR NOT EXISTS(SELECT 1 FROM wms_serial_units unit WHERE unit.tenant=t AND unit.sku=source_sku AND unit.serial=uid#>>'{}' AND unit.lot=e->'command'->>'lot') THEN RETURN false; END IF;
  END LOOP;
 END LOOP;
 IF e->'command'->>'type'='count_stock' AND cardinality(expected)>0 THEN
  source_sku=e->'command'->>'sku'; source_version=(heads->>source_sku)::bigint;
  -- A count is a complete observed set. Historical candidates are restricted
  -- to this exact bucket; later movements must not rewrite an old count proof.
  SELECT count(*) INTO actual_count FROM (
   SELECT DISTINCT step.serial FROM wms_serial_steps step WHERE step.tenant=t AND step.sku=source_sku AND step.lot=e->'command'->>'lot'
    AND step.sku_version<=source_version AND step.after_place->>'status'='on_hand' AND step.after_place->>'warehouse_id'=w AND step.after_place->>'location'=e->'command'->>'location'
  ) candidates WHERE wms_serial_at_version(t,source_sku,candidates.serial,source_version)=wms_serial_on_hand(w,e->'command'->>'location');
  IF to_jsonb(actual_count) IS DISTINCT FROM e->'command'->'counted' THEN RETURN false; END IF;
 END IF;
 IF e->'command'->>'type' IN('ship','unpack_shipment','handoff_shipment','cancel_receipt') THEN
  FOREACH source_sku IN ARRAY expected LOOP
   source_version=(heads->>source_sku)::bigint;
   FOR source_unit IN SELECT DISTINCT step.serial FROM wms_serial_steps step WHERE step.tenant=t AND step.sku=source_sku AND step.sku_version<source_version
    AND step.after_place->>'warehouse_id'=w AND (
     (e->'command'->>'type'='ship' AND step.after_place->>'status'='staged' AND step.after_place->>'order_id'=e->'command'->>'order_id')
     OR (e->'command'->>'type' IN('unpack_shipment','handoff_shipment') AND step.after_place->>'status'='staged' AND step.after_place->>'package_id'=e->'command'->>'shipment_id')
     OR (e->'command'->>'type'='cancel_receipt' AND step.after_place->>'status'='shipped' AND step.after_place->>'return_receipt_id'=e->'command'->>'receipt_id')) LOOP
    point=wms_serial_at_version(t,source_sku,source_unit.serial,source_version-1);
    IF point->>'warehouse_id'=w AND (
     (e->'command'->>'type'='ship' AND point->>'status'='staged' AND point->>'order_id'=e->'command'->>'order_id' AND point->'package_id'='null'::jsonb)
     OR (e->'command'->>'type' IN('unpack_shipment','handoff_shipment') AND point->>'status'='staged' AND point->>'package_id'=e->'command'->>'shipment_id')
     OR (e->'command'->>'type'='cancel_receipt' AND point->>'status'='shipped' AND point->>'return_receipt_id'=e->'command'->>'receipt_id'))
     AND NOT EXISTS(SELECT 1 FROM wms_serial_steps step WHERE step.tenant=t AND step.sku=source_sku AND step.serial=source_unit.serial AND step.event_id=eid) THEN RETURN false; END IF;
   END LOOP;
  END LOOP;
 END IF;
 RETURN true;
EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range THEN RETURN false;
END $$;
