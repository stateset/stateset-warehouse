-- Operational rows are separate from append-only history. No warehouse-sized blob.
CREATE TABLE IF NOT EXISTS wms_warehouses (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, version BIGINT NOT NULL CHECK(version>=0),
 metadata JSONB NOT NULL, PRIMARY KEY(tenant,warehouse_id),
 CHECK(COALESCE(metadata->>'id'=warehouse_id,false))
);
CREATE TABLE IF NOT EXISTS wms_products (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, sku TEXT NOT NULL, data JSONB NOT NULL,
 PRIMARY KEY(tenant,warehouse_id,sku), CHECK(COALESCE(data->>'sku'=sku,false)),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE TABLE IF NOT EXISTS wms_locations (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, code TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('receiving','storage','staging')), data JSONB NOT NULL,
 PRIMARY KEY(tenant,warehouse_id,code), CHECK(COALESCE(data->>'code'=code AND data->>'kind'=kind,false)),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE TABLE IF NOT EXISTS wms_stock (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, sku TEXT NOT NULL, lot TEXT NOT NULL,
 location TEXT NOT NULL, on_hand BIGINT NOT NULL CHECK(on_hand BETWEEN 0 AND 1000000000),
 reserved BIGINT NOT NULL CHECK(reserved BETWEEN 0 AND on_hand),
 PRIMARY KEY(tenant,warehouse_id,sku,lot,location),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku),
 FOREIGN KEY(tenant,warehouse_id,location) REFERENCES wms_locations(tenant,warehouse_id,code)
);
CREATE TABLE IF NOT EXISTS wms_stock_ledger (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, sku TEXT NOT NULL, lot TEXT NOT NULL,
 location TEXT NOT NULL, quantity BIGINT NOT NULL,
 PRIMARY KEY(tenant,warehouse_id,sku,lot,location)
);
CREATE TABLE IF NOT EXISTS wms_stock_reserved (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, sku TEXT NOT NULL, lot TEXT NOT NULL,
 location TEXT NOT NULL, quantity BIGINT NOT NULL,
 PRIMARY KEY(tenant,warehouse_id,sku,lot,location)
);
CREATE TABLE IF NOT EXISTS wms_orders (
 seq BIGSERIAL UNIQUE, tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, id TEXT NOT NULL,
 reference TEXT NOT NULL, source_order_id TEXT,
 status TEXT NOT NULL CHECK(status IN('pending','allocated','picking','picked','packed','shipped','cancelled')),
 data JSONB NOT NULL, PRIMARY KEY(tenant,warehouse_id,id),
 UNIQUE(tenant,warehouse_id,reference), UNIQUE(tenant,warehouse_id,source_order_id),
 CHECK(COALESCE(data->>'id'=id AND data->>'status'=status AND data->>'reference'=reference,false)),
 CHECK((data->>'source_order_id') IS NOT DISTINCT FROM source_order_id),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE INDEX IF NOT EXISTS wms_orders_open ON wms_orders(tenant,warehouse_id,seq) WHERE status NOT IN('shipped','cancelled');
CREATE INDEX IF NOT EXISTS wms_orders_page ON wms_orders(tenant,warehouse_id,seq DESC);
CREATE TABLE IF NOT EXISTS wms_order_lines (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, order_id TEXT NOT NULL, id TEXT NOT NULL,
 sku TEXT NOT NULL, quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000),
 PRIMARY KEY(tenant,warehouse_id,order_id,id), UNIQUE(tenant,warehouse_id,order_id,id,sku),
 FOREIGN KEY(tenant,warehouse_id,order_id) REFERENCES wms_orders(tenant,warehouse_id,id),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku)
);
CREATE TABLE IF NOT EXISTS wms_allocations (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, order_id TEXT NOT NULL, line_id TEXT NOT NULL,
 sku TEXT NOT NULL, lot TEXT NOT NULL, location TEXT NOT NULL,
 quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000),
 picked BIGINT NOT NULL CHECK(picked>=0), shipped BIGINT NOT NULL CHECK(shipped>=0),
 released BIGINT NOT NULL CHECK(released>=0),
 CHECK(picked+released<=quantity AND shipped<=picked),
 PRIMARY KEY(tenant,warehouse_id,order_id,line_id,location,lot),
 FOREIGN KEY(tenant,warehouse_id,order_id,line_id,sku) REFERENCES wms_order_lines(tenant,warehouse_id,order_id,id,sku),
 FOREIGN KEY(tenant,warehouse_id,location) REFERENCES wms_locations(tenant,warehouse_id,code)
);
CREATE INDEX IF NOT EXISTS wms_allocations_stock ON wms_allocations(tenant,warehouse_id,sku,lot,location);
CREATE TABLE IF NOT EXISTS wms_receipts (
 seq BIGSERIAL UNIQUE, tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, id TEXT NOT NULL,
 reference TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN('expected','partial','received')),
 data JSONB NOT NULL, PRIMARY KEY(tenant,warehouse_id,id), UNIQUE(tenant,warehouse_id,reference),
 CHECK(COALESCE(data->>'id'=id AND data->>'status'=status AND data->>'reference'=reference,false)),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE INDEX IF NOT EXISTS wms_receipts_open ON wms_receipts(tenant,warehouse_id,seq) WHERE status<>'received';
