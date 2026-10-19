-- Serial identity and original observation evidence. Application admission must
-- commit these rows with the physical event, actor receipt and final outbox.
-- Operator, restore and workload qualification remain separate requirements.
-- Existing rows remain untracked; no historical serial identity is invented.
CREATE FUNCTION wms_serial_text(s TEXT) RETURNS BOOLEAN LANGUAGE SQL IMMUTABLE AS $$
 SELECT COALESCE(octet_length(s) BETWEEN 1 AND 240
  AND s=btrim(s,U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')
  AND s !~ '[[:cntrl:]]' AND s !~ U&'[\007F-\009F]',false);
$$;

CREATE FUNCTION wms_serial_place_valid(p JSONB) RETURNS BOOLEAN LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE required TEXT[]; k TEXT;
BEGIN
 IF jsonb_typeof(p) IS DISTINCT FROM 'object' THEN RETURN false; END IF;
 CASE p->>'status'
 WHEN 'on_hand','missing' THEN required=ARRAY['status','warehouse_id','location'];
 WHEN 'staged' THEN required=ARRAY['status','warehouse_id','order_id','line_id','allocation_location','package_id'];
 WHEN 'shipped' THEN required=ARRAY['status','warehouse_id','order_id','line_id','package_id','handoff_event_id','return_receipt_id'];
 WHEN 'in_transit' THEN required=ARRAY['status','source_warehouse_id','destination_warehouse_id','transfer_id','line_id'];
 WHEN 'lost' THEN required=ARRAY['status','source_warehouse_id','destination_warehouse_id','transfer_id','line_id','loss_id'];
 ELSE RETURN false;
 END CASE;
 IF NOT p ?& required OR p-required<>'{}'::jsonb THEN RETURN false; END IF;
 FOREACH k IN ARRAY required LOOP
  IF k IN('package_id','return_receipt_id') AND p->k='null'::jsonb THEN CONTINUE; END IF;
  IF jsonb_typeof(p->k) IS DISTINCT FROM 'string' OR NOT wms_serial_text(p->>k) THEN RETURN false; END IF;
 END LOOP;
 RETURN NOT (p->>'status' IN('on_hand','missing') AND p->>'location'='STAGE-01')
  AND NOT (p->>'status' IN('in_transit','lost') AND p->>'source_warehouse_id'=p->>'destination_warehouse_id');
END $$;

DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM wms_products WHERE COALESCE(data->'serial_controlled','false'::jsonb)<>'false'::jsonb) THEN
  RAISE EXCEPTION 'Existing attributed/unattributed serial metadata needs review before serial migration' USING ERRCODE='23514';
 END IF;
END $$;

