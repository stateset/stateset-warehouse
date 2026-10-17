CREATE FUNCTION fixture_handoff(order_number INTEGER,event_number INTEGER) RETURNS JSONB
LANGUAGE SQL AS $$
 SELECT jsonb_build_object(
  'order_id',lpad(order_number::text,32,'0')::uuid,
  'request_id',lpad((event_number+10000)::text,32,'0')::uuid,
  'event_id',lpad(event_number::text,32,'0')::uuid,
  'expected_updated_at','2026-10-17T00:00:00Z',
  'warehouse_id','wh-main','warehouse_order_id','10000000-0000-0000-0000-000000000001',
  'carrier','Carrier','handoff_reference','HANDOFF-1','handed_off_at','2026-10-17T01:00:00Z',
  'lines',jsonb_build_array(
   jsonb_build_object('line_id','oms-line-'||encode(sha256(convert_to('source:line-1','UTF8')),'hex'),'sku','SKU-A','quantity_each',2),
   jsonb_build_object('line_id','oms-line-'||encode(sha256(convert_to('position:1','UTF8')),'hex'),'sku','SKU-A','quantity_each',3)
  )
 );
$$;
CREATE FUNCTION expect_rejection(body JSONB,reason TEXT) RETURNS JSONB LANGUAGE plpgsql AS $$
DECLARE answer JSONB;
BEGIN
 answer:=apply_warehouse_fulfillment('tenant-a',body,repeat('d',64));
 IF answer->>'status'<>'rejected' OR answer->>'reason'<>reason
    OR answer->>'request_id'<>body->>'request_id' OR answer->>'event_id'<>body->>'event_id'
    OR answer->>'request_digest'<>repeat('d',64) OR answer->>'observed_updated_at' IS NULL
    OR (answer->>'replayed')::boolean<>false
 THEN RAISE EXCEPTION 'Wrong durable rejection: %',answer; END IF;
 RETURN answer;
END $$;
INSERT INTO orders(id,tenant_id,status,line_items,updated_at,netsuite_id,tracking,source_order_payload)
 SELECT lpad(n::text,32,'0')::uuid,'tenant-a',
  CASE n WHEN 2 THEN 'PROCESSING' WHEN 3 THEN 'CANCELLED' WHEN 4 THEN 'SHIPPED' WHEN 5 THEN 'DELIVERED' WHEN 6 THEN 'FULFILLED' ELSE 'PENDING' END,
  '[{"shopifyLineItemId":"line-1","sku":"SKU-A","quantity":2.0,"price":9.99},{"sku":"SKU-A","quantity":3,"price":9.99}]'::jsonb,
  '2026-10-17T00:00:00Z','NS-ORIGINAL','{"trackingNumber":"UNCHANGED"}','{"arbitrary":"preserve"}'
 FROM generate_series(1,13) n;
DO $$ BEGIN
 IF warehouse_fulfillment_contract()->>'contract'<>'stateset.wms-fulfillment.v1'
    OR warehouse_fulfillment_contract()->>'durable_rejections'<>'true'
 THEN RAISE EXCEPTION 'Database does not attest durable-rejection protocol'; END IF;
END $$;

