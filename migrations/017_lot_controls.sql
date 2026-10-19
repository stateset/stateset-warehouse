-- Availability is independent of custody. History, not a mutable JSON cache,
-- defines lot policy; legacy actions are never judged using today's clock.
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM wms_products WHERE data ? 'expiry_required' AND data->'expiry_required' NOT IN('false'::jsonb,'null'::jsonb)) THEN
  RAISE EXCEPTION 'Unattributed pre-migration expiry policy requires repair' USING ERRCODE='23514';
 END IF;
END $$;
CREATE TABLE wms_lot_enforcement (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, baseline_version BIGINT NOT NULL CHECK(baseline_version>=0),
 PRIMARY KEY(tenant,warehouse_id), FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
INSERT INTO wms_lot_enforcement SELECT tenant,warehouse_id,version FROM wms_warehouses;
CREATE FUNCTION wms_lot_initialize() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN INSERT INTO wms_lot_enforcement VALUES(NEW.tenant,NEW.warehouse_id,NEW.version); RETURN NEW; END $$;
CREATE TRIGGER wms_lot_initialize AFTER INSERT ON wms_warehouses FOR EACH ROW EXECUTE FUNCTION wms_lot_initialize();
CREATE TABLE wms_lot_changes (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,version BIGINT NOT NULL CHECK(version>0),ordinal INTEGER NOT NULL CHECK(ordinal>=0),
 sku TEXT NOT NULL,lot TEXT,kind TEXT NOT NULL CHECK(kind IN('inventory.expiry_policy_changed','lot.expiry_recorded','lot.expiry_corrected','lot.quarantined','lot.hold_released','lot.arrival_restricted')),
 source_event_id TEXT NOT NULL REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,document JSONB NOT NULL,
 PRIMARY KEY(tenant,warehouse_id,version,ordinal),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku) DEFERRABLE INITIALLY DEFERRED,
 CHECK(jsonb_typeof(document)='object' AND document->>'kind'=kind AND document->>'sku'=sku
 AND (document->>'lot') IS NOT DISTINCT FROM lot)
);
CREATE INDEX wms_lot_history ON wms_lot_changes(tenant,warehouse_id,sku,lot,version DESC);
CREATE UNIQUE INDEX wms_lot_hold_identity ON wms_lot_changes(tenant,warehouse_id,(document->>'hold_id'))
 WHERE kind IN('lot.quarantined','lot.arrival_restricted') AND document->>'hold_id' IS NOT NULL;