CREATE TABLE wms_serial_modes (
 tenant TEXT NOT NULL,sku TEXT NOT NULL,controlled BOOLEAN NOT NULL,
 source_warehouse_id TEXT,source_event_id TEXT REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 PRIMARY KEY(tenant,sku),
 CHECK((NOT controlled AND source_warehouse_id IS NULL AND source_event_id IS NULL)
    OR (controlled AND source_warehouse_id IS NOT NULL AND source_event_id IS NOT NULL)),
 FOREIGN KEY(tenant,source_warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE INDEX wms_serial_controlled_catalog ON wms_serial_modes(tenant,sku) WHERE controlled;
INSERT INTO wms_serial_modes(tenant,sku,controlled)
 SELECT DISTINCT tenant,sku,false FROM wms_products;
ALTER TABLE wms_products ADD FOREIGN KEY(tenant,sku) REFERENCES wms_serial_modes(tenant,sku);

CREATE TABLE wms_serial_heads (
 tenant TEXT NOT NULL,sku TEXT NOT NULL,version BIGINT NOT NULL CHECK(version>=0),
 last_event_id TEXT REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 PRIMARY KEY(tenant,sku),FOREIGN KEY(tenant,sku) REFERENCES wms_serial_modes(tenant,sku),
 CHECK((version=0 AND last_event_id IS NULL) OR (version>0 AND last_event_id IS NOT NULL))
);
CREATE TABLE wms_serial_batches (
 tenant TEXT NOT NULL,sku TEXT NOT NULL,version BIGINT NOT NULL CHECK(version>0),
 warehouse_id TEXT NOT NULL,warehouse_version BIGINT NOT NULL CHECK(warehouse_version>0),
 event_id TEXT NOT NULL REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 PRIMARY KEY(tenant,sku,version),UNIQUE(tenant,sku,event_id),
 FOREIGN KEY(tenant,sku) REFERENCES wms_serial_heads(tenant,sku),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE TABLE wms_serial_units (
 tenant TEXT NOT NULL,sku TEXT NOT NULL,serial TEXT COLLATE "C" NOT NULL CHECK(wms_serial_text(serial)),
 lot TEXT NOT NULL CHECK(wms_serial_text(lot)),revision BIGINT NOT NULL CHECK(revision>0),
 place JSONB NOT NULL CHECK(wms_serial_place_valid(place)),
 last_event_id TEXT NOT NULL REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 PRIMARY KEY(tenant,sku,serial),UNIQUE(tenant,sku,serial,lot),
 FOREIGN KEY(tenant,sku) REFERENCES wms_serial_modes(tenant,sku)
);
CREATE INDEX wms_serial_physical ON wms_serial_units(tenant,(place->>'warehouse_id'),sku,lot,(place->>'location')) WHERE place->>'status'='on_hand';
CREATE INDEX wms_serial_staged ON wms_serial_units(tenant,(place->>'warehouse_id'),(place->>'order_id'),(place->>'line_id'),(place->>'allocation_location'),sku,lot) WHERE place->>'status'='staged';
CREATE INDEX wms_serial_package ON wms_serial_units(tenant,(place->>'warehouse_id'),(place->>'package_id'),(place->>'line_id'),(place->>'allocation_location'),sku,lot) WHERE place->>'status'='staged' AND place->>'package_id' IS NOT NULL;
CREATE INDEX wms_serial_return ON wms_serial_units(tenant,(place->>'warehouse_id'),(place->>'return_receipt_id')) WHERE place->>'status'='shipped' AND place->>'return_receipt_id' IS NOT NULL;
CREATE INDEX wms_serial_transit ON wms_serial_units(tenant,(place->>'transfer_id'),(place->>'line_id'),(place->>'status')) WHERE place->>'status' IN('in_transit','lost');

CREATE TABLE wms_serial_steps (
 tenant TEXT NOT NULL,sku TEXT NOT NULL,serial TEXT COLLATE "C" NOT NULL,lot TEXT NOT NULL,
 revision BIGINT NOT NULL CHECK(revision>0),previous_revision BIGINT,
 sku_version BIGINT NOT NULL,warehouse_id TEXT NOT NULL,warehouse_version BIGINT NOT NULL CHECK(warehouse_version>0),
 event_id TEXT NOT NULL REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 10000),target JSONB NOT NULL,
 before_place JSONB,after_place JSONB NOT NULL CHECK(wms_serial_place_valid(after_place)),
 actor TEXT NOT NULL CHECK(wms_serial_text(actor)),recorded_at TEXT NOT NULL CHECK(wms_serial_text(recorded_at)),
 PRIMARY KEY(tenant,sku,serial,revision),UNIQUE(tenant,event_id,ordinal),UNIQUE(tenant,event_id,sku,serial),
 CHECK((revision=1 AND previous_revision IS NULL AND before_place IS NULL)
   OR (revision>1 AND previous_revision=revision-1 AND before_place IS NOT NULL AND wms_serial_place_valid(before_place))),
 CHECK(before_place IS DISTINCT FROM after_place),
 CHECK(jsonb_typeof(target)='array' AND jsonb_array_length(target)<=3),
 FOREIGN KEY(tenant,sku,serial,lot) REFERENCES wms_serial_units(tenant,sku,serial,lot) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(tenant,sku,serial,previous_revision) REFERENCES wms_serial_steps(tenant,sku,serial,revision) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(tenant,sku,sku_version) REFERENCES wms_serial_batches(tenant,sku,version) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
ALTER TABLE wms_serial_units ADD FOREIGN KEY(tenant,sku,serial,revision) REFERENCES wms_serial_steps(tenant,sku,serial,revision) DEFERRABLE INITIALLY DEFERRED;
CREATE INDEX wms_serial_lineage ON wms_serial_steps(tenant,sku,serial,sku_version DESC);
CREATE INDEX wms_serial_steps_event ON wms_serial_steps(tenant,warehouse_id,event_id,ordinal);

CREATE TABLE wms_serial_observations (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,event_id TEXT NOT NULL REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 100),sku TEXT NOT NULL,sku_version BIGINT NOT NULL,
 target JSONB NOT NULL CHECK(jsonb_typeof(target)='array' AND jsonb_array_length(target)<=3),
 serials JSONB NOT NULL CHECK(jsonb_typeof(serials)='array' AND jsonb_array_length(serials)<=10000),
 PRIMARY KEY(tenant,warehouse_id,event_id,ordinal),UNIQUE(tenant,warehouse_id,event_id,target),
 FOREIGN KEY(tenant,sku,sku_version) REFERENCES wms_serial_batches(tenant,sku,version) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE INDEX wms_serial_observation_identity ON wms_serial_observations USING GIN(serials);
CREATE INDEX wms_serial_observation_scope ON wms_serial_observations(tenant,sku,sku_version,ordinal);

CREATE FUNCTION wms_serial_mode_immutable() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' OR NEW IS DISTINCT FROM OLD THEN
  RAISE EXCEPTION 'Serial mode is chosen before stock and cannot reinterpret existing SKU identity' USING ERRCODE='23514';
 END IF;
 -- An exact no-op update is permitted solely to fence concurrent catalog
 -- creation under repeatable-read snapshots; business fields remain immutable.
 RETURN NEW;
END $$;
CREATE TRIGGER wms_serial_mode_immutable BEFORE UPDATE OR DELETE ON wms_serial_modes FOR EACH ROW EXECUTE FUNCTION wms_serial_mode_immutable();
CREATE TRIGGER wms_serial_mode_no_truncate BEFORE TRUNCATE ON wms_serial_modes FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE FUNCTION wms_serial_product_mode() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actual BOOLEAN; requested BOOLEAN;
BEGIN
 IF COALESCE(jsonb_typeof(NEW.data->'serial_controlled'),'boolean')<>'boolean' THEN
  RAISE EXCEPTION 'Serial mode requires an actual boolean' USING ERRCODE='23514';
 END IF;
 requested=COALESCE((NEW.data->>'serial_controlled')::boolean,false);
 IF TG_OP='UPDATE' AND (NEW.tenant,NEW.warehouse_id,NEW.sku,requested)
  IS DISTINCT FROM (OLD.tenant,OLD.warehouse_id,OLD.sku,COALESCE((OLD.data->>'serial_controlled')::boolean,false)) THEN
  RAISE EXCEPTION 'Product serial identity and mode cannot change after creation' USING ERRCODE='23514';
 END IF;
 INSERT INTO wms_serial_modes(tenant,sku,controlled) VALUES(NEW.tenant,NEW.sku,false)
  ON CONFLICT(tenant,sku) DO UPDATE SET sku=EXCLUDED.sku RETURNING controlled INTO actual;
 IF requested<>actual THEN
  RAISE EXCEPTION 'Every facility must retain the original tenant/SKU serial mode' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_serial_product_mode BEFORE INSERT OR UPDATE ON wms_products FOR EACH ROW EXECUTE FUNCTION wms_serial_product_mode();

CREATE FUNCTION wms_serial_unit_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' OR (NEW.tenant,NEW.sku,NEW.serial,NEW.lot) IS DISTINCT FROM (OLD.tenant,OLD.sku,OLD.serial,OLD.lot)
  OR NEW.revision<>OLD.revision+1 OR NEW.last_event_id=OLD.last_event_id THEN
  RAISE EXCEPTION 'Serialized units retain their original identity and advance one original event at a time' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_serial_unit_identity BEFORE UPDATE OR DELETE ON wms_serial_units FOR EACH ROW EXECUTE FUNCTION wms_serial_unit_identity();
CREATE TRIGGER wms_serial_unit_no_truncate BEFORE TRUNCATE ON wms_serial_units FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE FUNCTION wms_serial_head_identity() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' OR (NEW.tenant,NEW.sku) IS DISTINCT FROM (OLD.tenant,OLD.sku)
  OR NEW.version<>OLD.version+1 OR NEW.last_event_id IS NULL OR NEW.last_event_id IS NOT DISTINCT FROM OLD.last_event_id THEN
  RAISE EXCEPTION 'Serial review heads advance once per original SKU event' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_serial_head_identity BEFORE UPDATE OR DELETE ON wms_serial_heads FOR EACH ROW EXECUTE FUNCTION wms_serial_head_identity();
CREATE TRIGGER wms_serial_head_no_truncate BEFORE TRUNCATE ON wms_serial_heads FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_serial_batch_immutable BEFORE UPDATE OR DELETE ON wms_serial_batches FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_serial_batch_no_truncate BEFORE TRUNCATE ON wms_serial_batches FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_serial_step_immutable BEFORE UPDATE OR DELETE ON wms_serial_steps FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_serial_step_no_truncate BEFORE TRUNCATE ON wms_serial_steps FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_serial_observation_immutable BEFORE UPDATE OR DELETE ON wms_serial_observations FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_serial_observation_no_truncate BEFORE TRUNCATE ON wms_serial_observations FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();

CREATE FUNCTION wms_serial_mode_original(m wms_serial_modes) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT COALESCE(NOT EXISTS(SELECT 1 FROM wms_products p WHERE p.tenant=m.tenant AND p.sku=m.sku
  AND COALESCE(p.data->'serial_controlled','false'::jsonb)<>to_jsonb(m.controlled))
 AND (NOT m.controlled OR EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c
  ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
  WHERE e.id=m.source_event_id AND e.tenant=m.tenant AND e.warehouse_id=m.source_warehouse_id
  AND e.payload->>'id'=e.id AND e.payload->>'tenant'=m.tenant AND e.payload->>'warehouse_id'=m.source_warehouse_id
  AND e.payload->'version'=to_jsonb(e.version) AND e.payload->>'actor'=c.actor
  AND e.payload->>'type'='product.created' AND e.payload->'result'=c.result
  AND c.result=jsonb_build_object('entity_id',m.sku,'version',e.version,'type','product.created')
  AND e.payload->'command'->>'type'='create_product' AND e.payload->'command'->>'sku'=m.sku
  AND e.payload->'command'->'serial_controlled'='true'::jsonb
  AND EXISTS(SELECT 1 FROM wms_products p WHERE p.tenant=m.tenant AND p.warehouse_id=m.source_warehouse_id AND p.sku=m.sku))),false);
$$;

CREATE FUNCTION wms_serial_batch_original(b wms_serial_batches) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
 JOIN wms_serial_modes m ON m.tenant=b.tenant AND m.sku=b.sku AND m.controlled
 WHERE e.id=b.event_id AND e.tenant=b.tenant AND e.warehouse_id=b.warehouse_id AND e.version=b.warehouse_version
 AND e.payload->>'id'=e.id AND e.payload->>'tenant'=b.tenant AND e.payload->>'warehouse_id'=b.warehouse_id
 AND e.payload->'version'=to_jsonb(b.warehouse_version) AND e.payload->>'actor'=c.actor
 AND e.payload->'result'=c.result AND c.result->'version'=to_jsonb(b.warehouse_version) AND c.result->>'type'=e.payload->>'type'
 AND e.payload->'serial_heads'->b.sku=to_jsonb(b.version)
 AND b.warehouse_version<=(SELECT version FROM wms_warehouses WHERE tenant=b.tenant AND warehouse_id=b.warehouse_id)
 AND (b.version=1 OR EXISTS(SELECT 1 FROM wms_serial_batches previous WHERE previous.tenant=b.tenant AND previous.sku=b.sku AND previous.version=b.version-1)));
$$;

CREATE FUNCTION wms_serial_step_bound(s wms_serial_steps) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
 JOIN wms_serial_batches b ON b.tenant=s.tenant AND b.sku=s.sku AND b.version=s.sku_version AND b.event_id=s.event_id AND b.warehouse_id=s.warehouse_id AND b.warehouse_version=s.warehouse_version
 WHERE e.id=s.event_id AND e.tenant=s.tenant AND e.warehouse_id=s.warehouse_id AND e.version=s.warehouse_version
 AND e.payload->>'actor'=s.actor AND c.actor=s.actor AND e.payload->>'occurred_at'=s.recorded_at
 AND e.payload->'result'=c.result AND wms_serial_batch_original(b)
 AND e.payload->'serial_steps'->(s.ordinal-1)=to_jsonb(s)-'tenant'
 AND (s.revision=1 OR EXISTS(SELECT 1 FROM wms_serial_steps previous
  WHERE previous.tenant=s.tenant AND previous.sku=s.sku AND previous.serial=s.serial AND previous.revision=s.previous_revision
  AND previous.lot=s.lot AND previous.after_place=s.before_place AND previous.sku_version<s.sku_version)));
