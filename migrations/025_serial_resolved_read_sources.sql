-- Read-only event validation with command resolution supplied by one shared
-- statement snapshot. The caller must derive expected SKUs and complete input
-- binding from the original event under that same snapshot. This component
-- does not replace standalone guards, grant writes or persist verdicts.
CREATE FUNCTION wms_serial_event_valid_resolved(t TEXT,w TEXT,eid TEXT,expected TEXT[],inputs_bound BOOLEAN) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE e JSONB; inputs JSONB; steps JSONB; heads JSONB; item JSONB; uid JSONB; declared TEXT[];
 source_sku TEXT; source_version BIGINT; point JSONB; actual_count BIGINT; source_unit RECORD; expected_kind TEXT;
BEGIN
 SELECT payload INTO e FROM wms_events WHERE tenant=t AND warehouse_id=w AND id=eid;
 IF NOT FOUND THEN RETURN false; END IF;
 inputs=COALESCE(e->'serial_inputs','[]'::jsonb); steps=COALESCE(e->'serial_steps','[]'::jsonb); heads=COALESCE(e->'serial_heads','{}'::jsonb);
 IF jsonb_typeof(steps) IS DISTINCT FROM 'array' OR jsonb_array_length(steps)>10000 OR jsonb_typeof(heads) IS DISTINCT FROM 'object'
  OR NOT inputs_bound THEN RETURN false; END IF;
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