CREATE INDEX IF NOT EXISTS wms_receipts_page ON wms_receipts(tenant,warehouse_id,seq DESC);
CREATE TABLE IF NOT EXISTS wms_receipt_lines (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, receipt_id TEXT NOT NULL, id TEXT NOT NULL,
 sku TEXT NOT NULL, lot TEXT NOT NULL, quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000),
 received BIGINT NOT NULL CHECK(received BETWEEN 0 AND quantity),
 PRIMARY KEY(tenant,warehouse_id,receipt_id,id), UNIQUE(tenant,warehouse_id,receipt_id,id,sku,lot),
 FOREIGN KEY(tenant,warehouse_id,receipt_id) REFERENCES wms_receipts(tenant,warehouse_id,id),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku)
);
CREATE TABLE IF NOT EXISTS wms_receipt_evidence (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, receipt_id TEXT NOT NULL, line_id TEXT NOT NULL,
 quantity BIGINT NOT NULL, PRIMARY KEY(tenant,warehouse_id,receipt_id,line_id)
);
CREATE TABLE IF NOT EXISTS wms_exceptions (
 seq BIGSERIAL UNIQUE, tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, id TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN('open','resolved')), order_id TEXT, data JSONB NOT NULL,
 PRIMARY KEY(tenant,warehouse_id,id), FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE INDEX IF NOT EXISTS wms_exceptions_page ON wms_exceptions(tenant,warehouse_id,seq DESC);