CREATE FUNCTION wms_lot_required(t TEXT,w TEXT,s TEXT,v BIGINT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT COALESCE((SELECT (document->>'required')::boolean FROM wms_lot_changes
 WHERE tenant=t AND warehouse_id=w AND sku=s AND kind='inventory.expiry_policy_changed' AND version<=v ORDER BY version DESC,ordinal DESC LIMIT 1),false);
$$;
CREATE FUNCTION wms_lot_document(t TEXT,w TEXT,s TEXT,l TEXT,v BIGINT) RETURNS JSONB LANGUAGE SQL STABLE AS $$
 SELECT jsonb_build_object('sku',s,'lot',l,'expiry_recorded',EXISTS(SELECT 1 FROM wms_lot_changes WHERE tenant=t AND warehouse_id=w AND sku=s AND lot=l AND (kind IN('lot.expiry_recorded','lot.expiry_corrected') OR (kind='lot.arrival_restricted' AND document#>'{arrival,expiry_recorded}'='true'::jsonb)) AND version<=v),
 'expires_at',(SELECT document->'expires_at' FROM wms_lot_changes WHERE tenant=t AND warehouse_id=w AND sku=s AND lot=l AND kind IN('lot.expiry_recorded','lot.expiry_corrected','lot.arrival_restricted') AND version<=v ORDER BY version DESC,ordinal DESC LIMIT 1),
 'holds',COALESCE((SELECT jsonb_agg(jsonb_build_object('id',h.document->'hold_id','reason',h.document->'reason','actor',h.document->'actor','quarantined_at',h.document->'recorded_at') ORDER BY h.version,h.ordinal)
 FROM wms_lot_changes h WHERE h.tenant=t AND h.warehouse_id=w AND h.sku=s AND h.lot=l AND h.kind IN('lot.quarantined','lot.arrival_restricted') AND h.document->>'hold_id' IS NOT NULL AND h.version<=v
 AND NOT EXISTS(SELECT 1 FROM wms_lot_changes r WHERE r.tenant=t AND r.warehouse_id=w AND r.kind='lot.hold_released' AND r.document->>'hold_id'=h.document->>'hold_id' AND r.version<=v)),'[]'::jsonb));
$$;
CREATE FUNCTION wms_lot_available(required BOOLEAN,d JSONB,at_time TIMESTAMPTZ) RETURNS BOOLEAN LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN
 RETURN jsonb_typeof(d->'holds')='array' AND jsonb_array_length(d->'holds')=0
 AND CASE WHEN d->>'expires_at' IS NOT NULL THEN at_time<(d->>'expires_at')::timestamptz ELSE NOT required END;
EXCEPTION WHEN OTHERS THEN RETURN false;
END $$;
CREATE TABLE wms_lot_decisions (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,version BIGINT NOT NULL,kind TEXT NOT NULL CHECK(kind IN('allocation','pick','pack','handoff','transfer_dispatch')),
 sku TEXT NOT NULL,lot TEXT NOT NULL,checked_at TIMESTAMPTZ NOT NULL,required BOOLEAN NOT NULL,document JSONB NOT NULL,
 PRIMARY KEY(tenant,warehouse_id,version,kind,sku,lot),
 FOREIGN KEY(tenant,warehouse_id,version) REFERENCES wms_events(tenant,warehouse_id,version) DEFERRABLE INITIALLY DEFERRED
);
CREATE FUNCTION wms_lot_lock_key(t TEXT,w TEXT,s TEXT) RETURNS BIGINT LANGUAGE SQL IMMUTABLE AS $$
 SELECT hashtextextended(jsonb_build_array('wms-lot',t,w,s)::text,816421);
$$;
CREATE FUNCTION wms_lot_lock() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 UPDATE wms_warehouses SET version=version WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id;
 PERFORM pg_advisory_xact_lock(wms_lot_lock_key(NEW.tenant,NEW.warehouse_id,NEW.sku));
 RETURN NEW;
END $$;
CREATE TRIGGER wms_lot_change_lock BEFORE INSERT ON wms_lot_changes FOR EACH ROW EXECUTE FUNCTION wms_lot_lock();
CREATE FUNCTION wms_lot_change_valid(t TEXT,w TEXT,v BIGINT,n INTEGER) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE h RECORD; e RECORD; d JSONB; c JSONB; prior JSONB; a JSONB; transfer RECORD; frozen JSONB; source JSONB; cutoff TIMESTAMPTZ; conflicting BOOLEAN; held BOOLEAN;
BEGIN
 SELECT * INTO h FROM wms_lot_changes WHERE tenant=t AND warehouse_id=w AND version=v AND ordinal=n;
 IF NOT FOUND THEN RETURN false; END IF;
 d=h.document;
 IF d IS DISTINCT FROM (jsonb_build_object('kind',h.kind,'sku',h.sku,'lot',h.lot,'required',d->'required','expires_at',d->'expires_at','hold_id',d->'hold_id','reason',d->'reason','actor',d->'actor','recorded_at',d->'recorded_at') || CASE WHEN h.kind='lot.arrival_restricted' THEN jsonb_build_object('arrival',d->'arrival') ELSE '{}'::jsonb END)
 OR (h.kind<>'inventory.expiry_policy_changed' AND d->'required' IS DISTINCT FROM 'null'::jsonb)
 OR (h.kind IN('inventory.expiry_policy_changed','lot.quarantined','lot.hold_released') AND d->'expires_at' IS DISTINCT FROM 'null'::jsonb)
 OR (h.kind IN('inventory.expiry_policy_changed','lot.expiry_recorded','lot.expiry_corrected') AND d->'hold_id' IS DISTINCT FROM 'null'::jsonb)
 THEN RETURN false; END IF;
 SELECT * INTO e FROM wms_events WHERE id=h.source_event_id AND tenant=t AND warehouse_id=w AND version=v;
 IF NOT FOUND OR e.payload->>'id' IS DISTINCT FROM e.id OR e.payload->>'tenant' IS DISTINCT FROM t OR e.payload->>'warehouse_id' IS DISTINCT FROM w
 OR e.payload->'version' IS DISTINCT FROM to_jsonb(v) OR e.payload->'lot_changes'->n IS DISTINCT FROM d
 OR e.payload#>'{result,version}' IS DISTINCT FROM to_jsonb(v)
 OR e.payload->>'type' IS DISTINCT FROM (CASE WHEN h.kind='lot.arrival_restricted' THEN 'transfer.received' ELSE h.kind END)
 OR e.payload->>'actor' IS DISTINCT FROM d->>'actor' OR e.payload->>'occurred_at' IS DISTINCT FROM d->>'recorded_at'
 OR NOT EXISTS(SELECT 1 FROM wms_commands q WHERE q.tenant=t AND q.warehouse_id=w AND q.request_id=e.payload->>'request_id' AND q.actor=d->>'actor' AND q.result=e.payload->'result')
 OR v<=(SELECT baseline_version FROM wms_lot_enforcement WHERE tenant=t AND warehouse_id=w)
 OR length(btrim(d->>'reason')) NOT BETWEEN 1 AND 240 OR length(btrim(d->>'actor')) NOT BETWEEN 1 AND 240
 OR d->>'reason' IS DISTINCT FROM btrim(d->>'reason') OR d->>'actor' IS DISTINCT FROM btrim(d->>'actor')
 OR d->>'reason' ~ '[[:cntrl:]]' OR d->>'actor' ~ '[[:cntrl:]]'
 OR (d->>'recorded_at')::timestamptz IS NULL THEN RETURN false; END IF;
 c=e.payload->'command';
 prior=wms_lot_document(t,w,h.sku,h.lot,v-1);
 IF h.kind='lot.arrival_restricted' THEN
  a=d->'arrival';
  SELECT l.*,r.source_version AS dispatched_version INTO transfer FROM wms_transfer_lines l JOIN wms_transfers r ON r.tenant=l.tenant AND r.id=l.transfer_id
    WHERE l.tenant=t AND l.transfer_id=a->>'transfer_id' AND l.id=a->>'line_id' AND l.destination_warehouse_id=w AND l.sku=h.sku AND l.lot=h.lot;
  IF NOT FOUND OR a->>'source_warehouse_id' IS DISTINCT FROM transfer.source_warehouse_id
   OR (a->>'source_version')::bigint<transfer.dispatched_version
   OR (a->>'source_version')::bigint>(SELECT version FROM wms_warehouses WHERE tenant=t AND warehouse_id=transfer.source_warehouse_id)
   OR EXISTS(SELECT 1 FROM wms_lot_changes p WHERE p.tenant=t AND p.warehouse_id=transfer.source_warehouse_id AND p.sku=h.sku AND p.version<=(a->>'source_version')::bigint AND (p.document->>'recorded_at')::timestamptz>(d->>'recorded_at')::timestamptz)
   OR c->>'type' IS DISTINCT FROM 'receive_transfer' OR c->>'transfer_id' IS DISTINCT FROM transfer.transfer_id OR c->>'line_id' IS DISTINCT FROM transfer.id
   OR e.payload#>>'{result,entity_id}' IS DISTINCT FROM transfer.transfer_id
   OR d->>'reason' IS DISTINCT FROM 'Transfer arrival retains original and current lot restrictions'
   THEN RETURN false; END IF;
  source=wms_lot_document(t,transfer.source_warehouse_id,h.sku,h.lot,(a->>'source_version')::bigint);
  SELECT document INTO frozen FROM wms_lot_decisions WHERE tenant=t AND warehouse_id=transfer.source_warehouse_id AND version=transfer.dispatched_version AND kind='transfer_dispatch' AND sku=h.sku AND lot=h.lot;
  IF NOT COALESCE(wms_lot_dispatch_valid(t,transfer.source_warehouse_id,transfer.dispatched_version,h.sku,h.lot),false) THEN RETURN false; END IF;
  IF a->'source' IS DISTINCT FROM source OR a->'dispatched' IS DISTINCT FROM COALESCE(frozen,'null'::jsonb)
   OR a->'source_required' IS DISTINCT FROM to_jsonb(wms_lot_required(t,transfer.source_warehouse_id,h.sku,(a->>'source_version')::bigint)) THEN RETURN false; END IF;
  SELECT min((item->>'expires_at')::timestamptz),count(DISTINCT (item->>'expires_at')::timestamptz)>1 INTO cutoff,conflicting FROM (VALUES(prior),(source),(frozen)) q(item);
  held=conflicting OR jsonb_array_length(source->'holds')>0 OR ((a->>'source_required')::boolean AND source->>'expires_at' IS NULL);
  RETURN (d->>'expires_at')::timestamptz IS NOT DISTINCT FROM cutoff
   AND (d->>'hold_id' IS NOT NULL)=held
   AND (NOT held OR length(d->>'hold_id') BETWEEN 1 AND 240)
   AND a->'expiry_recorded'=to_jsonb((prior->>'expiry_recorded')::boolean OR (source->>'expiry_recorded')::boolean OR COALESCE((frozen->>'expiry_recorded')::boolean,false));
 ELSIF h.kind='inventory.expiry_policy_changed' THEN
  RETURN h.lot IS NULL AND jsonb_typeof(d->'required')='boolean' AND d->'required' IS DISTINCT FROM to_jsonb(wms_lot_required(t,w,h.sku,v-1))
   AND c=jsonb_build_object('type','set_product_expiry_policy','sku',h.sku,'required',d->'required','reason',d->'reason') AND e.payload#>>'{result,entity_id}'=h.sku;
 ELSIF h.kind IN('lot.expiry_recorded','lot.expiry_corrected') THEN
  IF h.lot IS NULL OR length(btrim(h.lot)) NOT BETWEEN 1 AND 240 THEN RETURN false; END IF;
  IF d->>'expires_at' IS NOT NULL THEN
   IF d->>'expires_at' !~ '^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,6})?\+00:00$' OR (d->>'expires_at')::timestamptz IS NULL THEN RETURN false; END IF;
  END IF;
  RETURN (CASE h.kind WHEN 'lot.expiry_recorded' THEN prior->'expiry_recorded'='false'::jsonb AND d->>'expires_at' IS NOT NULL ELSE prior->'expiry_recorded'='true'::jsonb AND prior->'expires_at' IS DISTINCT FROM d->'expires_at' END)
   AND c->>'type'=CASE h.kind WHEN 'lot.expiry_recorded' THEN 'record_lot_expiry' ELSE 'correct_lot_expiry' END
   AND c->>'sku'=h.sku AND c->>'lot'=h.lot AND c->'reason'=d->'reason'
   AND (CASE WHEN c->>'expires_at' IS NULL THEN d->>'expires_at' IS NULL ELSE (c->>'expires_at')::timestamptz=(d->>'expires_at')::timestamptz END)
   AND e.payload#>>'{result,entity_id}'=h.lot;
 ELSIF h.kind='lot.quarantined' THEN
  RETURN h.lot IS NOT NULL AND length(btrim(h.lot)) BETWEEN 1 AND 240 AND length(d->>'hold_id') BETWEEN 1 AND 240
   AND NOT EXISTS(SELECT 1 FROM wms_lot_changes p WHERE p.tenant=t AND p.warehouse_id=w AND p.kind='lot.quarantined' AND p.document->>'hold_id'=d->>'hold_id' AND (p.version,p.ordinal)<(v,n))
   AND c=jsonb_build_object('type','quarantine_lot','sku',h.sku,'lot',h.lot,'reason',d->'reason') AND e.payload#>>'{result,entity_id}'=d->>'hold_id';
 ELSIF h.kind='lot.hold_released' THEN
  RETURN EXISTS(SELECT 1 FROM jsonb_array_elements(prior->'holds') hold WHERE hold->>'id'=d->>'hold_id')
   AND c=jsonb_build_object('type','release_lot_hold','hold_id',d->'hold_id','reason',d->'reason') AND e.payload#>>'{result,entity_id}'=d->>'hold_id';
 END IF;
 RETURN false;
EXCEPTION WHEN OTHERS THEN RETURN false;
END $$;
CREATE FUNCTION wms_lot_decision_document(k TEXT,s TEXT,l TEXT,at_time TIMESTAMPTZ,r BOOLEAN,d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT jsonb_build_object('kind',k,'sku',s,'lot',l,'checked_at',to_char(at_time AT TIME ZONE 'UTC','YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),'required',r,'document',d);
$$;
CREATE FUNCTION wms_lot_decision_valid(t TEXT,w TEXT,v BIGINT,k TEXT,s TEXT,l TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE h RECORD; e RECORD;
BEGIN
 SELECT * INTO h FROM wms_lot_decisions WHERE tenant=t AND warehouse_id=w AND version=v AND kind=k AND sku=s AND lot=l;
 IF NOT FOUND OR h.document IS DISTINCT FROM wms_lot_document(t,w,s,l,v) OR h.required IS DISTINCT FROM wms_lot_required(t,w,s,v)
 OR NOT COALESCE(wms_lot_available(h.required,h.document,h.checked_at),false) THEN RETURN false; END IF;
 SELECT * INTO e FROM wms_events WHERE tenant=t AND warehouse_id=w AND version=v;
 IF NOT FOUND OR e.payload->>'id' IS DISTINCT FROM e.id OR e.payload->>'tenant' IS DISTINCT FROM t OR e.payload->>'warehouse_id' IS DISTINCT FROM w
 OR e.payload->'version' IS DISTINCT FROM to_jsonb(v) OR e.payload#>'{result,version}' IS DISTINCT FROM to_jsonb(v)
 OR e.payload->>'type' IS DISTINCT FROM (CASE e.payload#>>'{command,type}'
 WHEN 'allocate' THEN 'fulfillment.allocated' WHEN 'allocate_available' THEN 'fulfillment.available_allocated'
 WHEN 'pick' THEN 'fulfillment.picked' WHEN 'pack' THEN 'fulfillment.packed' WHEN 'pack_shipment' THEN 'shipment.packed'
 WHEN 'ship' THEN 'fulfillment.shipped' WHEN 'handoff_shipment' THEN 'shipment.handed_off'
 WHEN 'dispatch_transfer' THEN 'transfer.dispatched' ELSE 'invalid' END)
 OR NOT EXISTS(SELECT 1 FROM jsonb_array_elements(e.payload->'lot_decisions') d
 WHERE d=wms_lot_decision_document(h.kind,s,l,h.checked_at,h.required,h.document)) THEN RETURN false; END IF;
 RETURN CASE k WHEN 'allocation' THEN e.payload#>>'{command,type}' IN('allocate','allocate_available')
 WHEN 'pick' THEN e.payload#>>'{command,type}'='pick' WHEN 'pack' THEN e.payload#>>'{command,type}' IN('pack','pack_shipment')
 WHEN 'handoff' THEN e.payload#>>'{command,type}' IN('ship','handoff_shipment')
 WHEN 'transfer_dispatch' THEN e.payload#>>'{command,type}'='dispatch_transfer' ELSE false END
 AND EXISTS(SELECT 1 FROM wms_commands c WHERE c.tenant=t AND c.warehouse_id=w AND c.request_id=e.payload->>'request_id' AND c.actor=e.payload->>'actor' AND c.result=e.payload->'result');
EXCEPTION WHEN OTHERS THEN RETURN false;
END $$;
CREATE FUNCTION wms_lot_require(t TEXT,w TEXT,s TEXT,l TEXT,k TEXT) RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE v BIGINT; at_time TIMESTAMPTZ; d JSONB; r BOOLEAN;
BEGIN
 SELECT version INTO v FROM wms_warehouses WHERE tenant=t AND warehouse_id=w FOR UPDATE;
 PERFORM pg_advisory_xact_lock(wms_lot_lock_key(t,w,s));
 at_time=clock_timestamp(); d=wms_lot_document(t,w,s,l,v); r=wms_lot_required(t,w,s,v);
 IF NOT COALESCE(wms_lot_available(r,d,at_time),false) THEN
  RAISE EXCEPTION 'Lot unavailable for new fulfillment: % / %',s,l USING ERRCODE='23514';
 END IF;
 IF v<=(SELECT baseline_version FROM wms_lot_enforcement WHERE tenant=t AND warehouse_id=w) THEN RETURN; END IF;
 INSERT INTO wms_lot_decisions VALUES(t,w,v,k,s,l,at_time,r,d) ON CONFLICT DO NOTHING;
END $$;
CREATE FUNCTION wms_lot_forward_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE line RECORD;
BEGIN
 IF TG_TABLE_NAME='wms_allocations' THEN
  IF TG_OP='INSERT' THEN
   IF NEW.quantity>0 THEN PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,'allocation'); END IF;
   IF NEW.picked>0 THEN PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,'pick'); END IF;
   IF NEW.shipped>0 THEN PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,'handoff'); END IF;
  ELSE
   IF NEW.quantity>OLD.quantity THEN PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,'allocation'); END IF;
   IF NEW.picked>OLD.picked THEN PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,'pick'); END IF;
   IF NEW.shipped>OLD.shipped THEN PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,'handoff'); END IF;
  END IF;
 ELSIF TG_TABLE_NAME='wms_orders' THEN
  IF NEW.status='packed' AND (TG_OP='INSERT' OR OLD.status IS DISTINCT FROM NEW.status) THEN
  FOR line IN SELECT DISTINCT sku,lot FROM wms_allocations WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND order_id=NEW.id LOOP
   PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,line.sku,line.lot,'pack'); END LOOP;
  END IF;
 ELSIF TG_TABLE_NAME='wms_package_lines' THEN
  PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,'pack');
 ELSIF TG_TABLE_NAME='wms_package_closures' THEN
  IF NEW.kind='handed_off' THEN
  FOR line IN SELECT DISTINCT sku,lot FROM wms_package_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND package_id=NEW.package_id LOOP
   PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,line.sku,line.lot,'handoff'); END LOOP;
  END IF;
 ELSIF TG_TABLE_NAME='wms_movements' THEN
  IF NEW.kind='transfer_dispatch' THEN
  PERFORM wms_lot_require(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,'transfer_dispatch');
  END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_lot_allocation_guard AFTER INSERT OR UPDATE ON wms_allocations FOR EACH ROW EXECUTE FUNCTION wms_lot_forward_guard();
