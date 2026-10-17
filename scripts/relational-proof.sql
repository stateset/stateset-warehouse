-- Independent acceptance, run only in check-sql.py's disposable cluster.
BEGIN;
INSERT INTO wms_warehouses VALUES ('sql-proof','wh-proof',0,'{"id":"wh-proof"}');
INSERT INTO wms_products VALUES ('sql-proof','wh-proof','SKU','{"sku":"SKU"}');
INSERT INTO wms_locations VALUES
 ('sql-proof','wh-proof','RECV-01','receiving','{"code":"RECV-01","kind":"receiving"}'),
 ('sql-proof','wh-proof','A','storage','{"code":"A","kind":"storage"}'),
 ('sql-proof','wh-proof','STAGE-01','staging','{"code":"STAGE-01","kind":"staging"}');
INSERT INTO wms_receipts(tenant,warehouse_id,id,reference,status,data) VALUES
 ('sql-proof','wh-proof','R1','ASN-1','received','{"id":"R1","reference":"ASN-1","status":"received"}'),
 ('sql-proof','wh-proof','R2','ASN-2','expected','{"id":"R2","reference":"ASN-2","status":"expected"}');
INSERT INTO wms_receipt_lines VALUES
 ('sql-proof','wh-proof','R1','L1','SKU','LOT',10,10),
 ('sql-proof','wh-proof','R2','L1','SKU','LOT',5,0);
INSERT INTO wms_stock VALUES ('sql-proof','wh-proof','SKU','LOT','RECV-01',10,0);
INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,receipt_id,receipt_line_id,data)
 VALUES('proof-receive','sql-proof','wh-proof','SKU','LOT',NULL,'RECV-01',10,'receipt','ASN-1','R1','L1','{}');
COMMIT;

BEGIN;
INSERT INTO wms_stock VALUES ('sql-proof','wh-proof','SKU','LOT','A',10,7);
UPDATE wms_stock SET on_hand=0 WHERE tenant='sql-proof' AND location='RECV-01';
INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,data)
 VALUES('proof-putaway','sql-proof','wh-proof','SKU','LOT','RECV-01','A',10,'putaway','ASN-1','{}');
INSERT INTO wms_orders(tenant,warehouse_id,id,reference,status,data)
 VALUES('sql-proof','wh-proof','O1','SO-1','allocated','{"id":"O1","reference":"SO-1","status":"allocated"}');
INSERT INTO wms_order_lines VALUES
 ('sql-proof','wh-proof','O1','L1','SKU',4),
 ('sql-proof','wh-proof','O1','L2','SKU',3);
INSERT INTO wms_allocations VALUES
 ('sql-proof','wh-proof','O1','L1','SKU','LOT','A',4,0,0,0),
 ('sql-proof','wh-proof','O1','L2','SKU','LOT','A',3,0,0,0);
COMMIT;

BEGIN;
UPDATE wms_allocations SET picked=2 WHERE tenant='sql-proof' AND line_id='L1';
UPDATE wms_stock SET on_hand=8,reserved=5 WHERE tenant='sql-proof' AND location='A';
INSERT INTO wms_stock VALUES ('sql-proof','wh-proof','SKU','LOT','STAGE-01',2,2);
INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,data)
 VALUES('proof-pick','sql-proof','wh-proof','SKU','LOT','A','STAGE-01',2,'pick','SO-1','{}');
COMMIT;

DO $$ BEGIN
 IF (SELECT sum(on_hand) FROM wms_stock WHERE tenant='sql-proof')<>10 THEN RAISE EXCEPTION 'physical conservation failed'; END IF;
 IF (SELECT sum(quantity) FROM wms_stock_ledger WHERE tenant='sql-proof')<>10 THEN RAISE EXCEPTION 'ledger conservation failed'; END IF;
 IF (SELECT sum(reserved) FROM wms_stock WHERE tenant='sql-proof')<>7 THEN RAISE EXCEPTION 'reservation conservation failed'; END IF;
 IF (SELECT quantity FROM wms_stock_reserved WHERE tenant='sql-proof' AND location='STAGE-01')<>2 THEN RAISE EXCEPTION 'staging ownership failed'; END IF;
 BEGIN
  UPDATE wms_stock SET on_hand=on_hand+1 WHERE tenant='sql-proof' AND location='A';
  SET CONSTRAINTS ALL IMMEDIATE;
  RAISE EXCEPTION 'unrecorded physical stock was incorrectly permitted';
 EXCEPTION WHEN check_violation THEN NULL;
 END;
 BEGIN
  UPDATE wms_stock SET reserved=reserved-1 WHERE tenant='sql-proof' AND location='A';
  SET CONSTRAINTS ALL IMMEDIATE;
  RAISE EXCEPTION 'reservation ownership corruption was incorrectly permitted';
 EXCEPTION WHEN check_violation THEN NULL;
 END;
 BEGIN
  UPDATE wms_receipt_lines SET received=9 WHERE tenant='sql-proof' AND receipt_id='R1';
  SET CONSTRAINTS ALL IMMEDIATE;
  RAISE EXCEPTION 'receipt quantity without exact movement evidence was incorrectly permitted';
 EXCEPTION WHEN check_violation THEN NULL;
 END;
 BEGIN
  UPDATE wms_receipt_lines SET received=1 WHERE tenant='sql-proof' AND receipt_id='R2';
  SET CONSTRAINTS ALL IMMEDIATE;
  RAISE EXCEPTION 'another receipt borrowed evidence for identical SKU and lot';
 EXCEPTION WHEN check_violation THEN NULL;
 END;
 BEGIN
  UPDATE wms_movements SET quantity=quantity+1 WHERE id='proof-receive';
  RAISE EXCEPTION 'movement history mutation was incorrectly permitted';
 EXCEPTION WHEN check_violation THEN NULL;
 END;
 INSERT INTO wms_activity(id,tenant,warehouse_id,kind,data) VALUES('proof-audit','sql-proof','wh-proof','proof','{}');
 BEGIN
  DELETE FROM wms_activity WHERE id='proof-audit';
  RAISE EXCEPTION 'audit deletion was incorrectly permitted';
 EXCEPTION WHEN check_violation THEN NULL;
 END;
END $$;

BEGIN;
UPDATE wms_stock SET on_hand=9 WHERE tenant='sql-proof' AND location='A';
INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,data)
 VALUES('proof-rollback','sql-proof','wh-proof','SKU','LOT',NULL,'A',1,'count','COUNT-ROLLBACK','{}');
ROLLBACK;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM wms_movements WHERE id='proof-rollback') THEN RAISE EXCEPTION 'rolled-back movement survived'; END IF;
 IF (SELECT on_hand FROM wms_stock WHERE tenant='sql-proof' AND location='A')<>8 THEN RAISE EXCEPTION 'rollback changed operational stock'; END IF;
 IF (SELECT quantity FROM wms_stock_ledger WHERE tenant='sql-proof' AND location='A')<>8 THEN RAISE EXCEPTION 'rollback changed movement ledger'; END IF;
END $$;
SELECT 'PASS:relational_inventory_proofs' AS wms_sql_proof;
