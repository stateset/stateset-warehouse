-- Authorization consumes a shipment's return allowance at creation, so several
-- open RMAs cannot collectively receive more than the original line and lot.
CREATE TABLE wms_return_lines (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, receipt_id TEXT NOT NULL,
 line_id TEXT NOT NULL, order_id TEXT NOT NULL, order_line_id TEXT NOT NULL,
 sku TEXT NOT NULL, lot TEXT NOT NULL, quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000),
 reason TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 240),
 PRIMARY KEY(tenant,warehouse_id,receipt_id,line_id),
 FOREIGN KEY(tenant,warehouse_id,receipt_id,line_id) REFERENCES wms_receipt_lines(tenant,warehouse_id,receipt_id,id),
 FOREIGN KEY(tenant,warehouse_id,order_id,order_line_id,sku) REFERENCES wms_order_lines(tenant,warehouse_id,order_id,id,sku)
);
CREATE INDEX wms_returns_source ON wms_return_lines(tenant,warehouse_id,order_id,order_line_id,lot);
CREATE TRIGGER wms_return_lines_immutable BEFORE UPDATE OR DELETE ON wms_return_lines FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_return_lines_no_truncate BEFORE TRUNCATE ON wms_return_lines FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_return_owner_revision AFTER INSERT ON wms_return_lines FOR EACH ROW EXECUTE FUNCTION wms_track_resources();
CREATE FUNCTION wms_check_return_limit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE shipped BIGINT; authorized BIGINT;
BEGIN
 -- Same lock/order as normal physical commands; direct concurrent SQL inserts
 -- also serialize before computing an allowance. No remote I/O holds this lock.
 UPDATE wms_warehouses SET version=version WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id;
 IF NOT EXISTS(SELECT 1 FROM wms_orders WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND id=NEW.order_id AND status='shipped')
 OR NOT EXISTS(SELECT 1 FROM wms_receipt_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND receipt_id=NEW.receipt_id AND id=NEW.line_id AND sku=NEW.sku AND lot=NEW.lot AND quantity=NEW.quantity)
 OR EXISTS(SELECT 1 FROM wms_return_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND receipt_id=NEW.receipt_id AND (order_id<>NEW.order_id OR reason<>NEW.reason)) THEN
  RAISE EXCEPTION 'Return source and exact receipt-line ownership disagree' USING ERRCODE='23514';
 END IF;
 SELECT COALESCE(sum(a.shipped),0) INTO shipped FROM wms_allocations a WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND order_id=NEW.order_id AND line_id=NEW.order_line_id AND lot=NEW.lot;
 SELECT COALESCE(sum(r.quantity),0) INTO authorized FROM wms_return_lines r WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND order_id=NEW.order_id AND order_line_id=NEW.order_line_id AND lot=NEW.lot;
 IF authorized>shipped THEN RAISE EXCEPTION 'Returns exceed shipped line/lot quantity' USING ERRCODE='23514'; END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_return_shipment_proof AFTER INSERT ON wms_return_lines DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_return_limit();
