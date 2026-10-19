-- Packages preserve exact allocation ownership across partial fulfillment.
-- Historical full-order handoffs and their original immutable documents stay intact.
ALTER TABLE wms_orders DROP CONSTRAINT wms_orders_status_check;
ALTER TABLE wms_orders ADD CONSTRAINT wms_orders_status_check CHECK(status IN('pending','allocated','picking','picked','packed','partially_shipped','shipped','cancelled'));
CREATE TABLE wms_split_orders (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,order_id TEXT NOT NULL,
 source_event_id TEXT NOT NULL UNIQUE REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 source_version BIGINT NOT NULL CHECK(source_version>0),document JSONB NOT NULL,
 PRIMARY KEY(tenant,warehouse_id,order_id), FOREIGN KEY(tenant,warehouse_id,order_id) REFERENCES wms_orders(tenant,warehouse_id,id)
);
CREATE TABLE wms_packages (
 seq BIGSERIAL UNIQUE,tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,id TEXT NOT NULL,order_id TEXT NOT NULL,
 reference TEXT NOT NULL CHECK(length(btrim(reference)) BETWEEN 1 AND 240),actor TEXT NOT NULL,packed_at TEXT NOT NULL,
 source_event_id TEXT NOT NULL UNIQUE REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 source_version BIGINT NOT NULL CHECK(source_version>0),
 PRIMARY KEY(tenant,warehouse_id,id),UNIQUE(tenant,warehouse_id,id,order_id),UNIQUE(tenant,warehouse_id,reference),
 FOREIGN KEY(tenant,warehouse_id,order_id) REFERENCES wms_split_orders(tenant,warehouse_id,order_id)
);
CREATE INDEX wms_packages_order ON wms_packages(tenant,warehouse_id,order_id,seq);
CREATE TABLE wms_package_lines (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,package_id TEXT NOT NULL,order_id TEXT NOT NULL,line_id TEXT NOT NULL,
 sku TEXT NOT NULL,location TEXT NOT NULL,lot TEXT NOT NULL,quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000),
 PRIMARY KEY(tenant,warehouse_id,package_id,line_id,location,lot),
 FOREIGN KEY(tenant,warehouse_id,package_id,order_id) REFERENCES wms_packages(tenant,warehouse_id,id,order_id),
 FOREIGN KEY(tenant,warehouse_id,order_id,line_id,sku) REFERENCES wms_order_lines(tenant,warehouse_id,order_id,id,sku),
 FOREIGN KEY(tenant,warehouse_id,order_id,line_id,location,lot) REFERENCES wms_allocations(tenant,warehouse_id,order_id,line_id,location,lot) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX wms_package_allocation ON wms_package_lines(tenant,warehouse_id,order_id,line_id,location,lot);
CREATE TABLE wms_package_closures (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,package_id TEXT NOT NULL,order_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN('unpacked','handed_off')),reason TEXT NOT NULL,carrier TEXT NOT NULL,handoff_reference TEXT NOT NULL,
 actor TEXT NOT NULL,closed_at TEXT NOT NULL,event_id TEXT NOT NULL UNIQUE REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 version BIGINT NOT NULL CHECK(version>0), PRIMARY KEY(tenant,warehouse_id,package_id),
 FOREIGN KEY(tenant,warehouse_id,package_id,order_id) REFERENCES wms_packages(tenant,warehouse_id,id,order_id),
 CHECK((kind='unpacked' AND length(btrim(reason)) BETWEEN 1 AND 240 AND carrier='' AND handoff_reference='') OR
       (kind='handed_off' AND reason='' AND length(btrim(carrier)) BETWEEN 1 AND 240 AND length(btrim(handoff_reference)) BETWEEN 1 AND 240))
);
ALTER TABLE wms_movements ADD COLUMN package_id TEXT,ADD COLUMN order_line_id TEXT,ADD COLUMN allocation_location TEXT,
 ADD COLUMN shipment_event_id TEXT REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 ADD FOREIGN KEY(tenant,warehouse_id,package_id,order_line_id,allocation_location,lot) REFERENCES wms_package_lines(tenant,warehouse_id,package_id,line_id,location,lot) DEFERRABLE INITIALLY DEFERRED,
 ADD CONSTRAINT wms_package_movement_identity CHECK((package_id IS NULL AND order_line_id IS NULL AND allocation_location IS NULL AND shipment_event_id IS NULL)
 OR (kind='shipment' AND package_id IS NOT NULL AND order_line_id IS NOT NULL AND allocation_location IS NOT NULL AND shipment_event_id IS NOT NULL)),
 ADD CONSTRAINT wms_package_movement_data CHECK(data->>'package_id' IS NOT DISTINCT FROM package_id AND data->>'order_line_id' IS NOT DISTINCT FROM order_line_id
 AND data->>'allocation_location' IS NOT DISTINCT FROM allocation_location AND data->>'shipment_event_id' IS NOT DISTINCT FROM shipment_event_id);