DO $$
DECLARE answer JSONB;replay JSONB;body JSONB;before TIMESTAMPTZ;
BEGIN
 body:=fixture_handoff(1,101);
 answer:=apply_warehouse_fulfillment('tenant-a',body,repeat('a',64));
 IF answer->>'contract'<>'stateset.wms-fulfillment.v1' OR answer->>'status'<>'fulfilled'
    OR answer->>'tenant_id'<>'tenant-a' OR answer->>'request_digest'<>repeat('a',64)
    OR answer->>'request_id'<>body->>'request_id' OR answer->>'applied_updated_at' IS NULL
    OR (answer->>'replayed')::boolean<>false THEN RAISE EXCEPTION 'Wrong native receipt'; END IF;
 IF (SELECT status FROM orders WHERE id=(body->>'order_id')::uuid)<>'FULFILLED'
    OR (SELECT fulfillment_status FROM orders WHERE id=(body->>'order_id')::uuid)<>'FULFILLED'
    OR (SELECT netsuite_id FROM orders WHERE id=(body->>'order_id')::uuid)<>'NS-ORIGINAL'
    OR (SELECT tracking FROM orders WHERE id=(body->>'order_id')::uuid)<>'{"trackingNumber":"UNCHANGED"}'::jsonb
    OR (SELECT source_order_payload FROM orders WHERE id=(body->>'order_id')::uuid)<>'{"arbitrary":"preserve"}'::jsonb
 THEN RAISE EXCEPTION 'Handoff changed an unrelated field or did not fulfill'; END IF;
 before:=(SELECT updated_at FROM orders WHERE id=(body->>'order_id')::uuid);
 replay:=apply_warehouse_fulfillment('tenant-a',body,repeat('a',64));
 IF replay IS DISTINCT FROM (answer||'{"replayed":true}'::jsonb)
    OR (SELECT updated_at FROM orders WHERE id=(body->>'order_id')::uuid)<>before
    OR (SELECT count(*) FROM warehouse_fulfillment_receipts)<>1 THEN RAISE EXCEPTION 'Replay changed canonical state'; END IF;
 BEGIN
  PERFORM apply_warehouse_fulfillment('tenant-a',body,repeat('b',64));
  RAISE EXCEPTION 'Mismatched fingerprint accepted';
 EXCEPTION WHEN serialization_failure THEN NULL; END;
 BEGIN
  PERFORM apply_warehouse_fulfillment('tenant-a',jsonb_set(body,'{carrier}','"Other carrier"'),repeat('a',64));
  RAISE EXCEPTION 'Mismatched evidence accepted';
 EXCEPTION WHEN serialization_failure THEN NULL; END;
 PERFORM expect_rejection(fixture_handoff(1,102),'order_already_fulfilled');
 UPDATE orders SET status='CANCELLED',updated_at=updated_at+interval '1 second' WHERE id=(body->>'order_id')::uuid;
 replay:=apply_warehouse_fulfillment('tenant-a',body,repeat('a',64));
 IF replay IS DISTINCT FROM (answer||'{"replayed":true}'::jsonb)
    OR (SELECT status FROM orders WHERE id=(body->>'order_id')::uuid)<>'CANCELLED'
 THEN RAISE EXCEPTION 'Retry revived a terminal order'; END IF;
 answer:=apply_warehouse_fulfillment('tenant-a',fixture_handoff(2,103),repeat('c',64));
 IF answer->>'status'<>'fulfilled' THEN RAISE EXCEPTION 'PROCESSING order not accepted'; END IF;
END $$;