CREATE TABLE IF NOT EXISTS wms_movements (
 seq BIGSERIAL UNIQUE, id TEXT PRIMARY KEY, tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL,
 sku TEXT NOT NULL, lot TEXT NOT NULL, from_location TEXT, to_location TEXT,
 quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000000),
 kind TEXT NOT NULL CHECK(kind IN('receipt','putaway','pick','unpick','shipment','count')),
 reference TEXT NOT NULL, receipt_id TEXT, receipt_line_id TEXT, data JSONB NOT NULL,
 CHECK(from_location IS DISTINCT FROM to_location),
 CHECK((kind='receipt' AND receipt_id IS NOT NULL AND receipt_line_id IS NOT NULL AND from_location IS NULL AND to_location IS NOT NULL)
    OR (kind<>'receipt' AND receipt_id IS NULL AND receipt_line_id IS NULL)),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku),
 FOREIGN KEY(tenant,warehouse_id,from_location) REFERENCES wms_locations(tenant,warehouse_id,code),
 FOREIGN KEY(tenant,warehouse_id,to_location) REFERENCES wms_locations(tenant,warehouse_id,code),
 FOREIGN KEY(tenant,warehouse_id,receipt_id,receipt_line_id,sku,lot) REFERENCES wms_receipt_lines(tenant,warehouse_id,receipt_id,id,sku,lot)
);
CREATE INDEX IF NOT EXISTS wms_movements_page ON wms_movements(tenant,warehouse_id,seq DESC);
CREATE INDEX IF NOT EXISTS wms_movements_receipt ON wms_movements(tenant,warehouse_id,receipt_id,receipt_line_id) WHERE receipt_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS wms_activity (
 seq BIGSERIAL UNIQUE, id TEXT PRIMARY KEY, tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL,
 kind TEXT NOT NULL, data JSONB NOT NULL,
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
CREATE INDEX IF NOT EXISTS wms_activity_page ON wms_activity(tenant,warehouse_id,seq DESC);

CREATE OR REPLACE FUNCTION wms_apply_movement() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.from_location IS NOT NULL THEN
  INSERT INTO wms_stock_ledger VALUES(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,NEW.from_location,-NEW.quantity)
  ON CONFLICT(tenant,warehouse_id,sku,lot,location) DO UPDATE SET quantity=wms_stock_ledger.quantity+EXCLUDED.quantity;
 END IF;
 IF NEW.to_location IS NOT NULL THEN
  INSERT INTO wms_stock_ledger VALUES(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,NEW.to_location,NEW.quantity)
  ON CONFLICT(tenant,warehouse_id,sku,lot,location) DO UPDATE SET quantity=wms_stock_ledger.quantity+EXCLUDED.quantity;
 END IF;
 IF NEW.kind='receipt' THEN
  IF (SELECT kind FROM wms_locations WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND code=NEW.to_location)<>'receiving' THEN
   RAISE EXCEPTION 'Receipt must enter a receiving location' USING ERRCODE='23514';
  END IF;
  INSERT INTO wms_receipt_evidence VALUES(NEW.tenant,NEW.warehouse_id,NEW.receipt_id,NEW.receipt_line_id,NEW.quantity)
  ON CONFLICT(tenant,warehouse_id,receipt_id,line_id) DO UPDATE SET quantity=wms_receipt_evidence.quantity+EXCLUDED.quantity;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_movement_ledger AFTER INSERT ON wms_movements FOR EACH ROW EXECUTE FUNCTION wms_apply_movement();

CREATE OR REPLACE FUNCTION wms_reserved_delta(t TEXT,w TEXT,s TEXT,l TEXT,b TEXT,q BIGINT) RETURNS VOID LANGUAGE SQL AS $$
 INSERT INTO wms_stock_reserved VALUES(t,w,s,l,b,q)
 ON CONFLICT(tenant,warehouse_id,sku,lot,location) DO UPDATE SET quantity=wms_stock_reserved.quantity+EXCLUDED.quantity;
$$;
CREATE OR REPLACE FUNCTION wms_apply_allocation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP IN('UPDATE','DELETE') THEN
  PERFORM wms_reserved_delta(OLD.tenant,OLD.warehouse_id,OLD.sku,OLD.lot,OLD.location,-(OLD.quantity-OLD.picked-OLD.released));
  PERFORM wms_reserved_delta(OLD.tenant,OLD.warehouse_id,OLD.sku,OLD.lot,'STAGE-01',-(OLD.picked-OLD.shipped));
 END IF;
 IF TG_OP IN('UPDATE','INSERT') THEN
  IF (SELECT kind FROM wms_locations WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND code=NEW.location)<>'storage' THEN
   RAISE EXCEPTION 'Allocation source must be storage' USING ERRCODE='23514';
  END IF;
  PERFORM wms_reserved_delta(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,NEW.location,NEW.quantity-NEW.picked-NEW.released);
  PERFORM wms_reserved_delta(NEW.tenant,NEW.warehouse_id,NEW.sku,NEW.lot,'STAGE-01',NEW.picked-NEW.shipped);
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER wms_allocation_reservations AFTER INSERT OR UPDATE OR DELETE ON wms_allocations FOR EACH ROW EXECUTE FUNCTION wms_apply_allocation();

CREATE OR REPLACE FUNCTION wms_check_stock() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE t TEXT; w TEXT; s TEXT; l TEXT; b TEXT; physical BIGINT; held BIGINT; ledger BIGINT; allocated BIGINT;
BEGIN
 IF TG_OP='DELETE' THEN t=OLD.tenant;w=OLD.warehouse_id;s=OLD.sku;l=OLD.lot;b=OLD.location;
 ELSE t=NEW.tenant;w=NEW.warehouse_id;s=NEW.sku;l=NEW.lot;b=NEW.location; END IF;
 SELECT on_hand,reserved INTO physical,held FROM wms_stock WHERE tenant=t AND warehouse_id=w AND sku=s AND lot=l AND location=b;
 SELECT quantity INTO ledger FROM wms_stock_ledger WHERE tenant=t AND warehouse_id=w AND sku=s AND lot=l AND location=b;
 SELECT quantity INTO allocated FROM wms_stock_reserved WHERE tenant=t AND warehouse_id=w AND sku=s AND lot=l AND location=b;
 IF COALESCE(physical,0)<>COALESCE(ledger,0) OR COALESCE(held,0)<>COALESCE(allocated,0) THEN
  RAISE EXCEPTION 'Warehouse physical ledger/reservation mismatch for %/%/%',s,l,b USING ERRCODE='23514';
 END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_stock_proof AFTER INSERT OR UPDATE OR DELETE ON wms_stock DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_stock();
CREATE CONSTRAINT TRIGGER wms_ledger_proof AFTER INSERT OR UPDATE OR DELETE ON wms_stock_ledger DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_stock();
CREATE CONSTRAINT TRIGGER wms_reserved_proof AFTER INSERT OR UPDATE OR DELETE ON wms_stock_reserved DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_stock();

CREATE OR REPLACE FUNCTION wms_check_receipt() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE t TEXT;w TEXT;r TEXT;l TEXT;actual BIGINT;proof BIGINT;
BEGIN
 IF TG_OP='DELETE' THEN t=OLD.tenant;w=OLD.warehouse_id;r=OLD.receipt_id;
 ELSE t=NEW.tenant;w=NEW.warehouse_id;r=NEW.receipt_id; END IF;
 IF TG_TABLE_NAME='wms_receipt_lines' THEN
  IF TG_OP='DELETE' THEN l=OLD.id; ELSE l=NEW.id; END IF;
 ELSE IF TG_OP='DELETE' THEN l=OLD.line_id; ELSE l=NEW.line_id; END IF; END IF;
 SELECT received INTO actual FROM wms_receipt_lines WHERE tenant=t AND warehouse_id=w AND receipt_id=r AND id=l;
 SELECT quantity INTO proof FROM wms_receipt_evidence WHERE tenant=t AND warehouse_id=w AND receipt_id=r AND line_id=l;
 IF COALESCE(actual,0)<>COALESCE(proof,0) THEN RAISE EXCEPTION 'Receipt execution lacks movement evidence' USING ERRCODE='23514'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_receipt_line_proof AFTER INSERT OR UPDATE OR DELETE ON wms_receipt_lines DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_receipt();
CREATE CONSTRAINT TRIGGER wms_receipt_evidence_proof AFTER INSERT OR UPDATE OR DELETE ON wms_receipt_evidence DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_receipt();
CREATE OR REPLACE FUNCTION wms_immutable_history() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 RAISE EXCEPTION 'Warehouse history is append-only' USING ERRCODE='23514';
END $$;
CREATE TRIGGER wms_movements_immutable BEFORE UPDATE OR DELETE ON wms_movements FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_activity_immutable BEFORE UPDATE OR DELETE ON wms_activity FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_events_immutable BEFORE UPDATE OR DELETE ON wms_events FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
-- Prevent an old API process from becoming a second inventory authority.
CREATE OR REPLACE FUNCTION wms_fence_legacy_writes() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
 RAISE EXCEPTION 'Legacy warehouse writes fenced: relational storage is active; upgrade the API process' USING ERRCODE='23514';
END $$;
CREATE TRIGGER wms_legacy_write_fence BEFORE INSERT OR UPDATE OR DELETE ON wms_workspaces FOR EACH ROW EXECUTE FUNCTION wms_fence_legacy_writes();