CREATE UNIQUE INDEX wms_package_one_handoff_line ON wms_movements(tenant,warehouse_id,package_id,order_line_id,allocation_location,lot) WHERE package_id IS NOT NULL;

CREATE FUNCTION wms_split_header(d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT jsonb_build_object('id',d->'id','reference',d->'reference','source_order_id',d->'source_order_id','customer',d->'customer','destination',d->'destination','priority',d->'priority','created_at',d->'created_at');
$$;
CREATE FUNCTION wms_split_contract(d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT CASE WHEN jsonb_typeof(d->'lines')='array' THEN wms_split_header(d)||jsonb_build_object('lines',
 COALESCE((SELECT jsonb_agg(jsonb_build_object('id',l->'id','sku',l->'sku','quantity',l->'quantity') ORDER BY l->>'id') FROM jsonb_array_elements(d->'lines') l),'[]'::jsonb)) END;
$$;
-- Conditionally retain the new typed field. Old source documents keep exactly
-- their previous shape, including the absence of a shipment_mode property.
CREATE OR REPLACE FUNCTION wms_order_header(d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT CASE WHEN jsonb_typeof(d)='object' THEN jsonb_build_object(
 'id',d->'id','reference',d->'reference','source_order_id',d->'source_order_id','customer',d->'customer','destination',d->'destination','priority',d->'priority','status',d->'status',
 'carrier',d->'carrier','handoff_reference',d->'handoff_reference','created_at',d->'created_at','shipped_at',d->'shipped_at')||
 CASE WHEN d->>'shipment_mode' IS NOT NULL THEN jsonb_build_object('shipment_mode',d->'shipment_mode') ELSE '{}'::jsonb END END;
$$;
CREATE FUNCTION wms_split_status(t TEXT,w TEXT,o TEXT) RETURNS TEXT LANGUAGE SQL STABLE AS $$
 SELECT CASE WHEN q.shipped=q.ordered THEN 'shipped' WHEN q.shipped>0 THEN 'partially_shipped' WHEN q.picked=q.ordered THEN 'picked'
 WHEN q.picked>0 THEN 'picking' WHEN q.allocated>0 THEN 'allocated' ELSE 'pending' END FROM
 (SELECT (SELECT sum(quantity) FROM wms_order_lines WHERE tenant=t AND warehouse_id=w AND order_id=o) AS ordered,
 COALESCE(sum(shipped),0) AS shipped,COALESCE(sum(picked),0) AS picked,COALESCE(sum(quantity-released),0) AS allocated FROM wms_allocations WHERE tenant=t AND warehouse_id=w AND order_id=o) q;
$$;
CREATE FUNCTION wms_package_command_lines(d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT CASE WHEN jsonb_typeof(d)='array' THEN (SELECT jsonb_agg(jsonb_build_object('line_id',l->'line_id','location',l->'location','lot',l->'lot','quantity',l->'quantity') ORDER BY l->>'line_id',l->>'location',l->>'lot') FROM jsonb_array_elements(d) l) END;
$$;
CREATE FUNCTION wms_package_document(t TEXT,w TEXT,p TEXT) RETURNS JSONB LANGUAGE SQL STABLE AS $$
 SELECT to_jsonb(h)-'tenant'-'warehouse_id'-'seq'||jsonb_build_object('lines',(
 SELECT jsonb_agg(to_jsonb(l)-'tenant'-'warehouse_id'-'package_id'-'order_id' ORDER BY l.line_id,l.location,l.lot) FROM wms_package_lines l WHERE l.tenant=t AND l.warehouse_id=w AND l.package_id=p),
 'closure',(SELECT to_jsonb(c)-'tenant'-'warehouse_id'-'package_id'-'order_id' FROM wms_package_closures c WHERE c.tenant=t AND c.warehouse_id=w AND c.package_id=p))
 FROM wms_packages h WHERE h.tenant=t AND h.warehouse_id=w AND h.id=p;
$$;
CREATE FUNCTION wms_split_consistent(t TEXT,w TEXT,o TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE b RECORD; h RECORD; terminal RECORD; e RECORD; d JSONB; expected JSONB; n BIGINT; latest RECORD;
BEGIN
 SELECT * INTO b FROM wms_split_orders WHERE tenant=t AND warehouse_id=w AND order_id=o;
 IF NOT FOUND THEN RETURN false; END IF;
 d=wms_current_order(t,w,o);
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

CREATE OR REPLACE FUNCTION wms_shipment_consistent(t TEXT,w TEXT,o TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT CASE WHEN EXISTS(SELECT 1 FROM wms_split_orders WHERE tenant=t AND warehouse_id=w AND order_id=o)
 THEN wms_split_consistent(t,w,o) AND EXISTS(SELECT 1 FROM wms_package_closures WHERE tenant=t AND warehouse_id=w AND order_id=o AND kind='handed_off')
 ELSE COALESCE((SELECT wms_shipment_facts_match(t,w,o,e.document) FROM wms_shipment_evidence e WHERE e.tenant=t AND e.warehouse_id=w AND e.order_id=o),false) END;
$$;
CREATE FUNCTION wms_actual_shipped(t TEXT,w TEXT,o TEXT) RETURNS TABLE(line_id TEXT,sku TEXT,lot TEXT,quantity BIGINT) LANGUAGE SQL STABLE AS $$
 SELECT s.line_id,s.sku,s.lot,sum(s.quantity)::bigint FROM (
 SELECT a->>'line_id' AS line_id,a->>'sku' AS sku,a->>'lot' AS lot,(a->>'shipped')::bigint AS quantity FROM wms_shipment_evidence e CROSS JOIN LATERAL jsonb_array_elements(e.document->'allocations') a WHERE e.tenant=t AND e.warehouse_id=w AND e.order_id=o
 UNION ALL SELECT l.line_id,l.sku,l.lot,l.quantity FROM wms_package_lines l JOIN wms_package_closures c ON c.tenant=l.tenant AND c.warehouse_id=l.warehouse_id AND c.package_id=l.package_id AND c.kind='handed_off' WHERE l.tenant=t AND l.warehouse_id=w AND l.order_id=o) s GROUP BY s.line_id,s.sku,s.lot;
$$;
CREATE OR REPLACE FUNCTION wms_require_shipment(t TEXT,w TEXT,o TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM wms_orders WHERE tenant=t AND warehouse_id=w AND id=o AND status IN('shipped','partially_shipped'))
 AND NOT EXISTS(SELECT 1 FROM wms_shipment_evidence WHERE tenant=t AND warehouse_id=w AND order_id=o)
 AND NOT EXISTS(SELECT 1 FROM wms_package_closures WHERE tenant=t AND warehouse_id=w AND order_id=o AND kind='handed_off') THEN
 RAISE EXCEPTION 'Completed handoff requires original shipment evidence' USING ERRCODE='23514'; END IF;
END $$;
CREATE OR REPLACE FUNCTION wms_shipment_movement_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 PERFORM wms_serialize_shipment(NEW.tenant,NEW.warehouse_id);
 IF NEW.package_id IS NULL AND (EXISTS(SELECT 1 FROM wms_shipment_evidence e WHERE e.tenant=NEW.tenant AND e.warehouse_id=NEW.warehouse_id AND e.document->>'reference'=NEW.reference)
 OR EXISTS(SELECT 1 FROM wms_orders o WHERE o.tenant=NEW.tenant AND o.warehouse_id=NEW.warehouse_id AND o.reference=NEW.reference AND o.data->>'shipment_mode'='packages')) THEN
 RAISE EXCEPTION 'Shipment movement requires its exact original handoff ownership' USING ERRCODE='23514'; END IF;
 RETURN NEW;
END $$;
CREATE OR REPLACE FUNCTION wms_shipment_movement_required() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.package_id IS NULL AND NOT EXISTS(SELECT 1 FROM wms_shipment_evidence e WHERE e.tenant=NEW.tenant AND e.warehouse_id=NEW.warehouse_id AND e.document->>'reference'=NEW.reference) THEN
 RAISE EXCEPTION 'Shipment movement requires its original completed order evidence' USING ERRCODE='23514'; END IF;
 RETURN NULL;
END $$;

CREATE FUNCTION wms_split_before_order() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE b JSONB;
BEGIN
 SELECT document INTO b FROM wms_split_orders WHERE tenant=OLD.tenant AND warehouse_id=OLD.warehouse_id AND order_id=OLD.id;
 IF FOUND AND (TG_OP='DELETE' OR (NEW.tenant,NEW.warehouse_id,NEW.id) IS DISTINCT FROM (OLD.tenant,OLD.warehouse_id,OLD.id)
  OR NEW.data->>'shipment_mode' IS DISTINCT FROM 'packages' OR wms_split_header(NEW.data) IS DISTINCT FROM b-'lines') THEN
 RAISE EXCEPTION 'Original split fulfillment contract and mode are immutable' USING ERRCODE='23514'; END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;RETURN NEW;
END $$;
CREATE TRIGGER wms_split_order_identity BEFORE UPDATE OR DELETE ON wms_orders FOR EACH ROW EXECUTE FUNCTION wms_split_before_order();
-- Packing changes physical ownership even without a stock/order-row delta.
-- Fence scanner and unpick reviews, including claims changed and later restored.
CREATE FUNCTION wms_split_lock() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 PERFORM wms_serialize_shipment(NEW.tenant,NEW.warehouse_id);
 PERFORM wms_bump_resource(NEW.tenant,NEW.warehouse_id,jsonb_build_array('order',NEW.order_id));
 RETURN NEW;END $$;
CREATE FUNCTION wms_split_commit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE o TEXT;r RECORD;
BEGIN
 IF TG_OP='DELETE' THEN r=OLD;ELSE r=NEW;END IF;
 IF TG_TABLE_NAME='wms_movements' THEN SELECT order_id INTO o FROM wms_packages WHERE tenant=r.tenant AND warehouse_id=r.warehouse_id AND id=r.package_id;
 ELSIF TG_TABLE_NAME='wms_orders' THEN o=r.id;ELSE o=r.order_id;END IF;
 IF o IS NOT NULL AND (EXISTS(SELECT 1 FROM wms_split_orders WHERE tenant=r.tenant AND warehouse_id=r.warehouse_id AND order_id=o)
 OR EXISTS(SELECT 1 FROM wms_orders WHERE tenant=r.tenant AND warehouse_id=r.warehouse_id AND id=o AND data->>'shipment_mode'='packages'))
 AND NOT COALESCE(wms_split_consistent(r.tenant,r.warehouse_id,o),false) THEN
 RAISE EXCEPTION 'Split fulfillment lacks exact packing, physical handoff or reservation evidence' USING ERRCODE='23514';END IF;RETURN NULL;
END $$;
DO $$ DECLARE table_name TEXT; BEGIN
 FOREACH table_name IN ARRAY ARRAY['wms_split_orders','wms_packages','wms_package_lines','wms_package_closures'] LOOP
 EXECUTE format('CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION wms_immutable_history()',table_name||'_immutable',table_name);
 EXECUTE format('CREATE TRIGGER %I BEFORE TRUNCATE ON %I FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history()',table_name||'_no_truncate',table_name);
 EXECUTE format('CREATE TRIGGER %I BEFORE INSERT ON %I FOR EACH ROW EXECUTE FUNCTION wms_split_lock()',table_name||'_lock',table_name);
 EXECUTE format('CREATE CONSTRAINT TRIGGER %I AFTER INSERT ON %I DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_split_commit()',table_name||'_proof',table_name);
 END LOOP;
END $$;
CREATE CONSTRAINT TRIGGER wms_split_header_proof AFTER INSERT OR UPDATE ON wms_orders DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_split_commit();
CREATE CONSTRAINT TRIGGER wms_split_allocation_proof AFTER INSERT OR UPDATE OR DELETE ON wms_allocations DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_split_commit();
CREATE CONSTRAINT TRIGGER wms_split_movement_proof AFTER INSERT ON wms_movements DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN(NEW.package_id IS NOT NULL) EXECUTE FUNCTION wms_split_commit();

CREATE OR REPLACE FUNCTION wms_seed_shipment_evidence(t TEXT,w TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE r RECORD; e RECORD; n BIGINT; legacy_version BIGINT; legacy_order JSONB;
BEGIN
 FOR r IN SELECT o.id FROM wms_orders o WHERE o.tenant=t AND o.warehouse_id=w AND o.status='shipped' AND o.data->>'shipment_mode' IS DISTINCT FROM 'packages'
  AND NOT EXISTS(SELECT 1 FROM wms_shipment_evidence s WHERE s.tenant=t AND s.warehouse_id=w AND s.order_id=o.id) LOOP
  SELECT count(*) INTO n FROM wms_events WHERE tenant=t AND warehouse_id=w AND payload->>'type'='fulfillment.shipped' AND payload#>>'{order,id}'=r.id;
  IF n=1 THEN
   SELECT * INTO e FROM wms_events WHERE tenant=t AND warehouse_id=w AND payload->>'type'='fulfillment.shipped' AND payload#>>'{order,id}'=r.id;
   INSERT INTO wms_shipment_evidence(tenant,warehouse_id,order_id,source_kind,event_id,source_version,document)
    VALUES(t,w,r.id,'event',e.id,e.version,wms_canonical_order(e.payload->'order'));
  ELSIF n>1 THEN RAISE EXCEPTION 'Multiple full-order shipment events require reconciliation' USING ERRCODE='23514';
  ELSE
   SELECT s.version,a INTO legacy_version,legacy_order FROM wms_workspaces s CROSS JOIN LATERAL jsonb_array_elements(s.state->'orders') a
    WHERE s.tenant=t AND s.warehouse_id=w AND a->>'id'=r.id AND a->>'status'='shipped';
   IF FOUND THEN
    INSERT INTO wms_shipment_evidence(tenant,warehouse_id,order_id,source_kind,source_version,document)
     VALUES(t,w,r.id,'legacy_snapshot',legacy_version,wms_canonical_order(legacy_order));
   ELSE
    INSERT INTO wms_shipment_evidence(tenant,warehouse_id,order_id,source_kind,source_version,document)
     SELECT t,w,r.id,'synthetic_fixture',h.version,wms_current_order(t,w,r.id) FROM wms_warehouses h WHERE h.tenant=t AND h.warehouse_id=w;
   END IF;
  END IF;
 END LOOP;
 IF EXISTS(SELECT 1 FROM wms_events event WHERE event.tenant=t AND event.warehouse_id=w AND event.payload->>'type'='fulfillment.shipped'
  AND NOT EXISTS(SELECT 1 FROM wms_shipment_evidence evidence WHERE evidence.event_id=event.id)) THEN
  RAISE EXCEPTION 'Original shipment events disagree with retained warehouse ownership' USING ERRCODE='23514';
 END IF;
END $$;


CREATE OR REPLACE FUNCTION wms_check_return_limit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE shipped BIGINT; authorized BIGINT;
BEGIN
 UPDATE wms_warehouses SET version=version WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id;
 IF NOT EXISTS(SELECT 1 FROM wms_orders WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND id=NEW.order_id AND status IN('shipped','partially_shipped'))
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

CREATE FUNCTION wms_split_line_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM wms_split_orders WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND order_id=NEW.order_id)
 AND NOT EXISTS(SELECT 1 FROM wms_order_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND order_id=NEW.order_id AND id=NEW.id AND sku=NEW.sku AND quantity=NEW.quantity) THEN
 RAISE EXCEPTION 'Original split order lines cannot be added or replaced' USING ERRCODE='23514'; END IF;RETURN NEW;
END $$;
CREATE TRIGGER wms_split_line_identity BEFORE INSERT ON wms_order_lines FOR EACH ROW EXECUTE FUNCTION wms_split_line_identity();

-- Conditional v1 may use one complete physical package, with its actual event.
-- Multiple or partial handoffs can never be represented as one invented shipment.
CREATE OR REPLACE FUNCTION wms_native_fulfillment_shipment() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.kind='oms_fulfillment' AND NOT EXISTS(
   SELECT 1 FROM wms_events e
   WHERE e.id=NEW.frozen->>'event_id'
     AND e.tenant=NEW.tenant AND e.warehouse_id=NEW.warehouse_id
     AND (e.payload->>'type'='fulfillment.shipped' OR (
       e.payload->>'type'='shipment.handed_off'
       AND e.payload#>>'{order,shipment_mode}'='packages'
       AND wms_split_consistent(NEW.tenant,NEW.warehouse_id,NEW.order_id)
       AND (SELECT count(*) FROM wms_package_closures c JOIN wms_packages p
         ON (p.tenant,p.warehouse_id,p.id)=(c.tenant,c.warehouse_id,c.package_id)
         WHERE p.tenant=NEW.tenant AND p.warehouse_id=NEW.warehouse_id
           AND p.order_id=NEW.order_id AND c.kind='handed_off')=1
       AND EXISTS(SELECT 1 FROM wms_package_closures c JOIN wms_packages p
         ON (p.tenant,p.warehouse_id,p.id)=(c.tenant,c.warehouse_id,c.package_id)
         WHERE p.tenant=NEW.tenant AND p.warehouse_id=NEW.warehouse_id
           AND p.order_id=NEW.order_id AND c.kind='handed_off' AND c.event_id=e.id)
     ))
     AND e.payload->'order'->>'id'=NEW.order_id
     AND e.payload->'order'->>'status'='shipped'
     AND NEW.frozen#>>'{payload,event_id}'=e.id
     AND NEW.frozen#>>'{payload,warehouse_id}'=NEW.warehouse_id
     AND NEW.frozen#>>'{payload,warehouse_order_id}'=NEW.order_id
     AND NEW.frozen#>>'{payload,carrier}'=e.payload#>>'{order,carrier}'
     AND NEW.frozen#>>'{payload,handoff_reference}'=e.payload#>>'{order,handoff_reference}'
     AND NEW.frozen#>>'{payload,handed_off_at}'=e.payload#>>'{order,shipped_at}'
     AND e.payload#>>'{order,source_order_id}'=
       'oms:'||(NEW.frozen->>'recipient_tenant_id')||':'||(NEW.frozen#>>'{payload,order_id}')
     AND jsonb_typeof(NEW.frozen#>'{payload,lines}')='array'
     AND (
       SELECT jsonb_agg(jsonb_build_array(l->>'line_id',l->>'sku',l->'quantity_each') ORDER BY l->>'line_id')
       FROM jsonb_array_elements(NEW.frozen#>'{payload,lines}') l
     ) = (
       SELECT jsonb_agg(jsonb_build_array(l->>'id',l->>'sku',l->'quantity') ORDER BY l->>'id')
       FROM jsonb_array_elements(e.payload#>'{order,lines}') l
     )
 ) THEN
   RAISE EXCEPTION 'Conditional OMS fulfillment requires its committed physical shipment event';
 END IF;
 RETURN NEW;
END $$;
