-- Packaging is reviewed input evidence; custody and canonical command quantities
-- remain exact each. No historic conversion is inferred on upgrade.
CREATE TABLE wms_unit_pack_changes (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, id TEXT NOT NULL,
 version BIGINT NOT NULL CHECK(version>0), sku TEXT NOT NULL,
 code TEXT NOT NULL CHECK(code ~ '^[A-Z0-9_-]{1,32}$' AND code<>'EA'),
 label TEXT NOT NULL CHECK(octet_length(label) BETWEEN 1 AND 240 AND btrim(label)=label AND label !~ '[[:cntrl:]]'),
 each_per_pack BIGINT NOT NULL CHECK(each_per_pack BETWEEN 1 AND 1000000),
 enabled BOOLEAN NOT NULL,
 reason TEXT NOT NULL CHECK(octet_length(reason) BETWEEN 1 AND 240 AND btrim(reason)=reason AND reason !~ '[[:cntrl:]]'),
 actor TEXT NOT NULL CHECK(octet_length(actor) BETWEEN 1 AND 240 AND btrim(actor)=actor AND actor !~ '[[:cntrl:]]'),
 recorded_at TEXT NOT NULL,
 source_event_id TEXT NOT NULL UNIQUE REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 PRIMARY KEY(tenant,warehouse_id,id), UNIQUE(tenant,warehouse_id,version),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku)
);
CREATE INDEX wms_unit_pack_current ON wms_unit_pack_changes(tenant,warehouse_id,sku,code,version DESC);
CREATE TABLE wms_unit_observations (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL,
 event_id TEXT NOT NULL REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 version BIGINT NOT NULL CHECK(version>0), ordinal INTEGER NOT NULL CHECK(ordinal BETWEEN 1 AND 100),
 target JSONB NOT NULL CHECK(jsonb_typeof(target)='array' AND jsonb_array_length(target)<=3),
 sku TEXT NOT NULL, pack_code TEXT NOT NULL,
 definition_id TEXT NOT NULL,
 pack_quantity BIGINT NOT NULL CHECK(pack_quantity BETWEEN 0 AND 1000000),
 each_per_pack BIGINT NOT NULL CHECK(each_per_pack BETWEEN 1 AND 1000000),
 each_quantity BIGINT NOT NULL CHECK(each_quantity BETWEEN 0 AND 1000000 AND each_quantity=pack_quantity*each_per_pack),
 PRIMARY KEY(tenant,warehouse_id,event_id,ordinal), UNIQUE(tenant,warehouse_id,event_id,target),
 FOREIGN KEY(tenant,warehouse_id,definition_id) REFERENCES wms_unit_pack_changes(tenant,warehouse_id,id),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku)
);
CREATE INDEX wms_unit_observation_sku ON wms_unit_observations(tenant,warehouse_id,sku,version,ordinal);
CREATE TRIGGER wms_unit_pack_immutable BEFORE UPDATE OR DELETE ON wms_unit_pack_changes FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_unit_pack_no_truncate BEFORE TRUNCATE ON wms_unit_pack_changes FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_unit_observation_immutable BEFORE UPDATE OR DELETE ON wms_unit_observations FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_unit_observation_no_truncate BEFORE TRUNCATE ON wms_unit_observations FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();

