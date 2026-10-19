-- Close only the unshipped remainder of a partly shipped package order.
-- Original demand, physical handoffs and return allowance remain immutable.
ALTER TABLE wms_orders DROP CONSTRAINT wms_orders_status_check;
ALTER TABLE wms_orders ADD CONSTRAINT wms_orders_status_check CHECK(status IN('pending','allocated','picking','picked','packed','partially_shipped','closed_partial','shipped','cancelled'));
DROP INDEX wms_orders_open;
CREATE INDEX wms_orders_open ON wms_orders(tenant,warehouse_id,seq) WHERE status NOT IN('shipped','cancelled','closed_partial');
CREATE TABLE wms_order_cancellations (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,order_id TEXT NOT NULL,
 reason TEXT NOT NULL CHECK(length(btrim(reason)) BETWEEN 1 AND 240),
 actor TEXT NOT NULL CHECK(length(btrim(actor)) BETWEEN 1 AND 240),cancelled_at TEXT NOT NULL,
 source_event_id TEXT NOT NULL UNIQUE REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 source_version BIGINT NOT NULL CHECK(source_version>0),document JSONB NOT NULL,
 PRIMARY KEY(tenant,warehouse_id,order_id),
 FOREIGN KEY(tenant,warehouse_id,order_id) REFERENCES wms_split_orders(tenant,warehouse_id,order_id)
);
CREATE TABLE wms_order_cancelled_lines (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,order_id TEXT NOT NULL,line_id TEXT NOT NULL,sku TEXT NOT NULL,
 ordered BIGINT NOT NULL CHECK(ordered BETWEEN 1 AND 1000000),
 shipped BIGINT NOT NULL CHECK(shipped>=0),cancelled BIGINT NOT NULL CHECK(cancelled>=0 AND shipped+cancelled=ordered),
 PRIMARY KEY(tenant,warehouse_id,order_id,line_id),
 FOREIGN KEY(tenant,warehouse_id,order_id) REFERENCES wms_order_cancellations(tenant,warehouse_id,order_id),
 FOREIGN KEY(tenant,warehouse_id,order_id,line_id,sku) REFERENCES wms_order_lines(tenant,warehouse_id,order_id,id,sku)
);
CREATE FUNCTION wms_order_cancellation_document(t TEXT,w TEXT,o TEXT) RETURNS JSONB LANGUAGE SQL STABLE AS $$
 SELECT jsonb_build_object('reason',c.reason,'actor',c.actor,'cancelled_at',c.cancelled_at,'lines',
 COALESCE((SELECT jsonb_object_agg(l.line_id,jsonb_build_object('sku',l.sku,'ordered',l.ordered,'shipped',l.shipped,'cancelled',l.cancelled))
 FROM wms_order_cancelled_lines l WHERE l.tenant=t AND l.warehouse_id=w AND l.order_id=o),'{}'::jsonb))
 FROM wms_order_cancellations c WHERE c.tenant=t AND c.warehouse_id=w AND c.order_id=o;