CREATE TRIGGER wms_lot_pack_guard AFTER INSERT OR UPDATE ON wms_orders FOR EACH ROW EXECUTE FUNCTION wms_lot_forward_guard();
CREATE TRIGGER wms_lot_package_guard AFTER INSERT ON wms_package_lines FOR EACH ROW EXECUTE FUNCTION wms_lot_forward_guard();
CREATE TRIGGER wms_lot_handoff_guard AFTER INSERT ON wms_package_closures FOR EACH ROW EXECUTE FUNCTION wms_lot_forward_guard();
CREATE TRIGGER wms_lot_transfer_guard AFTER INSERT ON wms_movements FOR EACH ROW EXECUTE FUNCTION wms_lot_forward_guard();
CREATE FUNCTION wms_lot_proof_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_TABLE_NAME='wms_lot_changes' THEN
  IF NOT COALESCE(wms_lot_change_valid(NEW.tenant,NEW.warehouse_id,NEW.version,NEW.ordinal),false) THEN RAISE EXCEPTION 'Original lot policy source disagrees' USING ERRCODE='23514'; END IF;
 ELSIF TG_TABLE_NAME='wms_lot_decisions' THEN
  IF NOT COALESCE(wms_lot_decision_valid(NEW.tenant,NEW.warehouse_id,NEW.version,NEW.kind,NEW.sku,NEW.lot),false) THEN RAISE EXCEPTION 'Original lot eligibility source disagrees' USING ERRCODE='23514'; END IF;
 ELSE
  IF EXISTS(SELECT 1 FROM wms_products p WHERE p.tenant=NEW.tenant AND p.warehouse_id=NEW.warehouse_id
   AND (p.data->'expiry_required' IS NOT NULL AND jsonb_typeof(p.data->'expiry_required')<>'boolean'
    OR COALESCE((p.data->>'expiry_required')::boolean,false) IS DISTINCT FROM wms_lot_required(p.tenant,p.warehouse_id,p.sku,9223372036854775807))) THEN
   RAISE EXCEPTION 'Product expiry requirement needs original policy evidence' USING ERRCODE='23514'; END IF;
 END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_lot_change_proof AFTER INSERT ON wms_lot_changes DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_lot_proof_guard();