DO $$
DECLARE n INTEGER;body JSONB;original JSONB;rejection JSONB;answer JSONB;
BEGIN
 FOR n IN 3..6 LOOP
  PERFORM expect_rejection(fixture_handoff(n,110+n),'source_state_not_eligible');
 END LOOP;
 original:=jsonb_set(fixture_handoff(7,120),'{expected_updated_at}','"2026-10-16T00:00:00Z"');
 rejection:=expect_rejection(original,'source_revision_changed');
 IF (SELECT status FROM orders WHERE id=lpad('7',32,'0')::uuid)<>'PENDING' THEN RAISE EXCEPTION 'Stale rejection applied work'; END IF;
 body:=jsonb_set(jsonb_set(original,'{request_id}','"20000000-0000-0000-0000-000000000001"'),'{expected_updated_at}','"2026-10-17T00:00:00Z"');
 answer:=apply_warehouse_fulfillment('tenant-a',body,repeat('e',64));
 IF answer->>'status'<>'fulfilled' OR answer->>'event_id'<>original->>'event_id' THEN RAISE EXCEPTION 'Reviewed replacement did not apply exact physical event'; END IF;
 answer:=apply_warehouse_fulfillment('tenant-a',original,repeat('d',64));
 IF answer IS DISTINCT FROM (rejection||'{"replayed":true}'::jsonb)
    OR (SELECT status FROM orders WHERE id=lpad('7',32,'0')::uuid)<>'FULFILLED'
 THEN RAISE EXCEPTION 'Late rejected request changed its evidence or revived work'; END IF;
 body:=fixture_handoff(8,121);
 PERFORM expect_rejection(jsonb_set(body,'{lines,0,quantity_each}','3'),'source_lines_changed');
 PERFORM expect_rejection(jsonb_set(fixture_handoff(8,122),'{lines,0,line_id}',to_jsonb('oms-line-'||repeat('f',64))),'source_lines_changed');
 BEGIN
  PERFORM apply_warehouse_fulfillment('tenant-b',body,repeat('d',64));
  RAISE EXCEPTION 'Foreign tenant mutated order';
 EXCEPTION WHEN no_data_found THEN NULL; END;
 BEGIN
  PERFORM apply_warehouse_fulfillment('tenant-a',fixture_handoff(99,123),repeat('d',64));
  RAISE EXCEPTION 'Missing order accepted';
 EXCEPTION WHEN no_data_found THEN NULL; END;
 UPDATE orders SET line_items=jsonb_set(line_items,'{0,quantity}','2.000001') WHERE id=lpad('9',32,'0')::uuid;
 PERFORM expect_rejection(fixture_handoff(9,124),'source_lines_changed');
 UPDATE orders SET line_items=jsonb_set(line_items,'{1,shopifyLineItemId}','"line-1"') WHERE id=lpad('10',32,'0')::uuid;
 PERFORM expect_rejection(fixture_handoff(10,125),'source_lines_changed');
 UPDATE orders SET status='PROCESSING',fulfillment_status='PARTIALLY_FULFILLED' WHERE id=lpad('11',32,'0')::uuid;
 PERFORM expect_rejection(fixture_handoff(11,126),'source_state_not_eligible');
 UPDATE orders SET source_order_payload='{"fulfillment_status":"partial"}' WHERE id=lpad('12',32,'0')::uuid;
 PERFORM expect_rejection(fixture_handoff(12,127),'source_state_not_eligible');
 UPDATE orders SET source_order_payload='{"fulfillment_status":"unknown-forward-state"}' WHERE id=lpad('12',32,'0')::uuid;
 PERFORM expect_rejection(fixture_handoff(12,128),'source_state_not_eligible');
 UPDATE orders SET source_order_payload='{"fulfillmentStatus":"partial"}' WHERE id=lpad('12',32,'0')::uuid;
 PERFORM expect_rejection(fixture_handoff(12,1281),'source_state_not_eligible');
 UPDATE orders SET source_order_payload='{"cancelledAt":"2026-10-17T00:00:00Z"}' WHERE id=lpad('12',32,'0')::uuid;
 PERFORM expect_rejection(fixture_handoff(12,1282),'source_state_not_eligible');
 UPDATE orders SET source_order_payload='{"cancelled_at":" "}' WHERE id=lpad('12',32,'0')::uuid;
 PERFORM expect_rejection(fixture_handoff(12,1283),'source_state_not_eligible');
 body:=jsonb_set(fixture_handoff(13,129),'{event_id}',fixture_handoff(1,101)->'event_id');
 PERFORM expect_rejection(body,'order_already_fulfilled');
 BEGIN
  UPDATE warehouse_fulfillment_receipts SET request_digest=repeat('f',64);
  RAISE EXCEPTION 'Evidence update allowed';
 EXCEPTION WHEN check_violation THEN NULL; END;
 BEGIN
  DELETE FROM warehouse_fulfillment_receipts;
  RAISE EXCEPTION 'Evidence delete allowed';
 EXCEPTION WHEN check_violation THEN NULL; END;
 IF (SELECT count(*) FROM warehouse_fulfillment_receipts WHERE response->>'status'='fulfilled')<>3
    OR (SELECT count(*) FROM warehouse_fulfillment_receipts WHERE response->>'status'='rejected')<>17
    OR (SELECT status FROM orders WHERE id=lpad('8',32,'0')::uuid)<>'PENDING'
    OR (SELECT status FROM orders WHERE id=lpad('13',32,'0')::uuid)<>'PENDING'
 THEN RAISE EXCEPTION 'Rejected handoff side effects or immutable receipt totals wrong'; END IF;
END $$;

CREATE FUNCTION fail_receipt_insert() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN RAISE EXCEPTION 'Injected final receipt failure' USING ERRCODE='23514'; END $$;
CREATE TRIGGER inject_receipt_failure BEFORE INSERT ON warehouse_fulfillment_receipts
 FOR EACH ROW EXECUTE FUNCTION fail_receipt_insert();
DO $$
DECLARE before_count BIGINT;
BEGIN
 before_count:=(SELECT count(*) FROM warehouse_fulfillment_receipts);
 BEGIN
  PERFORM apply_warehouse_fulfillment('tenant-a',fixture_handoff(13,130),repeat('e',64));
  RAISE EXCEPTION 'Injected receipt failure did not fire';
 EXCEPTION WHEN check_violation THEN NULL; END;
 IF (SELECT status FROM orders WHERE id=lpad('13',32,'0')::uuid)<>'PENDING'
    OR (SELECT updated_at FROM orders WHERE id=lpad('13',32,'0')::uuid)<>'2026-10-17T00:00:00Z'::timestamptz
    OR (SELECT count(*) FROM warehouse_fulfillment_receipts)<>before_count
 THEN RAISE EXCEPTION 'Final receipt failure did not roll back order update'; END IF;
END $$;
DROP TRIGGER inject_receipt_failure ON warehouse_fulfillment_receipts;
SELECT 'PASS:conditional_oms_handoff_receiver' AS proof;