$$;
CREATE FUNCTION wms_cancelled_allocations(d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT CASE WHEN jsonb_typeof(d->'allocations')='array' THEN
 COALESCE((SELECT jsonb_agg(a ORDER BY a->>'line_id',a->>'location',a->>'lot') FROM jsonb_array_elements(d->'allocations') a),'[]'::jsonb) END;
$$;
CREATE OR REPLACE FUNCTION wms_order_header(d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT CASE WHEN jsonb_typeof(d)='object' THEN jsonb_build_object(
 'id',d->'id','reference',d->'reference','source_order_id',d->'source_order_id','customer',d->'customer','destination',d->'destination','priority',d->'priority','status',d->'status',
 'carrier',d->'carrier','handoff_reference',d->'handoff_reference','created_at',d->'created_at','shipped_at',d->'shipped_at')||
 CASE WHEN d->>'shipment_mode' IS NOT NULL THEN jsonb_build_object('shipment_mode',d->'shipment_mode') ELSE '{}'::jsonb END||
 CASE WHEN d->>'remainder_cancellation' IS NOT NULL THEN jsonb_build_object('remainder_cancellation',d->'remainder_cancellation') ELSE '{}'::jsonb END END;
$$;

CREATE FUNCTION wms_order_cancellation_consistent(t TEXT,w TEXT,o TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE h RECORD; e RECORD; d JSONB; expected JSONB;
BEGIN
 SELECT * INTO h FROM wms_order_cancellations WHERE tenant=t AND warehouse_id=w AND order_id=o;
 IF NOT FOUND THEN
  RETURN NOT EXISTS(SELECT 1 FROM wms_orders WHERE tenant=t AND warehouse_id=w AND id=o
   AND (status='closed_partial' OR COALESCE(data->'remainder_cancellation','null'::jsonb)<>'null'::jsonb));
 END IF;
 d=wms_current_order(t,w,o);
 expected=wms_order_cancellation_document(t,w,o);
 IF d IS NULL OR d->>'status' IS DISTINCT FROM 'closed_partial' OR d->>'shipment_mode' IS DISTINCT FROM 'packages'
 OR h.document IS DISTINCT FROM expected OR d->'remainder_cancellation' IS DISTINCT FROM expected
 OR h.source_version>(SELECT version FROM wms_warehouses WHERE tenant=t AND warehouse_id=w)
 OR NOT EXISTS(SELECT 1 FROM wms_order_cancelled_lines WHERE tenant=t AND warehouse_id=w AND order_id=o AND shipped>0)
 OR NOT EXISTS(SELECT 1 FROM wms_order_cancelled_lines WHERE tenant=t AND warehouse_id=w AND order_id=o AND cancelled>0)
 OR (SELECT count(*) FROM wms_order_cancelled_lines WHERE tenant=t AND warehouse_id=w AND order_id=o)
   <>(SELECT count(*) FROM wms_order_lines WHERE tenant=t AND warehouse_id=w AND order_id=o)
 OR EXISTS(SELECT 1 FROM wms_order_cancelled_lines c JOIN wms_order_lines l
   ON l.tenant=c.tenant AND l.warehouse_id=c.warehouse_id AND l.order_id=c.order_id AND l.id=c.line_id
   WHERE c.tenant=t AND c.warehouse_id=w AND c.order_id=o AND (c.sku<>l.sku OR c.ordered<>l.quantity
   OR c.shipped<>(SELECT COALESCE(sum(s.quantity),0) FROM wms_actual_shipped(t,w,o) s WHERE s.line_id=c.line_id AND s.sku=c.sku)))
 OR EXISTS(SELECT 1 FROM wms_allocations WHERE tenant=t AND warehouse_id=w AND order_id=o AND (picked<>shipped OR released<>quantity-shipped))
 OR EXISTS(SELECT 1 FROM wms_packages p LEFT JOIN wms_package_closures c
   ON c.tenant=p.tenant AND c.warehouse_id=p.warehouse_id AND c.package_id=p.id
   WHERE p.tenant=t AND p.warehouse_id=w AND p.order_id=o
   AND (c.package_id IS NULL OR p.source_version>=h.source_version OR c.version>=h.source_version))
 THEN RETURN false; END IF;
 SELECT * INTO e FROM wms_events WHERE id=h.source_event_id AND tenant=t AND warehouse_id=w AND version=h.source_version;
 IF NOT FOUND OR e.payload->>'type' IS DISTINCT FROM 'fulfillment.remainder_cancelled'
 OR e.payload->>'id' IS DISTINCT FROM e.id OR e.payload->>'tenant' IS DISTINCT FROM t OR e.payload->>'warehouse_id' IS DISTINCT FROM w
 OR e.payload->'version' IS DISTINCT FROM to_jsonb(e.version) OR e.payload->>'actor' IS DISTINCT FROM h.actor
 OR e.payload->'command' IS DISTINCT FROM jsonb_build_object('type','cancel_remainder','order_id',o,'reason',h.reason)
 OR e.payload#>>'{result,entity_id}' IS DISTINCT FROM o OR e.payload#>'{result,version}' IS DISTINCT FROM to_jsonb(e.version)
 OR wms_order_header(e.payload->'order') IS DISTINCT FROM wms_order_header(d)
 OR wms_split_contract(e.payload->'order') IS DISTINCT FROM wms_split_contract(d)
 OR wms_cancelled_allocations(e.payload->'order') IS DISTINCT FROM wms_cancelled_allocations(d)
 OR NOT EXISTS(SELECT 1 FROM wms_commands c WHERE c.tenant=t AND c.warehouse_id=w
   AND c.request_id=e.payload->>'request_id' AND c.actor=h.actor AND c.result=e.payload->'result')
 THEN RETURN false; END IF;
 RETURN true;
END $$;
CREATE OR REPLACE FUNCTION wms_split_status(t TEXT,w TEXT,o TEXT) RETURNS TEXT LANGUAGE SQL STABLE AS $$
 SELECT CASE WHEN EXISTS(SELECT 1 FROM wms_order_cancellations WHERE tenant=t AND warehouse_id=w AND order_id=o) THEN 'closed_partial' WHEN q.shipped=q.ordered THEN 'shipped' WHEN q.shipped>0 THEN 'partially_shipped' WHEN q.picked=q.ordered THEN 'picked'
 WHEN q.picked>0 THEN 'picking' WHEN q.allocated>0 THEN 'allocated' ELSE 'pending' END FROM
 (SELECT (SELECT sum(quantity) FROM wms_order_lines WHERE tenant=t AND warehouse_id=w AND order_id=o) AS ordered,
 COALESCE(sum(shipped),0) AS shipped,COALESCE(sum(picked),0) AS picked,COALESCE(sum(quantity-released),0) AS allocated FROM wms_allocations WHERE tenant=t AND warehouse_id=w AND order_id=o) q;
$$;
CREATE OR REPLACE FUNCTION wms_split_consistent(t TEXT,w TEXT,o TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE b RECORD; h RECORD; terminal RECORD; e RECORD; d JSONB; expected JSONB; n BIGINT; latest RECORD;
BEGIN
 SELECT * INTO b FROM wms_split_orders WHERE tenant=t AND warehouse_id=w AND order_id=o;
 IF NOT FOUND THEN RETURN false; END IF;
 d=wms_current_order(t,w,o);
 IF NOT COALESCE(wms_order_cancellation_consistent(t,w,o),false) THEN RETURN false; END IF;
 IF d IS NULL OR d->>'shipment_mode' IS DISTINCT FROM 'packages' OR wms_split_contract(d) IS DISTINCT FROM b.document THEN RETURN false; END IF;
 SELECT * INTO e FROM wms_events WHERE id=b.source_event_id AND tenant=t AND warehouse_id=w AND version=b.source_version;
 IF NOT FOUND OR b.source_version>(SELECT version FROM wms_warehouses WHERE tenant=t AND warehouse_id=w)
 OR e.payload->>'type' IS DISTINCT FROM (CASE e.payload#>>'{command,type}' WHEN 'allocate_available' THEN 'fulfillment.available_allocated' WHEN 'pack_shipment' THEN 'shipment.packed' ELSE 'invalid' END)
 OR COALESCE(e.payload#>>'{command,type}','') NOT IN('allocate_available','pack_shipment') OR e.payload#>>'{command,order_id}' IS DISTINCT FROM o
 OR e.payload->>'id' IS DISTINCT FROM e.id OR e.payload->>'tenant' IS DISTINCT FROM t OR e.payload->>'warehouse_id' IS DISTINCT FROM w
 OR e.payload->'version' IS DISTINCT FROM to_jsonb(e.version) OR wms_split_contract(e.payload->'order') IS DISTINCT FROM b.document THEN RETURN false; END IF;
 IF EXISTS(SELECT 1 FROM wms_shipment_evidence WHERE tenant=t AND warehouse_id=w AND order_id=o)
 OR EXISTS(SELECT 1 FROM wms_movements WHERE tenant=t AND warehouse_id=w AND kind='shipment' AND reference=d->>'reference' AND package_id IS NULL)
 OR EXISTS(SELECT 1 FROM wms_order_lines l WHERE l.tenant=t AND l.warehouse_id=w AND l.order_id=o AND l.quantity<
  (SELECT COALESCE(sum(a.quantity-a.released),0) FROM wms_allocations a WHERE a.tenant=t AND a.warehouse_id=w AND a.order_id=o AND a.line_id=l.id)) THEN RETURN false; END IF;
 FOR h IN SELECT * FROM wms_packages WHERE tenant=t AND warehouse_id=w AND order_id=o LOOP
  SELECT * INTO e FROM wms_events WHERE id=h.source_event_id AND tenant=t AND warehouse_id=w AND version=h.source_version;
  IF NOT FOUND OR h.source_version<b.source_version OR h.source_version>(SELECT version FROM wms_warehouses WHERE tenant=t AND warehouse_id=w)
  OR e.payload->>'type' IS DISTINCT FROM 'shipment.packed' OR e.payload->>'id' IS DISTINCT FROM e.id
  OR e.payload->>'tenant' IS DISTINCT FROM t OR e.payload->>'warehouse_id' IS DISTINCT FROM w OR e.payload->'version' IS DISTINCT FROM to_jsonb(e.version)
  OR e.payload->'shipment' IS DISTINCT FROM (wms_package_document(t,w,h.id)||jsonb_build_object('closure',NULL))
  OR wms_split_contract(e.payload->'order') IS DISTINCT FROM b.document
  OR e.payload->>'actor' IS DISTINCT FROM h.actor OR e.payload#>>'{command,type}' IS DISTINCT FROM 'pack_shipment'
  OR e.payload#>>'{command,order_id}' IS DISTINCT FROM o OR e.payload#>>'{command,reference}' IS DISTINCT FROM h.reference
  OR e.payload#>>'{result,entity_id}' IS DISTINCT FROM h.id OR e.payload#>'{result,version}' IS DISTINCT FROM to_jsonb(e.version) THEN RETURN false; END IF;
  SELECT jsonb_agg(jsonb_build_object('line_id',l.line_id,'location',l.location,'lot',l.lot,'quantity',l.quantity) ORDER BY l.line_id,l.location,l.lot) INTO expected
   FROM wms_package_lines l WHERE l.tenant=t AND l.warehouse_id=w AND l.package_id=h.id;
  IF expected IS NULL OR jsonb_array_length(expected)>100 OR expected IS DISTINCT FROM wms_package_command_lines(e.payload#>'{command,lines}') THEN RETURN false; END IF;
  SELECT * INTO terminal FROM wms_package_closures WHERE tenant=t AND warehouse_id=w AND package_id=h.id;
  IF FOUND THEN
   SELECT * INTO e FROM wms_events WHERE id=terminal.event_id AND tenant=t AND warehouse_id=w AND version=terminal.version;
   IF NOT FOUND OR terminal.version<=h.source_version OR terminal.version>(SELECT version FROM wms_warehouses WHERE tenant=t AND warehouse_id=w)
   OR e.payload->>'type' IS DISTINCT FROM (CASE terminal.kind WHEN 'handed_off' THEN 'shipment.handed_off' ELSE 'shipment.unpacked' END)
   OR e.payload->>'id' IS DISTINCT FROM e.id OR e.payload->>'tenant' IS DISTINCT FROM t OR e.payload->>'warehouse_id' IS DISTINCT FROM w OR e.payload->'version' IS DISTINCT FROM to_jsonb(e.version)
   OR e.payload->'shipment' IS DISTINCT FROM wms_package_document(t,w,h.id)
   OR wms_split_contract(e.payload->'order') IS DISTINCT FROM b.document
   OR e.payload->>'actor' IS DISTINCT FROM terminal.actor OR e.payload#>>'{command,shipment_id}' IS DISTINCT FROM h.id
   OR e.payload#>>'{command,type}' IS DISTINCT FROM (CASE terminal.kind WHEN 'handed_off' THEN 'handoff_shipment' ELSE 'unpack_shipment' END)
   OR e.payload#>>'{result,entity_id}' IS DISTINCT FROM h.id OR e.payload#>'{result,version}' IS DISTINCT FROM to_jsonb(e.version)
   OR (terminal.kind='handed_off' AND (e.payload#>>'{command,carrier}' IS DISTINCT FROM terminal.carrier OR e.payload#>>'{command,handoff_reference}' IS DISTINCT FROM terminal.handoff_reference))
   OR (terminal.kind='unpacked' AND e.payload#>>'{command,reason}' IS DISTINCT FROM terminal.reason) THEN RETURN false; END IF;
  END IF;
  IF EXISTS(SELECT 1 FROM wms_package_lines l LEFT JOIN wms_movements m ON m.tenant=l.tenant AND m.warehouse_id=l.warehouse_id AND m.package_id=l.package_id
    AND m.order_line_id=l.line_id AND m.allocation_location=l.location AND m.lot=l.lot
   WHERE l.tenant=t AND l.warehouse_id=w AND l.package_id=h.id AND (
   (terminal.kind='handed_off' AND (m.id IS NULL OR m.kind<>'shipment' OR m.sku<>l.sku OR m.quantity<>l.quantity OR m.from_location IS DISTINCT FROM 'STAGE-01' OR m.to_location IS NOT NULL
    OR m.reference IS DISTINCT FROM d->>'reference' OR m.shipment_event_id IS DISTINCT FROM terminal.event_id OR m.data->>'actor' IS DISTINCT FROM terminal.actor
    OR m.data->>'at' IS DISTINCT FROM terminal.closed_at OR m.data->>'reason' IS DISTINCT FROM terminal.handoff_reference))
   OR (terminal.kind IS DISTINCT FROM 'handed_off' AND m.id IS NOT NULL))) THEN RETURN false; END IF;
 END LOOP;
 IF EXISTS(SELECT 1 FROM wms_allocations a WHERE a.tenant=t AND a.warehouse_id=w AND a.order_id=o AND (
 a.shipped<>(SELECT COALESCE(sum(l.quantity),0) FROM wms_package_lines l JOIN wms_package_closures c ON c.tenant=l.tenant AND c.warehouse_id=l.warehouse_id AND c.package_id=l.package_id AND c.kind='handed_off'
  WHERE l.tenant=t AND l.warehouse_id=w AND l.order_id=o AND l.line_id=a.line_id AND l.location=a.location AND l.lot=a.lot)
 OR a.picked-a.shipped<(SELECT COALESCE(sum(l.quantity),0) FROM wms_package_lines l LEFT JOIN wms_package_closures c ON c.tenant=l.tenant AND c.warehouse_id=l.warehouse_id AND c.package_id=l.package_id
  WHERE l.tenant=t AND l.warehouse_id=w AND l.order_id=o AND l.line_id=a.line_id AND l.location=a.location AND l.lot=a.lot AND c.package_id IS NULL))) THEN RETURN false; END IF;
 IF d->>'status'='cancelled' THEN
  IF EXISTS(SELECT 1 FROM wms_allocations WHERE tenant=t AND warehouse_id=w AND order_id=o AND (picked<>0 OR shipped<>0 OR released<>quantity))
   OR EXISTS(SELECT 1 FROM wms_packages p LEFT JOIN wms_package_closures c ON c.tenant=p.tenant AND c.warehouse_id=p.warehouse_id AND c.package_id=p.id WHERE p.tenant=t AND p.warehouse_id=w AND p.order_id=o AND c.kind IS DISTINCT FROM 'unpacked') THEN RETURN false; END IF;
 ELSIF d->>'status' IS DISTINCT FROM wms_split_status(t,w,o) THEN RETURN false; END IF;
 SELECT count(*) INTO n FROM wms_package_closures WHERE tenant=t AND warehouse_id=w AND order_id=o AND kind='handed_off';
 IF n=0 THEN RETURN d->>'carrier'='' AND d->>'handoff_reference'='' AND d->'shipped_at'='null'::jsonb; END IF;
 SELECT * INTO latest FROM wms_package_closures WHERE tenant=t AND warehouse_id=w AND order_id=o AND kind='handed_off' ORDER BY version DESC LIMIT 1;
 RETURN d->>'shipped_at'=latest.closed_at AND CASE WHEN n=1 THEN d->>'carrier'=latest.carrier AND d->>'handoff_reference'=latest.handoff_reference ELSE d->>'carrier'='' AND d->>'handoff_reference'='' END;
END $$;
CREATE OR REPLACE FUNCTION wms_require_shipment(t TEXT,w TEXT,o TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM wms_orders WHERE tenant=t AND warehouse_id=w AND id=o AND status IN('shipped','partially_shipped','closed_partial'))
 AND NOT EXISTS(SELECT 1 FROM wms_shipment_evidence WHERE tenant=t AND warehouse_id=w AND order_id=o)
 AND NOT EXISTS(SELECT 1 FROM wms_package_closures WHERE tenant=t AND warehouse_id=w AND order_id=o AND kind='handed_off') THEN
 RAISE EXCEPTION 'Completed handoff requires original shipment evidence' USING ERRCODE='23514'; END IF;
END $$;
CREATE OR REPLACE FUNCTION wms_check_return_limit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE shipped BIGINT; authorized BIGINT;
BEGIN
 UPDATE wms_warehouses SET version=version WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id;
 IF NOT EXISTS(SELECT 1 FROM wms_orders WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND id=NEW.order_id AND status IN('shipped','partially_shipped','closed_partial'))
 OR NOT EXISTS(SELECT 1 FROM wms_receipt_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND receipt_id=NEW.receipt_id AND id=NEW.line_id AND sku=NEW.sku AND lot=NEW.lot AND quantity=NEW.quantity)
 OR EXISTS(SELECT 1 FROM wms_return_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND receipt_id=NEW.receipt_id AND (order_id<>NEW.order_id OR reason<>NEW.reason)) THEN
  RAISE EXCEPTION 'Return source and exact receipt-line ownership disagree' USING ERRCODE='23514';
 END IF;
 IF NOT wms_shipment_consistent(NEW.tenant,NEW.warehouse_id,NEW.order_id) THEN
  RAISE EXCEPTION 'Return requires its consistent immutable original shipment' USING ERRCODE='23514';
 END IF;
 SELECT COALESCE(sum(s.quantity),0) INTO shipped FROM wms_actual_shipped(NEW.tenant,NEW.warehouse_id,NEW.order_id) s
 WHERE s.line_id=NEW.order_line_id AND s.sku=NEW.sku AND s.lot=NEW.lot;
 SELECT COALESCE(sum(r.quantity-COALESCE(c.cancelled,0)),0) INTO authorized
 FROM wms_return_lines r LEFT JOIN wms_receipt_cancelled_lines c
 ON c.tenant=r.tenant AND c.warehouse_id=r.warehouse_id AND c.receipt_id=r.receipt_id AND c.line_id=r.line_id
 WHERE r.tenant=NEW.tenant AND r.warehouse_id=NEW.warehouse_id AND r.order_id=NEW.order_id AND r.order_line_id=NEW.order_line_id AND r.lot=NEW.lot;
 IF authorized>shipped THEN RAISE EXCEPTION 'Returns exceed shipped line/lot quantity' USING ERRCODE='23514'; END IF;
 RETURN NULL;
END $$;

DO $$ DECLARE table_name TEXT; BEGIN
 FOREACH table_name IN ARRAY ARRAY['wms_order_cancellations','wms_order_cancelled_lines'] LOOP
 EXECUTE format('CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION wms_immutable_history()',table_name||'_immutable',table_name);
 EXECUTE format('CREATE TRIGGER %I BEFORE TRUNCATE ON %I FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history()',table_name||'_no_truncate',table_name);
 EXECUTE format('CREATE TRIGGER %I BEFORE INSERT ON %I FOR EACH ROW EXECUTE FUNCTION wms_split_lock()',table_name||'_lock',table_name);
 EXECUTE format('CREATE CONSTRAINT TRIGGER %I AFTER INSERT ON %I DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_split_commit()',table_name||'_proof',table_name);
 END LOOP;
END $$;
