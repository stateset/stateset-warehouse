-- Cancellation closes only the unreceived remainder. Both the original receipt
-- and the quantities physically received remain permanent evidence.
ALTER TABLE wms_receipts DROP CONSTRAINT wms_receipts_status_check;
ALTER TABLE wms_receipts ADD CONSTRAINT wms_receipts_status_check
 CHECK(status IN('expected','partial','received','cancelled'));
DROP INDEX wms_receipts_open;
CREATE INDEX wms_receipts_open ON wms_receipts(tenant,warehouse_id,seq)
 WHERE status IN('expected','partial');
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM wms_receipts r LEFT JOIN LATERAL (
  SELECT count(*) AS lines,CASE WHEN bool_and(received=0) THEN 'expected'
   WHEN bool_and(received=quantity) THEN 'received' ELSE 'partial' END AS status
  FROM wms_receipt_lines l WHERE l.tenant=r.tenant AND l.warehouse_id=r.warehouse_id AND l.receipt_id=r.id
 ) p ON true WHERE p.lines=0 OR r.status<>p.status) THEN
  RAISE EXCEPTION 'Existing receipt state requires repair before cancellation migration' USING ERRCODE='23514';
 END IF;
 IF EXISTS(SELECT 1 FROM wms_return_lines r JOIN wms_receipt_lines l
  ON l.tenant=r.tenant AND l.warehouse_id=r.warehouse_id AND l.receipt_id=r.receipt_id AND l.id=r.line_id
  WHERE (r.sku,r.lot,r.quantity) IS DISTINCT FROM (l.sku,l.lot,l.quantity))
 OR EXISTS(SELECT 1 FROM wms_receipt_lines l WHERE EXISTS(
  SELECT 1 FROM wms_return_lines r WHERE r.tenant=l.tenant AND r.warehouse_id=l.warehouse_id AND r.receipt_id=l.receipt_id)
  AND NOT EXISTS(SELECT 1 FROM wms_return_lines r WHERE r.tenant=l.tenant AND r.warehouse_id=l.warehouse_id AND r.receipt_id=l.receipt_id AND r.line_id=l.id)) THEN
  RAISE EXCEPTION 'Existing return receipt ownership/expected quantity requires repair before cancellation migration' USING ERRCODE='23514';
 END IF;
END $$;

-- Freeze the original expectation before cancellation as well. Otherwise an
-- altered receipt quantity could release more than its immutable authorization.
CREATE FUNCTION wms_receipt_line_identity_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' OR (NEW.tenant,NEW.warehouse_id,NEW.receipt_id,NEW.id,NEW.sku,NEW.lot,NEW.quantity)
  IS DISTINCT FROM (OLD.tenant,OLD.warehouse_id,OLD.receipt_id,OLD.id,OLD.sku,OLD.lot,OLD.quantity) THEN
  RAISE EXCEPTION 'Receipt line identity and expected quantity are immutable' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_receipt_line_identity_guard BEFORE UPDATE OR DELETE ON wms_receipt_lines
 FOR EACH ROW EXECUTE FUNCTION wms_receipt_line_identity_guard();