CREATE CONSTRAINT TRIGGER wms_lot_decision_proof AFTER INSERT ON wms_lot_decisions DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_lot_proof_guard();
CREATE CONSTRAINT TRIGGER wms_lot_product_proof AFTER INSERT OR UPDATE ON wms_products DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_lot_proof_guard();
CREATE FUNCTION wms_lot_track() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 PERFORM wms_bump_resource(NEW.tenant,NEW.warehouse_id,jsonb_build_array('inventory',NEW.sku));
 IF NEW.lot IS NOT NULL THEN PERFORM wms_bump_resource(NEW.tenant,NEW.warehouse_id,jsonb_build_array('lot',NEW.sku,NEW.lot)); END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_lot_revision AFTER INSERT ON wms_lot_changes FOR EACH ROW EXECUTE FUNCTION wms_lot_track();
DO $$ DECLARE n TEXT; BEGIN
 FOREACH n IN ARRAY ARRAY['wms_lot_enforcement','wms_lot_changes','wms_lot_decisions'] LOOP
 EXECUTE format('CREATE TRIGGER %I BEFORE UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION wms_immutable_history()',n||'_immutable',n);
 EXECUTE format('CREATE TRIGGER %I BEFORE TRUNCATE ON %I FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history()',n||'_no_truncate',n);
 END LOOP;