$$;

CREATE FUNCTION wms_serial_observation_bound(o wms_serial_observations) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_events e JOIN wms_serial_batches b ON b.tenant=o.tenant AND b.sku=o.sku AND b.version=o.sku_version AND b.event_id=o.event_id AND b.warehouse_id=o.warehouse_id
 WHERE e.id=o.event_id AND e.tenant=o.tenant AND e.warehouse_id=o.warehouse_id AND wms_serial_batch_original(b)
 AND e.payload->'serial_inputs'->(o.ordinal-1)=jsonb_build_object('target',o.target,'sku',o.sku,'serials',o.serials));
$$;

CREATE FUNCTION wms_serial_targets(t TEXT,w TEXT,c JSONB)
 RETURNS TABLE(target JSONB,sku TEXT,each_quantity BIGINT,allow_zero BOOLEAN) LANGUAGE SQL STABLE AS $$
 SELECT q.* FROM wms_unit_targets(t,w,c) q WHERE c->>'type' IN(
  'count_stock','move_stock','receive','pick','unpick','create_return','pack_shipment',
  'dispatch_transfer','receive_transfer','record_transfer_loss','reverse_transfer_loss');
$$;

CREATE FUNCTION wms_serial_inputs_match_command(t TEXT,w TEXT,c JSONB,inputs JSONB) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE i JSONB; n BIGINT=0;
BEGIN
 IF jsonb_typeof(inputs) IS DISTINCT FROM 'array' OR jsonb_array_length(inputs)>100 THEN RETURN false; END IF;
 -- Each original target is one declaration. A duplicated command line must
 -- not reuse a single observation merely because both entries name one SKU.
 IF EXISTS(SELECT 1 FROM wms_serial_targets(t,w,c) q GROUP BY q.target HAVING count(*)>1) THEN RETURN false; END IF;
 FOR i IN SELECT value FROM jsonb_array_elements(inputs) LOOP
  IF jsonb_typeof(i) IS DISTINCT FROM 'object' OR NOT i ?& ARRAY['target','sku','serials'] OR i-ARRAY['target','sku','serials']<>'{}'::jsonb
   OR jsonb_typeof(i->'target') IS DISTINCT FROM 'array' OR jsonb_array_length(i->'target')>3
   OR jsonb_typeof(i->'sku') IS DISTINCT FROM 'string' OR NOT wms_serial_text(i->>'sku')
   OR jsonb_typeof(i->'serials') IS DISTINCT FROM 'array' THEN RETURN false; END IF;
  n=n+jsonb_array_length(i->'serials');
  IF n>10000 OR EXISTS(SELECT 1 FROM jsonb_array_elements(i->'target') v WHERE jsonb_typeof(v) IS DISTINCT FROM 'string' OR NOT wms_serial_text(v#>>'{}'))
   OR EXISTS(SELECT 1 FROM jsonb_array_elements(i->'serials') v WHERE jsonb_typeof(v) IS DISTINCT FROM 'string' OR NOT wms_serial_text(v#>>'{}')) THEN RETURN false; END IF;
  IF NOT EXISTS(SELECT 1 FROM wms_serial_targets(t,w,c) q JOIN wms_serial_modes m ON m.tenant=t AND m.sku=q.sku AND m.controlled
   WHERE q.target=i->'target' AND q.sku=i->>'sku' AND q.each_quantity=jsonb_array_length(i->'serials')
   AND (q.each_quantity>0 OR q.allow_zero)) THEN RETURN false; END IF;
 END LOOP;
 IF EXISTS(SELECT 1 FROM jsonb_array_elements(inputs) AS entry(doc) GROUP BY entry.doc->'target' HAVING count(*)>1)
  OR EXISTS(SELECT 1 FROM jsonb_array_elements(inputs) AS entry(doc) CROSS JOIN LATERAL jsonb_array_elements(entry.doc->'serials') AS unit(label) GROUP BY entry.doc->>'sku',unit.label HAVING count(*)>1) THEN RETURN false; END IF;
 RETURN NOT EXISTS(SELECT 1 FROM wms_serial_targets(t,w,c) q JOIN wms_serial_modes m ON m.tenant=t AND m.sku=q.sku AND m.controlled
  WHERE NOT EXISTS(SELECT 1 FROM wms_products p WHERE p.tenant=t AND p.warehouse_id=w AND p.sku=q.sku AND p.data->'serial_controlled'='true'::jsonb)
  OR NOT EXISTS(SELECT 1 FROM jsonb_array_elements(inputs) AS entry(doc) WHERE entry.doc->'target'=q.target AND entry.doc->>'sku'=q.sku));
EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range THEN RETURN false;
END $$;

CREATE FUNCTION wms_serial_event_inputs_bound(t TEXT,w TEXT,eid TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE e JSONB; inputs JSONB; i JSONB; n INTEGER=0;
BEGIN
 SELECT payload INTO e FROM wms_events WHERE tenant=t AND warehouse_id=w AND id=eid;
 IF NOT FOUND THEN RETURN false; END IF;
 inputs=COALESCE(e->'serial_inputs','[]'::jsonb);
 IF NOT wms_serial_inputs_match_command(t,w,e->'command',inputs) THEN RETURN false; END IF;
 FOR i IN SELECT value FROM jsonb_array_elements(inputs) LOOP
  n=n+1;
  IF NOT EXISTS(SELECT 1 FROM wms_serial_observations o WHERE o.tenant=t AND o.warehouse_id=w AND o.event_id=eid AND o.ordinal=n
   AND o.sku=i->>'sku' AND o.target=i->'target' AND o.serials=i->'serials' AND e->'serial_heads'->o.sku=to_jsonb(o.sku_version)) THEN RETURN false; END IF;
 END LOOP;
 RETURN (SELECT count(*) FROM wms_serial_observations WHERE tenant=t AND warehouse_id=w AND event_id=eid)=n;
END $$;

CREATE OR REPLACE FUNCTION wms_serial_observation_bound(o wms_serial_observations) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_events e JOIN wms_serial_batches b ON b.tenant=o.tenant AND b.sku=o.sku AND b.version=o.sku_version AND b.event_id=o.event_id AND b.warehouse_id=o.warehouse_id
 WHERE e.id=o.event_id AND e.tenant=o.tenant AND e.warehouse_id=o.warehouse_id AND wms_serial_batch_original(b)
 AND e.payload->'serial_inputs'->(o.ordinal-1)=jsonb_build_object('target',o.target,'sku',o.sku,'serials',o.serials)
 AND wms_serial_event_inputs_bound(o.tenant,o.warehouse_id,o.event_id));
$$;

CREATE FUNCTION wms_serial_unit_bound(u wms_serial_units) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_serial_steps s WHERE s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial
  AND s.lot=u.lot AND s.revision=u.revision AND s.event_id=u.last_event_id AND s.after_place=u.place AND wms_serial_step_bound(s))
 AND NOT EXISTS(SELECT 1 FROM wms_serial_steps s WHERE s.tenant=u.tenant AND s.sku=u.sku AND s.serial=u.serial AND s.revision>u.revision);
$$;

-- Validate the latest projection from every inserted fact, not just when the
-- projection itself is written. Otherwise an appended batch/step can leave the
-- old current review/unit intact while all existing foreign keys still hold.
CREATE FUNCTION wms_serial_head_bound(h wms_serial_heads) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_serial_modes m WHERE m.tenant=h.tenant AND m.sku=h.sku AND m.controlled)
 AND ((h.version=0 AND h.last_event_id IS NULL AND NOT EXISTS(SELECT 1 FROM wms_serial_batches b WHERE b.tenant=h.tenant AND b.sku=h.sku))
 OR (EXISTS(SELECT 1 FROM wms_serial_batches b WHERE b.tenant=h.tenant AND b.sku=h.sku AND b.version=h.version AND b.event_id=h.last_event_id AND wms_serial_batch_original(b))
 AND NOT EXISTS(SELECT 1 FROM wms_serial_batches b WHERE b.tenant=h.tenant AND b.sku=h.sku AND b.version>h.version)));
$$;

CREATE FUNCTION wms_serial_fact_check() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE valid BOOLEAN;
BEGIN
 CASE TG_TABLE_NAME
 WHEN 'wms_serial_modes' THEN SELECT wms_serial_mode_original(m) AND (NOT m.controlled OR EXISTS(SELECT 1 FROM wms_serial_heads h WHERE h.tenant=m.tenant AND h.sku=m.sku AND wms_serial_head_bound(h))) INTO valid FROM wms_serial_modes m WHERE tenant=NEW.tenant AND sku=NEW.sku;
 WHEN 'wms_serial_batches' THEN SELECT wms_serial_batch_original(b) AND EXISTS(SELECT 1 FROM wms_serial_heads h WHERE h.tenant=b.tenant AND h.sku=b.sku AND wms_serial_head_bound(h)) INTO valid FROM wms_serial_batches b WHERE tenant=NEW.tenant AND sku=NEW.sku AND version=NEW.version;
 WHEN 'wms_serial_steps' THEN SELECT wms_serial_step_bound(s) AND EXISTS(SELECT 1 FROM wms_serial_units u WHERE u.tenant=s.tenant AND u.sku=s.sku AND u.serial=s.serial AND wms_serial_unit_bound(u)) INTO valid FROM wms_serial_steps s WHERE tenant=NEW.tenant AND sku=NEW.sku AND serial=NEW.serial AND revision=NEW.revision;
 WHEN 'wms_serial_observations' THEN SELECT wms_serial_observation_bound(o) INTO valid FROM wms_serial_observations o WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND event_id=NEW.event_id AND ordinal=NEW.ordinal;
 WHEN 'wms_serial_units' THEN SELECT wms_serial_unit_bound(u) INTO valid FROM wms_serial_units u WHERE tenant=NEW.tenant AND sku=NEW.sku AND serial=NEW.serial;
 WHEN 'wms_serial_heads' THEN SELECT wms_serial_head_bound(h) INTO valid FROM wms_serial_heads h WHERE tenant=NEW.tenant AND sku=NEW.sku;
 ELSE valid=false;
 END CASE;
 IF valid IS DISTINCT FROM true THEN RAISE EXCEPTION 'Original serial source, revision chain or current projection disagrees' USING ERRCODE='23514'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_serial_mode_bound AFTER INSERT OR UPDATE ON wms_serial_modes DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_fact_check();
CREATE CONSTRAINT TRIGGER wms_serial_head_bound AFTER INSERT OR UPDATE ON wms_serial_heads DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_fact_check();
CREATE CONSTRAINT TRIGGER wms_serial_batch_bound AFTER INSERT ON wms_serial_batches DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_fact_check();
CREATE CONSTRAINT TRIGGER wms_serial_step_bound AFTER INSERT ON wms_serial_steps DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_fact_check();
CREATE CONSTRAINT TRIGGER wms_serial_observation_bound AFTER INSERT ON wms_serial_observations DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_fact_check();
CREATE CONSTRAINT TRIGGER wms_serial_unit_bound AFTER INSERT OR UPDATE ON wms_serial_units DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_fact_check();

CREATE FUNCTION wms_serial_place_references_valid(t TEXT,s TEXT,l TEXT,p JSONB) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE valid BOOLEAN; status TEXT=p->>'status';
BEGIN
 IF NOT wms_serial_place_valid(p) OR NOT EXISTS(SELECT 1 FROM wms_serial_modes WHERE tenant=t AND sku=s AND controlled) THEN RETURN false; END IF;
 IF status IN('on_hand','missing') THEN
  RETURN EXISTS(SELECT 1 FROM wms_products product JOIN wms_locations bin ON bin.tenant=product.tenant AND bin.warehouse_id=product.warehouse_id
   WHERE product.tenant=t AND product.sku=s AND product.warehouse_id=p->>'warehouse_id' AND product.data->'serial_controlled'='true'::jsonb
   AND bin.code=p->>'location' AND bin.kind IN('storage','receiving'));
 ELSIF status='staged' THEN
  SELECT EXISTS(SELECT 1 FROM wms_allocations a WHERE a.tenant=t AND a.warehouse_id=p->>'warehouse_id' AND a.order_id=p->>'order_id'
   AND a.line_id=p->>'line_id' AND a.location=p->>'allocation_location' AND a.sku=s AND a.lot=l AND a.picked>a.shipped) INTO valid;
  RETURN valid AND (p->'package_id'='null'::jsonb OR EXISTS(SELECT 1 FROM wms_package_lines line
   WHERE line.tenant=t AND line.warehouse_id=p->>'warehouse_id' AND line.package_id=p->>'package_id' AND line.order_id=p->>'order_id'
   AND line.line_id=p->>'line_id' AND line.location=p->>'allocation_location' AND line.sku=s AND line.lot=l
   AND NOT EXISTS(SELECT 1 FROM wms_package_closures c WHERE c.tenant=line.tenant AND c.warehouse_id=line.warehouse_id AND c.package_id=line.package_id)));
 ELSIF status='shipped' THEN
  SELECT EXISTS(SELECT 1 FROM wms_order_lines line WHERE line.tenant=t AND line.warehouse_id=p->>'warehouse_id' AND line.order_id=p->>'order_id' AND line.id=p->>'line_id' AND line.sku=s)
   AND (CASE WHEN p->'package_id'='null'::jsonb THEN EXISTS(SELECT 1 FROM wms_shipment_evidence h WHERE h.tenant=t AND h.warehouse_id=p->>'warehouse_id' AND h.order_id=p->>'order_id' AND h.source_kind='event' AND h.event_id=p->>'handoff_event_id')
    ELSE EXISTS(SELECT 1 FROM wms_package_lines line JOIN wms_package_closures c ON c.tenant=line.tenant AND c.warehouse_id=line.warehouse_id AND c.package_id=line.package_id
     WHERE line.tenant=t AND line.warehouse_id=p->>'warehouse_id' AND line.package_id=p->>'package_id' AND line.order_id=p->>'order_id'
     AND line.line_id=p->>'line_id' AND line.sku=s AND line.lot=l AND c.kind='handed_off' AND c.event_id=p->>'handoff_event_id') END) INTO valid;
  RETURN valid AND (p->'return_receipt_id'='null'::jsonb OR EXISTS(SELECT 1 FROM wms_return_lines r JOIN wms_receipts receipt
   ON receipt.tenant=r.tenant AND receipt.warehouse_id=r.warehouse_id AND receipt.id=r.receipt_id
   WHERE r.tenant=t AND r.warehouse_id=p->>'warehouse_id' AND r.receipt_id=p->>'return_receipt_id'
   AND r.order_id=p->>'order_id' AND r.order_line_id=p->>'line_id' AND r.sku=s AND r.lot=l AND receipt.status<>'cancelled'));
 ELSIF status IN('in_transit','lost') THEN
  SELECT EXISTS(SELECT 1 FROM wms_transfer_lines line JOIN wms_products source ON source.tenant=line.tenant AND source.warehouse_id=line.source_warehouse_id AND source.sku=line.sku
   JOIN wms_products destination ON destination.tenant=line.tenant AND destination.warehouse_id=line.destination_warehouse_id AND destination.sku=line.sku
   WHERE line.tenant=t AND line.transfer_id=p->>'transfer_id' AND line.id=p->>'line_id' AND line.sku=s AND line.lot=l
   AND line.source_warehouse_id=p->>'source_warehouse_id' AND line.destination_warehouse_id=p->>'destination_warehouse_id'
   AND source.data->'serial_controlled'='true'::jsonb AND destination.data->'serial_controlled'='true'::jsonb) INTO valid;
  RETURN valid AND (status='in_transit' OR EXISTS(SELECT 1 FROM wms_transfer_loss_records r WHERE r.tenant=t AND r.id=p->>'loss_id' AND r.kind='loss'
   AND r.warehouse_id=p->>'destination_warehouse_id' AND r.transfer_id=p->>'transfer_id' AND r.line_id=p->>'line_id'));
 END IF;
 RETURN false;
END $$;

CREATE FUNCTION wms_serial_scope_current_valid(t TEXT,w TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 WITH expected_stock AS (
  SELECT s.sku,s.lot,s.location,s.on_hand AS quantity FROM wms_stock s JOIN wms_serial_modes m ON m.tenant=s.tenant AND m.sku=s.sku AND m.controlled
  WHERE s.tenant=t AND s.warehouse_id=w
 ), actual_stock AS (
  SELECT sku,lot,CASE WHEN place->>'status'='staged' THEN 'STAGE-01' ELSE place->>'location' END AS location,count(*)::bigint AS quantity
  FROM wms_serial_units WHERE tenant=t AND place->>'warehouse_id'=w AND place->>'status' IN('on_hand','staged')
  GROUP BY sku,lot,CASE WHEN place->>'status'='staged' THEN 'STAGE-01' ELSE place->>'location' END
 ), expected_staged AS (
  SELECT a.order_id,a.line_id,a.location,a.sku,a.lot,(a.picked-a.shipped)::bigint AS quantity FROM wms_allocations a
  JOIN wms_serial_modes m ON m.tenant=a.tenant AND m.sku=a.sku AND m.controlled WHERE a.tenant=t AND a.warehouse_id=w
 ), actual_staged AS (
  SELECT place->>'order_id' AS order_id,place->>'line_id' AS line_id,place->>'allocation_location' AS location,sku,lot,count(*)::bigint AS quantity
  FROM wms_serial_units WHERE tenant=t AND place->>'warehouse_id'=w AND place->>'status'='staged'
  GROUP BY place->>'order_id',place->>'line_id',place->>'allocation_location',sku,lot
 ), expected_packages AS (
  SELECT line.package_id,line.line_id,line.location,line.sku,line.lot,line.quantity FROM wms_package_lines line
  JOIN wms_serial_modes m ON m.tenant=line.tenant AND m.sku=line.sku AND m.controlled WHERE line.tenant=t AND line.warehouse_id=w
  AND NOT EXISTS(SELECT 1 FROM wms_package_closures c WHERE c.tenant=line.tenant AND c.warehouse_id=line.warehouse_id AND c.package_id=line.package_id)
 ), actual_packages AS (
  SELECT place->>'package_id' AS package_id,place->>'line_id' AS line_id,place->>'allocation_location' AS location,sku,lot,count(*)::bigint AS quantity
  FROM wms_serial_units WHERE tenant=t AND place->>'warehouse_id'=w AND place->>'status'='staged' AND place->>'package_id' IS NOT NULL
  GROUP BY place->>'package_id',place->>'line_id',place->>'allocation_location',sku,lot
 )
 SELECT NOT EXISTS(SELECT 1 FROM expected_stock e FULL JOIN actual_stock a USING(sku,lot,location) WHERE COALESCE(e.quantity,0)<>COALESCE(a.quantity,0))
 AND NOT EXISTS(SELECT 1 FROM expected_staged e FULL JOIN actual_staged a USING(order_id,line_id,location,sku,lot) WHERE COALESCE(e.quantity,0)<>COALESCE(a.quantity,0))
 AND NOT EXISTS(SELECT 1 FROM expected_packages e FULL JOIN actual_packages a USING(package_id,line_id,location,sku,lot) WHERE COALESCE(e.quantity,0)<>COALESCE(a.quantity,0))
 AND NOT EXISTS(SELECT 1 FROM wms_serial_units u WHERE u.tenant=t AND (u.place->>'warehouse_id'=w OR u.place->>'source_warehouse_id'=w OR u.place->>'destination_warehouse_id'=w)
  AND NOT wms_serial_place_references_valid(u.tenant,u.sku,u.lot,u.place));
$$;

CREATE FUNCTION wms_serial_on_hand(w TEXT,b TEXT) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT jsonb_build_object('status','on_hand','warehouse_id',w,'location',b);
$$;
CREATE FUNCTION wms_serial_staged(w TEXT,o TEXT,line TEXT,b TEXT,package TEXT) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT jsonb_build_object('status','staged','warehouse_id',w,'order_id',o,'line_id',line,'allocation_location',b,'package_id',package);
$$;
CREATE FUNCTION wms_serial_shipped(w TEXT,o TEXT,line TEXT,package TEXT,eid TEXT) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT jsonb_build_object('status','shipped','warehouse_id',w,'order_id',o,'line_id',line,'package_id',package,'handoff_event_id',eid,'return_receipt_id',NULL);
$$;
CREATE FUNCTION wms_serial_transit(source TEXT,destination TEXT,transfer TEXT,line TEXT) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT jsonb_build_object('status','in_transit','source_warehouse_id',source,'destination_warehouse_id',destination,'transfer_id',transfer,'line_id',line);
$$;

CREATE FUNCTION wms_serial_step_semantic_valid(s wms_serial_steps) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE e JSONB; c JSONB; command_type TEXT; expected_kind TEXT; observed BOOLEAN; original RECORD; matched BOOLEAN=false;
BEGIN
 SELECT payload INTO e FROM wms_events WHERE id=s.event_id AND tenant=s.tenant AND warehouse_id=s.warehouse_id AND version=s.warehouse_version;
 IF NOT FOUND THEN RETURN false; END IF;
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
 SELECT EXISTS(SELECT 1 FROM wms_serial_observations obs WHERE obs.tenant=s.tenant AND obs.warehouse_id=s.warehouse_id
  AND obs.event_id=s.event_id AND obs.sku=s.sku AND obs.target=s.target AND obs.serials @> jsonb_build_array(s.serial)
  AND wms_serial_observation_bound(obs)) INTO observed;
 IF command_type='count_stock' THEN
  RETURN COALESCE(s.target='[]'::jsonb AND s.sku=c->>'sku' AND s.lot=c->>'lot' AND (
   (observed AND s.after_place=wms_serial_on_hand(s.warehouse_id,c->>'location') AND (s.before_place IS NULL
    OR s.before_place=wms_serial_on_hand(s.warehouse_id,c->>'location')
    OR (s.before_place->>'status'='missing' AND s.before_place->>'warehouse_id'=s.warehouse_id)))
   OR (NOT observed AND s.before_place=wms_serial_on_hand(s.warehouse_id,c->>'location')
    AND s.after_place=jsonb_build_object('status','missing','warehouse_id',s.warehouse_id,'location',c->>'location')
    AND EXISTS(SELECT 1 FROM wms_serial_observations obs WHERE obs.tenant=s.tenant AND obs.warehouse_id=s.warehouse_id AND obs.event_id=s.event_id
     AND obs.sku=s.sku AND obs.target='[]'::jsonb AND wms_serial_observation_bound(obs)))),false);
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

CREATE OR REPLACE FUNCTION wms_serial_step_bound(s wms_serial_steps) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
 JOIN wms_serial_batches b ON b.tenant=s.tenant AND b.sku=s.sku AND b.version=s.sku_version AND b.event_id=s.event_id AND b.warehouse_id=s.warehouse_id AND b.warehouse_version=s.warehouse_version
 WHERE e.id=s.event_id AND e.tenant=s.tenant AND e.warehouse_id=s.warehouse_id AND e.version=s.warehouse_version
 AND e.payload->>'actor'=s.actor AND c.actor=s.actor AND e.payload->>'occurred_at'=s.recorded_at
 AND e.payload->'result'=c.result AND wms_serial_batch_original(b)
 AND e.payload->'serial_steps'->(s.ordinal-1)=to_jsonb(s)-'tenant' AND wms_serial_step_semantic_valid(s)
 AND (s.revision=1 OR EXISTS(SELECT 1 FROM wms_serial_steps previous
  WHERE previous.tenant=s.tenant AND previous.sku=s.sku AND previous.serial=s.serial AND previous.revision=s.previous_revision
  AND previous.lot=s.lot AND previous.after_place=s.before_place AND previous.sku_version<s.sku_version)));
$$;

CREATE INDEX wms_serial_history_stock ON wms_serial_steps(tenant,sku,(after_place->>'warehouse_id'),lot,(after_place->>'location'),sku_version,serial) WHERE after_place->>'status'='on_hand';
CREATE INDEX wms_serial_history_order ON wms_serial_steps(tenant,sku,(after_place->>'warehouse_id'),(after_place->>'order_id'),sku_version,serial) WHERE after_place->>'status'='staged';
CREATE INDEX wms_serial_history_package ON wms_serial_steps(tenant,sku,(after_place->>'warehouse_id'),(after_place->>'package_id'),sku_version,serial) WHERE after_place->>'status'='staged';
CREATE INDEX wms_serial_history_rma ON wms_serial_steps(tenant,sku,(after_place->>'warehouse_id'),(after_place->>'return_receipt_id'),sku_version,serial) WHERE after_place->>'status'='shipped' AND after_place->>'return_receipt_id' IS NOT NULL;

CREATE FUNCTION wms_serial_event_skus(t TEXT,w TEXT,c JSONB) RETURNS TABLE(sku TEXT) LANGUAGE SQL STABLE AS $$
 SELECT DISTINCT candidate.sku FROM (
  SELECT q.sku FROM wms_serial_targets(t,w,c) q
  UNION ALL SELECT line.sku FROM wms_order_lines line WHERE c->>'type'='ship' AND line.tenant=t AND line.warehouse_id=w AND line.order_id=c->>'order_id'
  UNION ALL SELECT line.sku FROM wms_package_lines line WHERE c->>'type' IN('unpack_shipment','handoff_shipment') AND line.tenant=t AND line.warehouse_id=w AND line.package_id=c->>'shipment_id'
  UNION ALL SELECT line.sku FROM wms_return_lines line WHERE c->>'type'='cancel_receipt' AND line.tenant=t AND line.warehouse_id=w AND line.receipt_id=c->>'receipt_id'
 ) candidate JOIN wms_serial_modes mode ON mode.tenant=t AND mode.sku=candidate.sku AND mode.controlled;
$$;

CREATE FUNCTION wms_serial_at_version(t TEXT,s TEXT,uid TEXT,v BIGINT) RETURNS JSONB LANGUAGE SQL STABLE AS $$
 SELECT after_place FROM wms_serial_steps WHERE tenant=t AND sku=s AND serial=uid AND sku_version<=v ORDER BY revision DESC LIMIT 1;
$$;

CREATE FUNCTION wms_serial_event_valid(t TEXT,w TEXT,eid TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
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
  OR EXISTS(SELECT 1 FROM wms_serial_steps step WHERE step.tenant=t AND step.event_id=eid AND NOT wms_serial_step_bound(step))
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

CREATE FUNCTION wms_serial_scope_transit_valid(t TEXT,w TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT NOT EXISTS(SELECT 1 FROM wms_transfer_lines line JOIN wms_serial_modes mode ON mode.tenant=line.tenant AND mode.sku=line.sku AND mode.controlled
  WHERE line.tenant=t AND (line.source_warehouse_id=w OR line.destination_warehouse_id=w)
  AND line.quantity - COALESCE((SELECT sum(m.quantity) FROM wms_movements m WHERE m.tenant=line.tenant AND m.warehouse_id=line.destination_warehouse_id
   AND m.transfer_id=line.transfer_id AND m.transfer_line_id=line.id AND m.kind='transfer_receive'),0)
  <> (SELECT count(*) FROM wms_serial_units unit WHERE unit.tenant=line.tenant AND unit.sku=line.sku AND unit.lot=line.lot
   AND unit.place->>'transfer_id'=line.transfer_id AND unit.place->>'line_id'=line.id AND unit.place->>'status' IN('in_transit','lost')))
 AND NOT EXISTS(SELECT 1 FROM wms_transfer_loss_records loss JOIN wms_transfer_lines line ON line.tenant=loss.tenant AND line.transfer_id=loss.transfer_id AND line.id=loss.line_id
  JOIN wms_serial_modes mode ON mode.tenant=line.tenant AND mode.sku=line.sku AND mode.controlled
  WHERE loss.tenant=t AND loss.kind='loss' AND (line.source_warehouse_id=w OR line.destination_warehouse_id=w)
  AND loss.quantity-COALESCE((SELECT sum(reversal.quantity) FROM wms_transfer_loss_records reversal WHERE reversal.tenant=loss.tenant AND reversal.original_loss_id=loss.id AND reversal.kind='reversal'),0)
  <> (SELECT count(*) FROM wms_serial_units unit WHERE unit.tenant=line.tenant AND unit.sku=line.sku AND unit.lot=line.lot AND unit.place->>'status'='lost'
   AND unit.place->>'loss_id'=loss.id AND unit.place->>'transfer_id'=line.transfer_id AND unit.place->>'line_id'=line.id));
$$;

CREATE FUNCTION wms_serial_scope_commit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE d JSONB; t TEXT; w TEXT; source_sku TEXT;
BEGIN
 IF TG_OP='DELETE' THEN d=to_jsonb(OLD); ELSE d=to_jsonb(NEW); END IF;
 t=d->>'tenant'; w=COALESCE(d->>'warehouse_id',d->>'source_warehouse_id',d->>'destination_warehouse_id'); source_sku=d->>'sku';
 IF source_sku IS NOT NULL AND NOT EXISTS(SELECT 1 FROM wms_serial_modes WHERE tenant=t AND sku=source_sku AND controlled) THEN RETURN NULL; END IF;
 IF NOT EXISTS(SELECT 1 FROM wms_serial_modes WHERE tenant=t AND controlled) THEN RETURN NULL; END IF;
 IF EXISTS(SELECT 1 FROM wms_serial_modes mode WHERE mode.tenant=t AND mode.controlled AND (source_sku IS NULL OR mode.sku=source_sku) AND NOT wms_serial_mode_original(mode))
  OR NOT wms_serial_scope_current_valid(t,w) OR NOT wms_serial_scope_transit_valid(t,w) THEN
  RAISE EXCEPTION 'Serialized stock, staging, package, transit or original loss ownership disagrees' USING ERRCODE='23514';
 END IF;
 IF TG_TABLE_NAME='wms_events' AND NOT wms_serial_event_valid(t,w,d->>'id') THEN
  RAISE EXCEPTION 'Original serial event lacks complete exact inputs, effects or source relationships' USING ERRCODE='23514';
 END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_serial_event_proof AFTER INSERT ON wms_events DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
CREATE CONSTRAINT TRIGGER wms_serial_stock_proof AFTER INSERT OR UPDATE OR DELETE ON wms_stock DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
CREATE CONSTRAINT TRIGGER wms_serial_allocation_proof AFTER INSERT OR UPDATE OR DELETE ON wms_allocations DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
CREATE CONSTRAINT TRIGGER wms_serial_package_line_proof AFTER INSERT OR UPDATE OR DELETE ON wms_package_lines DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
CREATE CONSTRAINT TRIGGER wms_serial_package_close_proof AFTER INSERT ON wms_package_closures DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
CREATE CONSTRAINT TRIGGER wms_serial_receipt_proof AFTER UPDATE OR DELETE ON wms_receipts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
CREATE CONSTRAINT TRIGGER wms_serial_catalog_proof AFTER INSERT OR UPDATE OR DELETE ON wms_products DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
CREATE CONSTRAINT TRIGGER wms_serial_location_proof AFTER UPDATE OR DELETE ON wms_locations DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
CREATE CONSTRAINT TRIGGER wms_serial_transfer_line_proof AFTER INSERT ON wms_transfer_lines DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();
CREATE CONSTRAINT TRIGGER wms_serial_loss_quantity_proof AFTER INSERT ON wms_transfer_loss_records DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_serial_scope_commit();

-- Remaining qualification: complete multi-unit application/owner workflows,
-- workload cost, source-checked read pages, operator flows and nonempty restore.
-- Existing count events already fence identity-only physical reviews (009).
-- Focused source/transaction checks alone do not qualify the complete feature.