CREATE TABLE wms_receipt_cancellations (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, receipt_id TEXT NOT NULL,
 reason TEXT NOT NULL CHECK(octet_length(reason) BETWEEN 1 AND 240 AND reason=btrim(reason) AND reason!~'[[:cntrl:]]'),
 actor TEXT NOT NULL CHECK(length(btrim(actor))>0),
 cancelled_at TEXT NOT NULL CHECK(cancelled_at ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})$' AND isfinite(cancelled_at::timestamptz)),
 PRIMARY KEY(tenant,warehouse_id,receipt_id),
 FOREIGN KEY(tenant,warehouse_id,receipt_id) REFERENCES wms_receipts(tenant,warehouse_id,id)
);
CREATE TABLE wms_receipt_cancelled_lines (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, receipt_id TEXT NOT NULL, line_id TEXT NOT NULL,
 sku TEXT NOT NULL, lot TEXT NOT NULL,
 quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000),
 received BIGINT NOT NULL CHECK(received BETWEEN 0 AND quantity),
 cancelled BIGINT NOT NULL CHECK(cancelled>=0 AND cancelled=quantity-received),
 PRIMARY KEY(tenant,warehouse_id,receipt_id,line_id),
 FOREIGN KEY(tenant,warehouse_id,receipt_id) REFERENCES wms_receipt_cancellations(tenant,warehouse_id,receipt_id),
 FOREIGN KEY(tenant,warehouse_id,receipt_id,line_id) REFERENCES wms_receipt_lines(tenant,warehouse_id,receipt_id,id)
);
CREATE TRIGGER wms_receipt_cancellations_immutable BEFORE UPDATE OR DELETE ON wms_receipt_cancellations
 FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_receipt_cancellations_no_truncate BEFORE TRUNCATE ON wms_receipt_cancellations
 FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_receipt_cancelled_lines_immutable BEFORE UPDATE OR DELETE ON wms_receipt_cancelled_lines
 FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_receipt_cancelled_lines_no_truncate BEFORE TRUNCATE ON wms_receipt_cancelled_lines
 FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();

CREATE FUNCTION wms_check_receipt_cancellation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE t TEXT; w TEXT; r TEXT; receipt_status TEXT; expected_status TEXT; total BIGINT;
BEGIN
 IF TG_OP='DELETE' THEN t=OLD.tenant; w=OLD.warehouse_id;
 ELSE t=NEW.tenant; w=NEW.warehouse_id; END IF;
 IF TG_TABLE_NAME='wms_receipts' THEN
  IF TG_OP='DELETE' THEN r=OLD.id; ELSE r=NEW.id; END IF;
 ELSE
  IF TG_OP='DELETE' THEN r=OLD.receipt_id; ELSE r=NEW.receipt_id; END IF;
 END IF;
 -- Serialize direct SQL cancellation/receiving as well as application writes.
 -- A competing repeatable-read writer must restart, not accept a stale remainder.
 UPDATE wms_warehouses SET version=version WHERE tenant=t AND warehouse_id=w;
 SELECT status INTO receipt_status FROM wms_receipts WHERE tenant=t AND warehouse_id=w AND id=r;
 IF NOT FOUND THEN RETURN NULL; END IF;
 SELECT count(*),CASE WHEN bool_and(received=0) THEN 'expected'
   WHEN bool_and(received=quantity) THEN 'received' ELSE 'partial' END
 INTO total,expected_status FROM wms_receipt_lines WHERE tenant=t AND warehouse_id=w AND receipt_id=r;
 IF EXISTS(SELECT 1 FROM wms_return_lines WHERE tenant=t AND warehouse_id=w AND receipt_id=r)
  AND EXISTS(SELECT 1 FROM wms_receipt_lines l LEFT JOIN wms_return_lines o
   ON o.tenant=l.tenant AND o.warehouse_id=l.warehouse_id AND o.receipt_id=l.receipt_id AND o.line_id=l.id
   WHERE l.tenant=t AND l.warehouse_id=w AND l.receipt_id=r
    AND (o.line_id IS NULL OR (o.sku,o.lot,o.quantity) IS DISTINCT FROM (l.sku,l.lot,l.quantity))) THEN
  RAISE EXCEPTION 'Every return receipt line must retain its exact original authorization' USING ERRCODE='23514';
 END IF;
 IF EXISTS(SELECT 1 FROM wms_receipt_cancellations WHERE tenant=t AND warehouse_id=w AND receipt_id=r) THEN
  IF receipt_status<>'cancelled' OR total=0
   OR total<>(SELECT count(*) FROM wms_receipt_cancelled_lines WHERE tenant=t AND warehouse_id=w AND receipt_id=r)
   OR NOT EXISTS(SELECT 1 FROM wms_receipt_cancelled_lines WHERE tenant=t AND warehouse_id=w AND receipt_id=r AND cancelled>0)
   OR EXISTS(SELECT 1 FROM wms_receipt_lines l LEFT JOIN wms_receipt_cancelled_lines c
    ON c.tenant=l.tenant AND c.warehouse_id=l.warehouse_id AND c.receipt_id=l.receipt_id AND c.line_id=l.id
    WHERE l.tenant=t AND l.warehouse_id=w AND l.receipt_id=r
      AND (c.line_id IS NULL OR (c.sku,c.lot,c.quantity,c.received,c.cancelled)
        IS DISTINCT FROM (l.sku,l.lot,l.quantity,l.received,l.quantity-l.received))) THEN
   RAISE EXCEPTION 'Receipt differs from immutable cancelled remainder' USING ERRCODE='23514';
  END IF;
 ELSIF total=0 OR receipt_status<>expected_status THEN
  RAISE EXCEPTION 'Receipt status disagrees with physical quantities or cancellation evidence' USING ERRCODE='23514';
 END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER wms_receipt_cancellation_header_proof AFTER INSERT OR UPDATE OR DELETE ON wms_receipts
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_receipt_cancellation();
CREATE CONSTRAINT TRIGGER wms_receipt_cancellation_line_proof AFTER INSERT OR UPDATE OR DELETE ON wms_receipt_lines
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_receipt_cancellation();
CREATE CONSTRAINT TRIGGER wms_receipt_cancellation_proof AFTER INSERT ON wms_receipt_cancellations
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_receipt_cancellation();
CREATE CONSTRAINT TRIGGER wms_receipt_cancelled_quantity_proof AFTER INSERT ON wms_receipt_cancelled_lines
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_receipt_cancellation();
CREATE CONSTRAINT TRIGGER wms_return_receipt_identity_proof AFTER INSERT ON wms_return_lines
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_check_receipt_cancellation();