END $$;

-- The event independently enumerates every required decision. In particular,
-- deleting/omitting an audit row cannot turn an unavailable action into history.
CREATE FUNCTION wms_lot_targets(e JSONB) RETURNS TABLE(kind TEXT,sku TEXT,lot TEXT) LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE command TEXT; field TEXT; decision TEXT;
BEGIN
 command=e#>>'{command,type}';
 IF command IN('allocate','allocate_available','pick','ship') THEN
  field=CASE command WHEN 'pick' THEN 'picked' WHEN 'ship' THEN 'shipped' ELSE 'quantity' END;
  decision=CASE command WHEN 'pick' THEN 'pick' WHEN 'ship' THEN 'handoff' ELSE 'allocation' END;
  RETURN QUERY SELECT DISTINCT decision,a->>'sku',a->>'lot'
   FROM jsonb_array_elements(e#>'{order,allocations}') a
   LEFT JOIN LATERAL (SELECT old FROM jsonb_array_elements(COALESCE(e#>'{previous_order,allocations}','[]'::jsonb)) old
    WHERE old->'line_id'=a->'line_id' AND old->'location'=a->'location' AND old->'lot'=a->'lot') p ON true
   WHERE (a->>field)::bigint>COALESCE((p.old->>field)::bigint,0);
 ELSIF command='pack' THEN
  RETURN QUERY SELECT DISTINCT 'pack'::text,a->>'sku',a->>'lot' FROM jsonb_array_elements(e#>'{order,allocations}') a;
 ELSIF command IN('pack_shipment','handoff_shipment') THEN
  decision=CASE command WHEN 'pack_shipment' THEN 'pack' ELSE 'handoff' END;
  RETURN QUERY SELECT DISTINCT decision,a->>'sku',a->>'lot' FROM jsonb_array_elements(e#>'{shipment,lines}') a;
 ELSIF command='dispatch_transfer' THEN
  RETURN QUERY SELECT DISTINCT 'transfer_dispatch'::text,a->>'sku',a->>'lot' FROM jsonb_array_elements(e#>'{command,lines}') a;
 END IF;
END $$;
CREATE FUNCTION wms_lot_event_valid(t TEXT,w TEXT,v BIGINT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE e RECORD; changes JSONB; decisions JSONB; command TEXT;
BEGIN
 IF v<=(SELECT baseline_version FROM wms_lot_enforcement WHERE tenant=t AND warehouse_id=w) THEN RETURN true; END IF;
 SELECT * INTO e FROM wms_events WHERE tenant=t AND warehouse_id=w AND version=v;
 IF NOT FOUND THEN RETURN false; END IF;
 SELECT COALESCE(jsonb_agg(document ORDER BY ordinal),'[]'::jsonb) INTO changes FROM wms_lot_changes WHERE tenant=t AND warehouse_id=w AND version=v;
 SELECT COALESCE(jsonb_agg(wms_lot_decision_document(kind,sku,lot,checked_at,required,document) ORDER BY kind,sku,lot),'[]'::jsonb) INTO decisions FROM wms_lot_decisions WHERE tenant=t AND warehouse_id=w AND version=v;
 IF e.payload->'lot_changes' IS DISTINCT FROM changes OR e.payload->'lot_decisions' IS DISTINCT FROM decisions THEN RETURN false; END IF;
 command=e.payload#>>'{command,type}';
 IF command IN('set_product_expiry_policy','record_lot_expiry','correct_lot_expiry','quarantine_lot','release_lot_hold','receive_transfer') THEN
  IF jsonb_array_length(changes)<>1 THEN RETURN false; END IF;
 ELSIF jsonb_array_length(changes)<>0 THEN RETURN false; END IF;
 RETURN NOT EXISTS((SELECT kind,sku,lot FROM wms_lot_targets(e.payload)) EXCEPT (SELECT kind,sku,lot FROM wms_lot_decisions WHERE tenant=t AND warehouse_id=w AND version=v))
 AND NOT EXISTS((SELECT kind,sku,lot FROM wms_lot_decisions WHERE tenant=t AND warehouse_id=w AND version=v) EXCEPT (SELECT kind,sku,lot FROM wms_lot_targets(e.payload)))
 AND NOT EXISTS(SELECT 1 FROM wms_lot_decisions WHERE tenant=t AND warehouse_id=w AND version=v AND checked_at<(e.payload->>'occurred_at')::timestamptz);
EXCEPTION WHEN OTHERS THEN RETURN false;
END $$;
CREATE FUNCTION wms_lot_event_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NOT COALESCE(wms_lot_event_valid(NEW.tenant,NEW.warehouse_id,NEW.version),false) THEN
  RAISE EXCEPTION 'Warehouse event requires every original lot policy and eligibility decision' USING ERRCODE='23514';
 END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_lot_event_proof AFTER INSERT ON wms_events DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_lot_event_guard();

-- Old physical transfers retain an explicit unknown baseline. New ones must
-- retain the original eligible dispatch, never silently downgrade to unknown.
CREATE FUNCTION wms_lot_dispatch_valid(t TEXT,w TEXT,v BIGINT,s TEXT,l TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT CASE WHEN v<=(SELECT baseline_version FROM wms_lot_enforcement WHERE tenant=t AND warehouse_id=w) THEN true
 ELSE COALESCE(wms_lot_event_valid(t,w,v),false) AND COALESCE(wms_lot_decision_valid(t,w,v,'transfer_dispatch',s,l),false) END;
$$;

CREATE INDEX wms_lot_order_history ON wms_events(tenant,warehouse_id,(payload#>>'{order,id}')) WHERE payload#>>'{order,id}' IS NOT NULL;
CREATE FUNCTION wms_lot_order_valid(t TEXT,w TEXT,o TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT NOT EXISTS(SELECT 1 FROM wms_events e WHERE e.tenant=t AND e.warehouse_id=w AND e.payload#>>'{order,id}'=o
 AND (NOT COALESCE(wms_lot_event_valid(t,w,e.version),false) OR EXISTS(SELECT 1 FROM wms_lot_decisions d
 WHERE d.tenant=t AND d.warehouse_id=w AND d.version=e.version AND NOT COALESCE(wms_lot_decision_valid(t,w,d.version,d.kind,d.sku,d.lot),false))));
$$;