CREATE FUNCTION wms_unit_pack_original(p wms_unit_pack_changes) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
 WHERE e.id=p.source_event_id AND e.tenant=p.tenant AND e.warehouse_id=p.warehouse_id AND e.version=p.version
 AND e.payload->>'id'=e.id AND e.payload->>'tenant'=p.tenant AND e.payload->>'warehouse_id'=p.warehouse_id
 AND e.payload->'version'=to_jsonb(p.version) AND e.payload->>'actor'=p.actor AND c.actor=p.actor
 AND p.version<=(SELECT version FROM wms_warehouses WHERE tenant=p.tenant AND warehouse_id=p.warehouse_id)
 AND (p.enabled OR COALESCE((SELECT x.enabled FROM wms_unit_pack_changes x WHERE x.tenant=p.tenant AND x.warehouse_id=p.warehouse_id AND x.sku=p.sku AND x.code=p.code AND x.version<p.version ORDER BY x.version DESC LIMIT 1),false))
 AND e.payload->>'type'='inventory.unit_pack_changed' AND e.payload->>'occurred_at'=p.recorded_at
 AND e.payload->'unit_pack_change'=to_jsonb(p)-'tenant'
 AND e.payload->'result'=c.result AND c.result=jsonb_build_object('entity_id',p.id,'version',p.version,'type','inventory.unit_pack_changed')
 AND e.payload->'command'=jsonb_build_object('type','set_unit_pack','sku',p.sku,'code',p.code,'label',p.label,'each_per_pack',p.each_per_pack,'enabled',p.enabled,'reason',p.reason));
$$;

-- The target belongs to the original command, not a position in an unrelated
-- SKU list. Owning line identities remain governed by their existing proofs.
CREATE FUNCTION wms_unit_targets(t TEXT,w TEXT,d JSONB)
RETURNS TABLE(target JSONB,sku TEXT,each_quantity BIGINT,allow_zero BOOLEAN) LANGUAGE SQL STABLE AS $$
 SELECT '[]'::jsonb,d->>'sku',(d->>'quantity')::bigint,false WHERE d->>'type'='move_stock'
 UNION ALL SELECT '[]'::jsonb,d->>'sku',(d->>'counted')::bigint,true WHERE d->>'type'='count_stock'
 UNION ALL SELECT '[]'::jsonb,l.sku,(d->>'quantity')::bigint,false FROM wms_receipt_lines l
 WHERE d->>'type'='receive' AND l.tenant=t AND l.warehouse_id=w AND l.receipt_id=d->>'receipt_id' AND l.id=d->>'line_id'
 UNION ALL SELECT '[]'::jsonb,l.sku,(d->>'quantity')::bigint,false FROM wms_order_lines l
 WHERE d->>'type' IN('pick','unpick') AND l.tenant=t AND l.warehouse_id=w AND l.order_id=d->>'order_id' AND l.id=d->>'line_id'
 UNION ALL SELECT '[]'::jsonb,l.sku,(d->>'quantity')::bigint,false FROM wms_transfer_lines l
 WHERE d->>'type' IN('receive_transfer','record_transfer_loss') AND l.tenant=t AND l.destination_warehouse_id=w AND l.transfer_id=d->>'transfer_id' AND l.id=d->>'line_id'
 UNION ALL SELECT '[]'::jsonb,l.sku,(d->>'quantity')::bigint,false FROM wms_transfer_loss_records r
 JOIN wms_transfer_lines l ON l.tenant=r.tenant AND l.transfer_id=r.transfer_id AND l.id=r.line_id
 WHERE d->>'type'='reverse_transfer_loss' AND r.tenant=t AND r.warehouse_id=w AND r.id=d->>'loss_id' AND r.kind='loss'
 UNION ALL SELECT jsonb_build_array(l->>'id'),l->>'sku',(l->>'quantity')::bigint,false
 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(d->'lines')='array' THEN d->'lines' ELSE '[]'::jsonb END) l
 WHERE d->>'type' IN('dispatch_transfer','create_order','create_receipt')
 UNION ALL SELECT jsonb_build_array(l->>'id'),o.sku,(l->>'quantity')::bigint,false
 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(d->'lines')='array' THEN d->'lines' ELSE '[]'::jsonb END) l
 JOIN wms_order_lines o ON o.tenant=t AND o.warehouse_id=w AND o.order_id=d->>'order_id' AND o.id=l->>'order_line_id'
 WHERE d->>'type'='create_return'
 UNION ALL SELECT jsonb_build_array(l->>'line_id',l->>'location',l->>'lot'),o.sku,(l->>'quantity')::bigint,false
 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(d->'lines')='array' THEN d->'lines' ELSE '[]'::jsonb END) l
 JOIN wms_order_lines o ON o.tenant=t AND o.warehouse_id=w AND o.order_id=d->>'order_id' AND o.id=l->>'line_id'
 WHERE d->>'type'='pack_shipment';