-- Only frozen, unreceived cancellation quantities release return allowance.
-- Original authorizations and exact shipped line/lot ownership stay immutable.
CREATE OR REPLACE FUNCTION wms_check_return_limit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE shipped BIGINT; authorized BIGINT;
BEGIN
 UPDATE wms_warehouses SET version=version WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id;
 IF NOT EXISTS(SELECT 1 FROM wms_orders WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND id=NEW.order_id AND status='shipped')
 OR NOT EXISTS(SELECT 1 FROM wms_receipt_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND receipt_id=NEW.receipt_id AND id=NEW.line_id AND sku=NEW.sku AND lot=NEW.lot AND quantity=NEW.quantity)
 OR EXISTS(SELECT 1 FROM wms_return_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND receipt_id=NEW.receipt_id AND (order_id<>NEW.order_id OR reason<>NEW.reason)) THEN
  RAISE EXCEPTION 'Return source and exact receipt-line ownership disagree' USING ERRCODE='23514';
 END IF;
 SELECT COALESCE(sum(a.shipped),0) INTO shipped FROM wms_allocations a WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND order_id=NEW.order_id AND line_id=NEW.order_line_id AND lot=NEW.lot;
 SELECT COALESCE(sum(r.quantity-COALESCE(c.cancelled,0)),0) INTO authorized
 FROM wms_return_lines r LEFT JOIN wms_receipt_cancelled_lines c
 ON c.tenant=r.tenant AND c.warehouse_id=r.warehouse_id AND c.receipt_id=r.receipt_id AND c.line_id=r.line_id
 WHERE r.tenant=NEW.tenant AND r.warehouse_id=NEW.warehouse_id AND r.order_id=NEW.order_id AND r.order_line_id=NEW.order_line_id AND r.lot=NEW.lot;
 IF authorized>shipped THEN RAISE EXCEPTION 'Returns exceed shipped line/lot quantity' USING ERRCODE='23514'; END IF;
 RETURN NULL;
END $$;
