-- A dispatch leaves source custody. Destination custody changes only on witnessed
-- partial arrivals. All original ownership and physical movements are immutable.
CREATE TABLE wms_transfers (
 seq BIGSERIAL UNIQUE, tenant TEXT NOT NULL, id TEXT NOT NULL,
 source_warehouse_id TEXT NOT NULL, destination_warehouse_id TEXT NOT NULL,
 reference TEXT NOT NULL CHECK(length(btrim(reference)) BETWEEN 1 AND 240),
 carrier TEXT NOT NULL CHECK(length(btrim(carrier)) BETWEEN 1 AND 240),
 handoff_reference TEXT NOT NULL CHECK(length(btrim(handoff_reference)) BETWEEN 1 AND 240),
 actor TEXT NOT NULL, dispatched_at TEXT NOT NULL,
 source_version BIGINT NOT NULL CHECK(source_version>0),
 source_event_id TEXT NOT NULL UNIQUE REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 PRIMARY KEY(tenant,id), UNIQUE(tenant,id,source_warehouse_id,destination_warehouse_id),
 UNIQUE(tenant,source_warehouse_id,reference), CHECK(source_warehouse_id<>destination_warehouse_id),
 FOREIGN KEY(tenant,source_warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id),
 FOREIGN KEY(tenant,destination_warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE INDEX wms_transfers_source ON wms_transfers(tenant,source_warehouse_id,seq);
CREATE INDEX wms_transfers_destination ON wms_transfers(tenant,destination_warehouse_id,seq);
CREATE TABLE wms_transfer_lines (
 tenant TEXT NOT NULL, transfer_id TEXT NOT NULL, id TEXT NOT NULL,
 source_warehouse_id TEXT NOT NULL, destination_warehouse_id TEXT NOT NULL,
 sku TEXT NOT NULL, lot TEXT NOT NULL, location TEXT NOT NULL,
 quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000),
 movement_id TEXT NOT NULL UNIQUE REFERENCES wms_movements(id) DEFERRABLE INITIALLY DEFERRED,
 PRIMARY KEY(tenant,transfer_id,id),
 FOREIGN KEY(tenant,transfer_id,source_warehouse_id,destination_warehouse_id) REFERENCES wms_transfers(tenant,id,source_warehouse_id,destination_warehouse_id),
 FOREIGN KEY(tenant,source_warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku),
 FOREIGN KEY(tenant,destination_warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku),
 FOREIGN KEY(tenant,source_warehouse_id,location) REFERENCES wms_locations(tenant,warehouse_id,code)
);
ALTER TABLE wms_movements DROP CONSTRAINT wms_movements_kind_check;
ALTER TABLE wms_movements ADD CONSTRAINT wms_movements_kind_check CHECK(kind IN('receipt','putaway','pick','unpick','shipment','count','transfer_dispatch','transfer_receive'));
ALTER TABLE wms_movements ADD COLUMN transfer_id TEXT, ADD COLUMN transfer_line_id TEXT,
 ADD COLUMN transfer_event_id TEXT REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 ADD FOREIGN KEY(tenant,transfer_id,transfer_line_id) REFERENCES wms_transfer_lines(tenant,transfer_id,id) DEFERRABLE INITIALLY DEFERRED,
 ADD CONSTRAINT wms_movement_transfer_identity CHECK(
  (kind IN('transfer_dispatch','transfer_receive') AND transfer_id IS NOT NULL AND transfer_line_id IS NOT NULL AND transfer_event_id IS NOT NULL)
  OR (kind NOT IN('transfer_dispatch','transfer_receive') AND transfer_id IS NULL AND transfer_line_id IS NULL AND transfer_event_id IS NULL)),
 ADD CONSTRAINT wms_movement_transfer_data CHECK(
  data->>'transfer_id' IS NOT DISTINCT FROM transfer_id AND data->>'transfer_line_id' IS NOT DISTINCT FROM transfer_line_id
  AND data->>'transfer_event_id' IS NOT DISTINCT FROM transfer_event_id);
CREATE UNIQUE INDEX wms_transfer_one_dispatch ON wms_movements(tenant,transfer_id,transfer_line_id) WHERE kind='transfer_dispatch';
CREATE UNIQUE INDEX wms_transfer_one_arrival_event ON wms_movements(transfer_event_id) WHERE kind='transfer_receive';
CREATE INDEX wms_transfer_movements ON wms_movements(tenant,transfer_id,transfer_line_id,kind);
CREATE TRIGGER wms_transfers_immutable BEFORE UPDATE OR DELETE ON wms_transfers FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_transfers_no_truncate BEFORE TRUNCATE ON wms_transfers FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_transfer_lines_immutable BEFORE UPDATE OR DELETE ON wms_transfer_lines FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_transfer_lines_no_truncate BEFORE TRUNCATE ON wms_transfer_lines FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();

CREATE FUNCTION wms_transfer_command_lines(d JSONB) RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT CASE WHEN jsonb_typeof(d)='array' THEN (SELECT jsonb_agg(jsonb_build_object('id',l->'id','sku',l->'sku','lot',l->'lot','location',l->'location','quantity',l->'quantity') ORDER BY l->>'id') FROM jsonb_array_elements(d) l) END;
$$;
CREATE FUNCTION wms_transfer_consistent(t TEXT,transfer TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
DECLARE h RECORD; source JSONB; expected JSONB;
BEGIN
 SELECT * INTO h FROM wms_transfers WHERE tenant=t AND id=transfer;
 IF NOT FOUND THEN RETURN false; END IF;
 SELECT payload INTO source FROM wms_events WHERE id=h.source_event_id AND tenant=t AND warehouse_id=h.source_warehouse_id AND version=h.source_version;
 IF NOT FOUND OR source->>'type' IS DISTINCT FROM 'transfer.dispatched'
 OR source->>'id' IS DISTINCT FROM h.source_event_id OR source->>'tenant' IS DISTINCT FROM t
 OR source->>'warehouse_id' IS DISTINCT FROM h.source_warehouse_id OR source->'version' IS DISTINCT FROM to_jsonb(h.source_version)
 OR source->>'actor' IS DISTINCT FROM h.actor OR source#>>'{command,type}' IS DISTINCT FROM 'dispatch_transfer'
 OR source#>>'{command,reference}' IS DISTINCT FROM h.reference
 OR source#>>'{command,destination_warehouse_id}' IS DISTINCT FROM h.destination_warehouse_id
 OR source#>>'{command,carrier}' IS DISTINCT FROM h.carrier OR source#>>'{command,handoff_reference}' IS DISTINCT FROM h.handoff_reference
 OR source#>>'{result,type}' IS DISTINCT FROM 'transfer.dispatched' OR source#>>'{result,entity_id}' IS DISTINCT FROM transfer OR source#>'{result,version}' IS DISTINCT FROM to_jsonb(h.source_version)
 OR h.source_version>(SELECT version FROM wms_warehouses WHERE tenant=t AND warehouse_id=h.source_warehouse_id) THEN RETURN false; END IF;
 SELECT jsonb_agg(jsonb_build_object('id',l.id,'sku',l.sku,'lot',l.lot,'location',l.location,'quantity',l.quantity) ORDER BY l.id) INTO expected FROM wms_transfer_lines l WHERE l.tenant=t AND l.transfer_id=transfer;
 IF expected IS NULL OR jsonb_array_length(expected)>100 OR expected IS DISTINCT FROM wms_transfer_command_lines(source#>'{command,lines}') THEN RETURN false; END IF;
 IF EXISTS(SELECT 1 FROM wms_transfer_lines l LEFT JOIN wms_movements m ON m.id=l.movement_id
  WHERE l.tenant=t AND l.transfer_id=transfer AND
  (m.id IS NULL OR (m.tenant,m.warehouse_id,m.transfer_id,m.transfer_line_id,m.transfer_event_id,m.kind,m.sku,m.lot,m.from_location,m.quantity,m.reference)
    IS DISTINCT FROM (t,h.source_warehouse_id,transfer,l.id,h.source_event_id,'transfer_dispatch',l.sku,l.lot,l.location,l.quantity,transfer)
   OR m.to_location IS NOT NULL OR m.data->>'actor' IS DISTINCT FROM h.actor OR m.data->>'reason' IS DISTINCT FROM h.handoff_reference
   OR (SELECT kind FROM wms_locations WHERE tenant=t AND warehouse_id=h.source_warehouse_id AND code=l.location) IS DISTINCT FROM 'storage'
   OR (SELECT COALESCE(sum(quantity),0) FROM wms_movements WHERE tenant=t AND transfer_id=transfer AND transfer_line_id=l.id AND kind='transfer_receive')>l.quantity)) THEN RETURN false; END IF;
 IF EXISTS(SELECT 1 FROM wms_movements m JOIN wms_transfer_lines l ON l.tenant=m.tenant AND l.transfer_id=m.transfer_id AND l.id=m.transfer_line_id
  LEFT JOIN wms_events e ON e.id=m.transfer_event_id
  WHERE m.tenant=t AND m.transfer_id=transfer AND (
   (m.kind='transfer_dispatch' AND m.id<>l.movement_id) OR
   (m.kind='transfer_receive' AND (
    m.warehouse_id<>h.destination_warehouse_id OR m.sku<>l.sku OR m.lot<>l.lot OR m.from_location IS NOT NULL OR m.to_location IS NULL OR m.reference<>transfer
    OR (SELECT kind FROM wms_locations WHERE tenant=t AND warehouse_id=m.warehouse_id AND code=m.to_location) IS DISTINCT FROM 'receiving'
    OR e.tenant IS DISTINCT FROM t OR e.warehouse_id IS DISTINCT FROM h.destination_warehouse_id OR e.version<=0
    OR e.version>(SELECT version FROM wms_warehouses WHERE tenant=t AND warehouse_id=h.destination_warehouse_id)
    OR e.payload->>'type' IS DISTINCT FROM 'transfer.received' OR e.payload->>'id' IS DISTINCT FROM e.id
    OR e.payload->>'tenant' IS DISTINCT FROM t OR e.payload->>'warehouse_id' IS DISTINCT FROM h.destination_warehouse_id
    OR e.payload->'version' IS DISTINCT FROM to_jsonb(e.version) OR e.payload->>'actor' IS DISTINCT FROM m.data->>'actor'
    OR e.payload#>>'{command,type}' IS DISTINCT FROM 'receive_transfer' OR e.payload#>>'{command,transfer_id}' IS DISTINCT FROM transfer
    OR e.payload#>>'{command,line_id}' IS DISTINCT FROM l.id OR e.payload#>'{command,quantity}' IS DISTINCT FROM to_jsonb(m.quantity)
    OR e.payload#>>'{command,location}' IS DISTINCT FROM m.to_location OR e.payload#>>'{command,arrival_reference}' IS DISTINCT FROM m.data->>'reason'
    OR COALESCE(length(btrim(m.data->>'reason')),0)=0
    OR e.payload#>>'{result,type}' IS DISTINCT FROM 'transfer.received' OR e.payload#>>'{result,entity_id}' IS DISTINCT FROM transfer OR e.payload#>'{result,version}' IS DISTINCT FROM to_jsonb(e.version)
   )))) THEN RETURN false; END IF;
 RETURN true;
END $$;
CREATE FUNCTION wms_transfer_document(t TEXT,transfer TEXT) RETURNS JSONB LANGUAGE SQL STABLE AS $$
 SELECT to_jsonb(h)-'tenant'||jsonb_build_object('lines',(
  SELECT jsonb_agg(to_jsonb(l)-'tenant'-'transfer_id'-'source_warehouse_id'-'destination_warehouse_id'||jsonb_build_object('received',
   (SELECT COALESCE(sum(m.quantity),0)::bigint FROM wms_movements m WHERE m.tenant=t AND m.transfer_id=transfer AND m.transfer_line_id=l.id AND m.kind='transfer_receive')) ORDER BY l.id)
  FROM wms_transfer_lines l WHERE l.tenant=t AND l.transfer_id=transfer))
 FROM wms_transfers h WHERE h.tenant=t AND h.id=transfer;
$$;
CREATE FUNCTION wms_transfer_open(t TEXT,transfer TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_transfer_lines l WHERE l.tenant=t AND l.transfer_id=transfer AND l.quantity>
  (SELECT COALESCE(sum(m.quantity),0) FROM wms_movements m WHERE m.tenant=t AND m.transfer_id=transfer AND m.transfer_line_id=l.id AND m.kind='transfer_receive'));
$$;
CREATE FUNCTION wms_transfer_lock() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE w TEXT;
BEGIN
 IF TG_TABLE_NAME='wms_transfers' THEN
  FOR w IN SELECT x FROM unnest(ARRAY[NEW.source_warehouse_id,NEW.destination_warehouse_id]) x ORDER BY x LOOP
   PERFORM wms_serialize_shipment(NEW.tenant,w);
  END LOOP;
 ELSE
  PERFORM wms_serialize_shipment(NEW.tenant,NEW.warehouse_id);
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_transfer_lock BEFORE INSERT ON wms_transfers FOR EACH ROW EXECUTE FUNCTION wms_transfer_lock();
CREATE TRIGGER wms_transfer_movement_lock BEFORE INSERT ON wms_movements FOR EACH ROW WHEN(NEW.transfer_id IS NOT NULL) EXECUTE FUNCTION wms_transfer_lock();
CREATE FUNCTION wms_transfer_commit_proof() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE transfer TEXT;
BEGIN
 IF TG_TABLE_NAME='wms_transfers' THEN transfer=NEW.id; ELSE transfer=NEW.transfer_id; END IF;
 IF NOT wms_transfer_consistent(NEW.tenant,transfer) THEN RAISE EXCEPTION 'Transfer lacks exact original dispatch or arrival evidence' USING ERRCODE='23514'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_transfer_commit_proof AFTER INSERT ON wms_transfers DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_transfer_commit_proof();
CREATE CONSTRAINT TRIGGER wms_transfer_line_proof AFTER INSERT ON wms_transfer_lines DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_transfer_commit_proof();
CREATE CONSTRAINT TRIGGER wms_transfer_movement_proof AFTER INSERT ON wms_movements DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN(NEW.transfer_id IS NOT NULL) EXECUTE FUNCTION wms_transfer_commit_proof();