$$;
CREATE FUNCTION wms_unit_observation_original(r wms_unit_observations) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
BEGIN
 RETURN EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
 JOIN wms_unit_pack_changes p ON p.tenant=r.tenant AND p.warehouse_id=r.warehouse_id AND p.id=r.definition_id
 WHERE e.id=r.event_id AND e.tenant=r.tenant AND e.warehouse_id=r.warehouse_id AND e.version=r.version
 AND e.payload->>'id'=e.id AND e.payload->>'tenant'=r.tenant AND e.payload->>'warehouse_id'=r.warehouse_id
 AND e.payload->'version'=to_jsonb(r.version) AND e.payload->>'actor'=c.actor AND e.payload->'result'=c.result
 AND c.result->'version'=to_jsonb(r.version) AND c.result->>'type'=e.payload->>'type'
 AND r.version<=(SELECT version FROM wms_warehouses WHERE tenant=r.tenant AND warehouse_id=r.warehouse_id)
 AND p.sku=r.sku AND p.code=r.pack_code AND p.each_per_pack=r.each_per_pack AND p.enabled AND wms_unit_pack_original(p)
 AND r.each_quantity=r.pack_quantity*p.each_per_pack
 AND p.id=(SELECT id FROM wms_unit_pack_changes x WHERE x.tenant=r.tenant AND x.warehouse_id=r.warehouse_id AND x.sku=r.sku AND x.code=r.pack_code AND x.version<r.version ORDER BY x.version DESC LIMIT 1)
 AND e.payload->'unit_inputs'->(r.ordinal-1)=jsonb_build_object('target',r.target,'sku',r.sku,'pack_code',r.pack_code,'definition_id',r.definition_id,'pack_quantity',r.pack_quantity,'each_per_pack',r.each_per_pack)
 AND EXISTS(SELECT 1 FROM wms_unit_targets(r.tenant,r.warehouse_id,e.payload->'command') q
 WHERE q.target=r.target AND q.sku=r.sku AND q.each_quantity=r.each_quantity AND (q.each_quantity>0 OR q.allow_zero)));
EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range THEN RETURN false;
END $$;
CREATE FUNCTION wms_unit_event_valid(t TEXT,w TEXT,eid TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE e RECORD; inputs JSONB; actual BIGINT;
BEGIN
 SELECT * INTO e FROM wms_events WHERE tenant=t AND warehouse_id=w AND id=eid;
 IF NOT FOUND THEN RETURN false; END IF;
 inputs=COALESCE(e.payload->'unit_inputs','[]'::jsonb);
 IF jsonb_typeof(inputs)<>'array' OR jsonb_array_length(inputs)>100 THEN RETURN false; END IF;
 SELECT count(*) INTO actual FROM wms_unit_observations WHERE tenant=t AND warehouse_id=w AND event_id=eid;
 IF actual<>jsonb_array_length(inputs) OR EXISTS(SELECT 1 FROM wms_unit_observations r WHERE r.tenant=t AND r.warehouse_id=w AND r.event_id=eid AND NOT wms_unit_observation_original(r)) THEN RETURN false; END IF;
 IF e.payload#>>'{command,type}'='set_unit_pack' THEN
  IF NOT EXISTS(SELECT 1 FROM wms_unit_pack_changes p WHERE p.tenant=t AND p.warehouse_id=w AND p.source_event_id=eid AND wms_unit_pack_original(p)) THEN RETURN false; END IF;
 ELSIF e.payload?'unit_pack_change' OR EXISTS(SELECT 1 FROM wms_unit_pack_changes p WHERE p.tenant=t AND p.warehouse_id=w AND p.source_event_id=eid) THEN RETURN false;
 END IF;
 RETURN true;
END $$;
CREATE FUNCTION wms_unit_scope_valid(t TEXT,w TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT NOT EXISTS(SELECT 1 FROM wms_unit_pack_changes p WHERE p.tenant=t AND p.warehouse_id=w AND
  (p.version>(SELECT version FROM wms_warehouses WHERE tenant=t AND warehouse_id=w) OR NOT wms_unit_pack_original(p)))
 AND NOT EXISTS(SELECT 1 FROM (SELECT DISTINCT ON(sku,code) sku,code,enabled FROM wms_unit_pack_changes WHERE tenant=t AND warehouse_id=w ORDER BY sku,code,version DESC) p WHERE enabled GROUP BY sku HAVING count(*)>20)
 AND NOT EXISTS(SELECT 1 FROM wms_events e WHERE e.tenant=t AND e.warehouse_id=w AND
  (e.payload?'unit_inputs' OR e.payload?'unit_pack_change' OR e.payload#>>'{command,type}'='set_unit_pack' OR e.payload#>>'{result,type}'='inventory.unit_pack_changed') AND NOT wms_unit_event_valid(t,w,e.id))
 AND NOT EXISTS(SELECT 1 FROM wms_unit_observations r WHERE r.tenant=t AND r.warehouse_id=w AND NOT wms_unit_observation_original(r));
$$;
CREATE FUNCTION wms_unit_lock() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 PERFORM wms_serialize_shipment(NEW.tenant,NEW.warehouse_id);
 IF TG_TABLE_NAME='wms_unit_pack_changes' THEN PERFORM wms_bump_resource(NEW.tenant,NEW.warehouse_id,jsonb_build_array('product',NEW.sku)); END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_unit_pack_lock BEFORE INSERT ON wms_unit_pack_changes FOR EACH ROW EXECUTE FUNCTION wms_unit_lock();
CREATE TRIGGER wms_unit_observation_lock BEFORE INSERT ON wms_unit_observations FOR EACH ROW EXECUTE FUNCTION wms_unit_lock();
CREATE FUNCTION wms_unit_commit_proof() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE eid TEXT;
BEGIN
 IF TG_TABLE_NAME='wms_unit_pack_changes' THEN
  eid=NEW.source_event_id;
  IF EXISTS(SELECT 1 FROM (SELECT DISTINCT ON(code) code,enabled FROM wms_unit_pack_changes WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND sku=NEW.sku AND version<=NEW.version ORDER BY code,version DESC) p WHERE enabled HAVING count(*)>20) THEN
   RAISE EXCEPTION 'A product cannot enable more than twenty packaging units' USING ERRCODE='23514';
  END IF;
 ELSIF TG_TABLE_NAME='wms_events' THEN eid=NEW.id; ELSE eid=NEW.event_id; END IF;
 IF NOT wms_unit_event_valid(NEW.tenant,NEW.warehouse_id,eid) THEN RAISE EXCEPTION 'Unit observation or pack definition lacks its exact original conversion source' USING ERRCODE='23514'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_unit_pack_proof AFTER INSERT ON wms_unit_pack_changes DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_unit_commit_proof();
CREATE CONSTRAINT TRIGGER wms_unit_observation_proof AFTER INSERT ON wms_unit_observations DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_unit_commit_proof();
CREATE CONSTRAINT TRIGGER wms_unit_event_proof AFTER INSERT ON wms_events DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN(NEW.payload?'unit_inputs' OR NEW.payload?'unit_pack_change' OR NEW.payload#>>'{command,type}'='set_unit_pack' OR NEW.payload#>>'{result,type}'='inventory.unit_pack_changed') EXECUTE FUNCTION wms_unit_commit_proof();
