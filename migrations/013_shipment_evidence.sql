-- Completed handoffs have one immutable source, shared by returns, reconciliation
-- and conditional OMS fulfillment. Historical baselines remain explicitly named.
CREATE TABLE wms_shipment_evidence (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, order_id TEXT NOT NULL,
 source_kind TEXT NOT NULL CHECK(source_kind IN('event','legacy_snapshot','synthetic_fixture')),
 event_id TEXT UNIQUE REFERENCES wms_events(id), source_version BIGINT NOT NULL CHECK(source_version>=0),
 document JSONB NOT NULL, recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(tenant,warehouse_id,order_id),
 FOREIGN KEY(tenant,warehouse_id,order_id) REFERENCES wms_orders(tenant,warehouse_id,id),
 CHECK((source_kind='event')=(event_id IS NOT NULL)),
 CHECK(COALESCE(document->>'id'=order_id AND document->>'status'='shipped',false))
);
CREATE INDEX wms_events_shipment_owner ON wms_events(tenant,warehouse_id,(payload#>>'{order,id}'))
 WHERE payload->>'type'='fulfillment.shipped';
CREATE INDEX wms_movements_shipment_owner ON wms_movements(tenant,warehouse_id,reference,sku,lot)
 WHERE kind='shipment';

CREATE FUNCTION wms_serialize_shipment(t TEXT,w TEXT) RETURNS VOID LANGUAGE SQL AS $$
 -- A prior write by this transaction already holds the head lock. Avoid another
 -- row version per allocation in many-lot picks. After transaction-ID wrap the
 -- 64-bit/32-bit comparison conservatively stops skipping rather than aliasing.
 UPDATE wms_warehouses SET version=version WHERE tenant=t AND warehouse_id=w AND xmin::text::bigint<>txid_current();
$$;

CREATE FUNCTION wms_order_header(d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT CASE WHEN jsonb_typeof(d)='object' THEN jsonb_build_object(
 'id',d->'id','reference',d->'reference','source_order_id',d->'source_order_id',
 'customer',d->'customer','destination',d->'destination','priority',d->'priority','status',d->'status',
 'carrier',d->'carrier','handoff_reference',d->'handoff_reference','created_at',d->'created_at','shipped_at',d->'shipped_at') END;
$$;
CREATE FUNCTION wms_canonical_order(d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT CASE WHEN jsonb_typeof(d->'lines')='array' AND jsonb_typeof(d->'allocations')='array'
 THEN wms_order_header(d)||jsonb_build_object(
 'lines',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',l->'id','sku',l->'sku','quantity',l->'quantity') ORDER BY l->>'id') FROM jsonb_array_elements(d->'lines') l),'[]'::jsonb),
 'allocations',COALESCE((SELECT jsonb_agg(jsonb_build_object('line_id',a->'line_id','sku',a->'sku','lot',a->'lot','location',a->'location','quantity',a->'quantity','picked',a->'picked','shipped',a->'shipped','released',a->'released') ORDER BY a->>'line_id',a->>'location',a->>'lot') FROM jsonb_array_elements(d->'allocations') a),'[]'::jsonb)) END;
$$;
CREATE FUNCTION wms_current_order(t TEXT,w TEXT,o TEXT) RETURNS JSONB LANGUAGE SQL STABLE AS $$
 SELECT wms_order_header(r.data)||jsonb_build_object(
 'lines',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',l.id,'sku',l.sku,'quantity',l.quantity) ORDER BY l.id) FROM wms_order_lines l WHERE l.tenant=t AND l.warehouse_id=w AND l.order_id=o),'[]'::jsonb),
 'allocations',COALESCE((SELECT jsonb_agg(jsonb_build_object('line_id',a.line_id,'sku',a.sku,'lot',a.lot,'location',a.location,'quantity',a.quantity,'picked',a.picked,'shipped',a.shipped,'released',a.released) ORDER BY a.line_id,a.location,a.lot) FROM wms_allocations a WHERE a.tenant=t AND a.warehouse_id=w AND a.order_id=o),'[]'::jsonb))
 FROM wms_orders r WHERE r.tenant=t AND r.warehouse_id=w AND r.id=o;
$$;

-- Compare typed business facts and independently recorded physical shipment
-- movements, never cached JSON children or reservation/ledger projections.
CREATE FUNCTION wms_shipment_facts_match(t TEXT,w TEXT,o TEXT,d JSONB) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE current_document JSONB; field TEXT;
BEGIN
 current_document=wms_current_order(t,w,o);
 IF d IS NULL OR current_document IS NULL OR d IS DISTINCT FROM current_document
  OR d->>'status' IS DISTINCT FROM 'shipped' THEN RETURN false; END IF;
 FOREACH field IN ARRAY ARRAY['id','reference','customer','destination','priority','carrier','handoff_reference','created_at','shipped_at'] LOOP
  IF jsonb_typeof(d->field) IS DISTINCT FROM 'string' OR length(btrim(d->>field))=0 THEN RETURN false; END IF;
 END LOOP;
 IF jsonb_typeof(d->'source_order_id') NOT IN('null','string') OR d->>'priority' NOT IN('normal','high','urgent')
  OR jsonb_array_length(d->'lines')=0 OR jsonb_array_length(d->'allocations')=0 THEN RETURN false; END IF;
 IF EXISTS(SELECT 1 FROM wms_allocations a WHERE a.tenant=t AND a.warehouse_id=w AND a.order_id=o
   AND (a.picked<>a.quantity OR a.shipped<>a.quantity OR a.released<>0))
 OR EXISTS(SELECT 1 FROM wms_order_lines l WHERE l.tenant=t AND l.warehouse_id=w AND l.order_id=o
   AND l.quantity<>(SELECT COALESCE(sum(a.shipped),0) FROM wms_allocations a WHERE a.tenant=t AND a.warehouse_id=w AND a.order_id=o AND a.line_id=l.id))
 OR EXISTS(SELECT 1 FROM wms_movements m WHERE m.tenant=t AND m.warehouse_id=w AND m.kind='shipment' AND m.reference=d->>'reference'
   AND (m.from_location IS DISTINCT FROM 'STAGE-01' OR m.to_location IS NOT NULL)) THEN RETURN false; END IF;
 RETURN NOT EXISTS(
  SELECT 1 FROM (SELECT a.sku,a.lot,sum(a.shipped) AS quantity FROM wms_allocations a WHERE a.tenant=t AND a.warehouse_id=w AND a.order_id=o GROUP BY a.sku,a.lot) expected
  FULL JOIN (SELECT m.sku,m.lot,sum(m.quantity) AS quantity FROM wms_movements m WHERE m.tenant=t AND m.warehouse_id=w AND m.kind='shipment' AND m.reference=d->>'reference' GROUP BY m.sku,m.lot) physical USING(sku,lot)
  WHERE expected.quantity IS DISTINCT FROM physical.quantity);
END $$;
CREATE FUNCTION wms_shipment_consistent(t TEXT,w TEXT,o TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT COALESCE((SELECT wms_shipment_facts_match(t,w,o,e.document) FROM wms_shipment_evidence e WHERE e.tenant=t AND e.warehouse_id=w AND e.order_id=o),false);
$$;

CREATE FUNCTION wms_shipment_source_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source JSONB; source_version BIGINT;
BEGIN
 PERFORM wms_serialize_shipment(NEW.tenant,NEW.warehouse_id);
 IF NEW.source_version>(SELECT version FROM wms_warehouses WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id) THEN
  RAISE EXCEPTION 'Shipment source version cannot exceed the warehouse history' USING ERRCODE='23514';
 END IF;
 IF NEW.document IS DISTINCT FROM wms_canonical_order(NEW.document)
  OR NOT wms_shipment_facts_match(NEW.tenant,NEW.warehouse_id,NEW.order_id,NEW.document) THEN
  RAISE EXCEPTION 'Shipment evidence disagrees with canonical order or physical handoff movements' USING ERRCODE='23514';
 END IF;
 IF NEW.source_kind='event' THEN
  SELECT payload,version INTO source,source_version FROM wms_events WHERE id=NEW.event_id AND tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id;
  IF NOT FOUND OR source_version<>NEW.source_version
   OR source->>'type' IS DISTINCT FROM 'fulfillment.shipped' OR source->>'id' IS DISTINCT FROM NEW.event_id
   OR source->>'tenant' IS DISTINCT FROM NEW.tenant OR source->>'warehouse_id' IS DISTINCT FROM NEW.warehouse_id
   OR source->'version' IS DISTINCT FROM to_jsonb(source_version)
   OR source#>>'{command,type}' IS DISTINCT FROM 'ship' OR source#>>'{command,order_id}' IS DISTINCT FROM NEW.order_id
   OR source#>>'{command,carrier}' IS DISTINCT FROM NEW.document->>'carrier'
   OR source#>>'{command,handoff_reference}' IS DISTINCT FROM NEW.document->>'handoff_reference'
   OR source#>>'{result,entity_id}' IS DISTINCT FROM NEW.order_id OR source#>'{result,version}' IS DISTINCT FROM to_jsonb(source_version)
   OR wms_canonical_order(source->'order') IS DISTINCT FROM NEW.document THEN
   RAISE EXCEPTION 'Shipment evidence requires its exact scoped committed handoff event' USING ERRCODE='23514';
  END IF;
 ELSE
  IF EXISTS(SELECT 1 FROM wms_events WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND payload->>'type'='fulfillment.shipped' AND payload#>>'{order,id}'=NEW.order_id) THEN
   RAISE EXCEPTION 'An existing physical event cannot be replaced by a shipment baseline' USING ERRCODE='23514';
  END IF;
  IF NEW.source_kind='legacy_snapshot' THEN
   SELECT s.state,s.version INTO source,source_version FROM wms_workspaces s WHERE s.tenant=NEW.tenant AND s.warehouse_id=NEW.warehouse_id;
   IF NOT FOUND OR source_version<>NEW.source_version OR NOT EXISTS(SELECT 1 FROM jsonb_array_elements(source->'orders') a WHERE a->>'id'=NEW.order_id AND wms_canonical_order(a)=NEW.document) THEN
    RAISE EXCEPTION 'Shipment baseline requires the original retained legacy snapshot' USING ERRCODE='23514';
   END IF;
  ELSIF NEW.warehouse_id<>'wh-pdx' OR NOT EXISTS(SELECT 1 FROM wms_activity a WHERE a.tenant=NEW.tenant AND a.warehouse_id=NEW.warehouse_id
    AND a.data->>'actor'='fixture:synthetic' AND a.data->>'kind'='fulfillment.shipped' AND a.data->>'reference'=NEW.order_id) THEN
   RAISE EXCEPTION 'Synthetic shipment baselines require the disclosed original demo fixture' USING ERRCODE='23514';
  END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_shipment_source_guard BEFORE INSERT ON wms_shipment_evidence FOR EACH ROW EXECUTE FUNCTION wms_shipment_source_guard();
CREATE TRIGGER wms_shipment_evidence_immutable BEFORE UPDATE OR DELETE ON wms_shipment_evidence FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_shipment_evidence_no_truncate BEFORE TRUNCATE ON wms_shipment_evidence FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();

CREATE FUNCTION wms_record_handoff_event() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 INSERT INTO wms_shipment_evidence(tenant,warehouse_id,order_id,source_kind,event_id,source_version,document)
 VALUES(NEW.tenant,NEW.warehouse_id,NEW.payload#>>'{order,id}','event',NEW.id,NEW.version,wms_canonical_order(NEW.payload->'order'));
 RETURN NULL;
END $$;
CREATE TRIGGER wms_record_handoff_event AFTER INSERT ON wms_events
 FOR EACH ROW WHEN(NEW.payload->>'type'='fulfillment.shipped') EXECUTE FUNCTION wms_record_handoff_event();

CREATE FUNCTION wms_seed_shipment_evidence(t TEXT,w TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE r RECORD; e RECORD; n BIGINT; legacy_version BIGINT; legacy_order JSONB;
BEGIN
 FOR r IN SELECT o.id FROM wms_orders o WHERE o.tenant=t AND o.warehouse_id=w AND o.status='shipped'
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

-- Require evidence at commit, while freezing existing completed facts before
-- edits. The expensive full comparison runs once when evidence is recorded,
-- including once more at commit; a many-lot shipment does not repeat it per lot.
CREATE FUNCTION wms_require_shipment(t TEXT,w TEXT,o TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM wms_orders WHERE tenant=t AND warehouse_id=w AND id=o AND status='shipped')
  AND NOT EXISTS(SELECT 1 FROM wms_shipment_evidence WHERE tenant=t AND warehouse_id=w AND order_id=o) THEN
  RAISE EXCEPTION 'Completed order requires original shipment evidence' USING ERRCODE='23514';
 END IF;
END $$;
CREATE FUNCTION wms_shipment_commit_proof() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT wms_shipment_consistent(NEW.tenant,NEW.warehouse_id,NEW.order_id) THEN
  RAISE EXCEPTION 'Committed shipment facts or movements changed during recording' USING ERRCODE='23514';
 END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_shipment_commit_proof AFTER INSERT ON wms_shipment_evidence
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_shipment_commit_proof();
CREATE FUNCTION wms_order_shipment_required() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE r RECORD; o TEXT;
BEGIN
 IF TG_OP='DELETE' THEN r=OLD; ELSE r=NEW; END IF;
 IF TG_TABLE_NAME='wms_orders' THEN o=r.id; ELSE o=r.order_id; END IF;
 PERFORM wms_require_shipment(r.tenant,r.warehouse_id,o);
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_order_shipment_required AFTER INSERT OR UPDATE ON wms_orders
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_order_shipment_required();
CREATE CONSTRAINT TRIGGER wms_allocation_shipment_required AFTER INSERT OR UPDATE ON wms_allocations
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_order_shipment_required();
CREATE FUNCTION wms_freeze_shipped_rows() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE r RECORD; o TEXT; frozen BOOLEAN;
BEGIN
 IF TG_OP='DELETE' THEN r=OLD; ELSE r=NEW; END IF;
 PERFORM wms_serialize_shipment(r.tenant,r.warehouse_id);
 IF TG_TABLE_NAME='wms_order_lines' THEN
  IF TG_OP='DELETE' OR (TG_OP='UPDATE' AND NEW IS DISTINCT FROM OLD)
   OR (TG_OP='INSERT' AND EXISTS(SELECT 1 FROM wms_shipment_evidence WHERE tenant=r.tenant AND warehouse_id=r.warehouse_id AND order_id=r.order_id)) THEN
   RAISE EXCEPTION 'Order line identity and expected quantity are immutable' USING ERRCODE='23514';
  END IF;
 ELSE
  IF TG_TABLE_NAME='wms_orders' THEN o=r.id; ELSE o=r.order_id; END IF;
  SELECT EXISTS(SELECT 1 FROM wms_shipment_evidence WHERE tenant=r.tenant AND warehouse_id=r.warehouse_id AND order_id=o) INTO frozen;
  IF TG_OP='UPDATE' THEN
   IF TG_TABLE_NAME='wms_orders' THEN
    frozen=frozen OR EXISTS(SELECT 1 FROM wms_shipment_evidence WHERE tenant=OLD.tenant AND warehouse_id=OLD.warehouse_id AND order_id=OLD.id);
    IF frozen AND ((NEW.tenant,NEW.warehouse_id,NEW.id,NEW.reference,NEW.source_order_id,NEW.status) IS DISTINCT FROM (OLD.tenant,OLD.warehouse_id,OLD.id,OLD.reference,OLD.source_order_id,OLD.status)
     OR wms_order_header(NEW.data) IS DISTINCT FROM wms_order_header(OLD.data)) THEN
     RAISE EXCEPTION 'Committed shipment header is immutable' USING ERRCODE='23514';
    END IF;
   ELSE
    frozen=frozen OR EXISTS(SELECT 1 FROM wms_shipment_evidence WHERE tenant=OLD.tenant AND warehouse_id=OLD.warehouse_id AND order_id=OLD.order_id);
    IF frozen AND NEW IS DISTINCT FROM OLD THEN RAISE EXCEPTION 'Committed shipment allocations are immutable' USING ERRCODE='23514'; END IF;
   END IF;
  ELSIF frozen THEN RAISE EXCEPTION 'Committed shipment ownership cannot be added or removed' USING ERRCODE='23514'; END IF;
 END IF;
 IF TG_OP='DELETE' THEN RETURN OLD; END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_shipped_order_guard BEFORE UPDATE OR DELETE ON wms_orders FOR EACH ROW EXECUTE FUNCTION wms_freeze_shipped_rows();
CREATE TRIGGER wms_order_line_identity_guard BEFORE INSERT OR UPDATE OR DELETE ON wms_order_lines FOR EACH ROW EXECUTE FUNCTION wms_freeze_shipped_rows();
CREATE TRIGGER wms_shipped_allocation_guard BEFORE INSERT OR UPDATE OR DELETE ON wms_allocations FOR EACH ROW EXECUTE FUNCTION wms_freeze_shipped_rows();
CREATE FUNCTION wms_shipment_movement_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 PERFORM wms_serialize_shipment(NEW.tenant,NEW.warehouse_id);
 IF EXISTS(SELECT 1 FROM wms_shipment_evidence e WHERE e.tenant=NEW.tenant AND e.warehouse_id=NEW.warehouse_id AND e.document->>'reference'=NEW.reference) THEN
  RAISE EXCEPTION 'A committed shipment cannot acquire more physical movements' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_shipment_movement_guard BEFORE INSERT ON wms_movements FOR EACH ROW WHEN(NEW.kind='shipment') EXECUTE FUNCTION wms_shipment_movement_guard();
CREATE FUNCTION wms_shipment_movement_required() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT EXISTS(SELECT 1 FROM wms_shipment_evidence e WHERE e.tenant=NEW.tenant AND e.warehouse_id=NEW.warehouse_id AND e.document->>'reference'=NEW.reference) THEN
  RAISE EXCEPTION 'Shipment movement requires its original completed order evidence' USING ERRCODE='23514';
 END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_shipment_movement_required AFTER INSERT ON wms_movements DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
 WHEN(NEW.kind='shipment') EXECUTE FUNCTION wms_shipment_movement_required();

-- Existing relational warehouses must agree with their original source. Legacy
-- warehouses not yet imported are checked after their retained snapshot is saved.
DO $$ DECLARE h RECORD;
BEGIN
 FOR h IN SELECT tenant,warehouse_id FROM wms_warehouses LOOP
  PERFORM wms_seed_shipment_evidence(h.tenant,h.warehouse_id);
 END LOOP;
 IF EXISTS(SELECT 1 FROM wms_events e JOIN wms_warehouses scope ON scope.tenant=e.tenant AND scope.warehouse_id=e.warehouse_id
  WHERE e.payload->>'type'='fulfillment.shipped' AND NOT EXISTS(SELECT 1 FROM wms_shipment_evidence s WHERE s.event_id=e.id)) THEN
  RAISE EXCEPTION 'Original shipment event is missing, ambiguous or disagrees with current ownership' USING ERRCODE='23514';
 END IF;
END $$;

-- Direct SQL authorizations use the same immutable source as the API.
CREATE OR REPLACE FUNCTION wms_check_return_limit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE shipped BIGINT; authorized BIGINT;
BEGIN
 UPDATE wms_warehouses SET version=version WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id;
 IF NOT EXISTS(SELECT 1 FROM wms_orders WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND id=NEW.order_id AND status='shipped')
 OR NOT EXISTS(SELECT 1 FROM wms_receipt_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND receipt_id=NEW.receipt_id AND id=NEW.line_id AND sku=NEW.sku AND lot=NEW.lot AND quantity=NEW.quantity)
 OR EXISTS(SELECT 1 FROM wms_return_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND receipt_id=NEW.receipt_id AND (order_id<>NEW.order_id OR reason<>NEW.reason)) THEN
  RAISE EXCEPTION 'Return source and exact receipt-line ownership disagree' USING ERRCODE='23514';
 END IF;
 IF NOT wms_shipment_consistent(NEW.tenant,NEW.warehouse_id,NEW.order_id) THEN
  RAISE EXCEPTION 'Return requires its consistent immutable original shipment' USING ERRCODE='23514';
 END IF;
 SELECT COALESCE(sum((a->>'shipped')::bigint),0) INTO shipped FROM wms_shipment_evidence e
 CROSS JOIN LATERAL jsonb_array_elements(e.document->'allocations') a
 WHERE e.tenant=NEW.tenant AND e.warehouse_id=NEW.warehouse_id AND e.order_id=NEW.order_id
  AND a->>'line_id'=NEW.order_line_id AND a->>'sku'=NEW.sku AND a->>'lot'=NEW.lot;
 SELECT COALESCE(sum(r.quantity-COALESCE(c.cancelled,0)),0) INTO authorized
 FROM wms_return_lines r LEFT JOIN wms_receipt_cancelled_lines c
 ON c.tenant=r.tenant AND c.warehouse_id=r.warehouse_id AND c.receipt_id=r.receipt_id AND c.line_id=r.line_id
 WHERE r.tenant=NEW.tenant AND r.warehouse_id=NEW.warehouse_id AND r.order_id=NEW.order_id AND r.order_line_id=NEW.order_line_id AND r.lot=NEW.lot;
 IF authorized>shipped THEN RAISE EXCEPTION 'Returns exceed shipped line/lot quantity' USING ERRCODE='23514'; END IF;
 RETURN NULL;
END $$;
