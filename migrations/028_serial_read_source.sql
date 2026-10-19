-- Match the bytewise collation propagated by literal serial identifiers into
-- historical lookups. Keep the existing version bound and key width; no stored
-- custody value or comparison rule changes.
CREATE INDEX wms_serial_lineage_bytewise
 ON wms_serial_steps(tenant COLLATE "C",sku COLLATE "C",serial,sku_version DESC);

-- Read-only counterpart of the original complete event validator. Resolve the
-- actual event, receipt and original batches once in this STABLE snapshot, then
-- retain every step/source/effect/predecessor predicate and complete historical
-- count/derived-closure check. Nothing persists a verdict or accepts caller-
-- supplied facts; standalone validators and write guards remain unchanged.
CREATE FUNCTION wms_serial_event_valid_read_source(t TEXT,w TEXT,eid TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE e JSONB; inputs JSONB; steps JSONB; heads JSONB; item JSONB; uid JSONB; expected TEXT[]; declared TEXT[];
 source_sku TEXT; source_version BIGINT; point JSONB; actual_count BIGINT; source_unit RECORD; expected_kind TEXT;
 source_event wms_events%ROWTYPE; source_command wms_commands%ROWTYPE; valid_batches wms_serial_batches[];
BEGIN
 SELECT * INTO source_event FROM wms_events WHERE tenant=t AND warehouse_id=w AND id=eid;
 IF NOT FOUND THEN RETURN false; END IF;
 e=CASE WHEN jsonb_typeof(source_event.payload)='object' THEN source_event.payload||'{}'::jsonb ELSE source_event.payload END;
 source_event.payload=e;
 inputs=COALESCE(e->'serial_inputs','[]'::jsonb); steps=COALESCE(e->'serial_steps','[]'::jsonb); heads=COALESCE(e->'serial_heads','{}'::jsonb);
 IF jsonb_typeof(steps) IS DISTINCT FROM 'array' OR jsonb_array_length(steps)>10000 OR jsonb_typeof(heads) IS DISTINCT FROM 'object'
  OR NOT wms_serial_event_inputs_bound(t,w,eid) THEN RETURN false; END IF;
 SELECT COALESCE(array_agg(sku ORDER BY sku),ARRAY[]::text[]) INTO expected FROM wms_serial_event_skus(t,w,e->'command');
 SELECT COALESCE(array_agg(key ORDER BY key),ARRAY[]::text[]) INTO declared FROM jsonb_object_keys(heads) key;
 IF expected<>declared THEN RETURN false; END IF;
 SELECT * INTO source_command FROM wms_commands WHERE tenant=t AND warehouse_id=w AND request_id=e->>'request_id';
 SELECT COALESCE(array_agg(b),ARRAY[]::wms_serial_batches[]) INTO valid_batches
 FROM wms_serial_batches b WHERE b.tenant=t AND b.event_id=eid AND wms_serial_batch_original(b);

 IF (SELECT count(*) FROM wms_serial_steps WHERE tenant=t AND warehouse_id=w AND event_id=eid)<>jsonb_array_length(steps)
  OR EXISTS(SELECT 1 FROM wms_serial_steps step WHERE step.tenant=t AND step.event_id=eid AND NOT EXISTS(SELECT 1 FROM unnest(valid_batches) b
 WHERE source_command.tenant=source_event.tenant AND source_command.warehouse_id=source_event.warehouse_id AND source_command.request_id=source_event.payload->>'request_id'
 AND b.tenant=step.tenant AND b.sku=step.sku AND b.version=step.sku_version AND b.event_id=step.event_id AND b.warehouse_id=step.warehouse_id AND b.warehouse_version=step.warehouse_version
 AND source_event.id=step.event_id AND source_event.tenant=step.tenant AND source_event.warehouse_id=step.warehouse_id AND source_event.version=step.warehouse_version
 AND source_event.payload->>'actor'=step.actor AND source_command.actor=step.actor AND source_event.payload->>'occurred_at'=step.recorded_at
 AND source_event.payload->'result'=source_command.result
 AND source_event.payload->'serial_steps'->(step.ordinal-1)=to_jsonb(step)-'tenant' AND wms_serial_step_effect_valid(step,source_event.payload,(EXISTS(SELECT 1 FROM wms_serial_observations o WHERE o.tenant=t AND o.warehouse_id=w AND o.event_id=eid
    AND o.sku=step.sku AND o.target=step.target AND o.serials @> jsonb_build_array(step.serial))),(EXISTS(SELECT 1 FROM wms_serial_observations o WHERE o.tenant=t AND o.warehouse_id=w AND o.event_id=eid
    AND o.sku=step.sku AND o.target='[]'::jsonb)))
 AND (step.revision=1 OR EXISTS(SELECT 1 FROM wms_serial_steps previous
  WHERE previous.tenant=step.tenant AND previous.sku=step.sku AND previous.serial=step.serial AND previous.revision=step.previous_revision
  AND previous.lot=step.lot AND previous.after_place=step.before_place AND previous.sku_version<step.sku_version))))
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
