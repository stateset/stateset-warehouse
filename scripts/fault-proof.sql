-- Fail the final outbox write after stock, ledger, event and receipt writes.
-- The failed subtransaction must leave every persisted component unchanged.
DO $$ BEGIN
 BEGIN
  UPDATE wms_stock SET on_hand=9 WHERE tenant='sql-proof' AND location='A';
  INSERT INTO wms_movements(id,tenant,warehouse_id,sku,lot,from_location,to_location,quantity,kind,reference,data)
   VALUES('proof-final-write-fault','sql-proof','wh-proof','SKU','LOT',NULL,'A',1,'count','COUNT-FAULT','{}');
  INSERT INTO wms_events(id,tenant,warehouse_id,version,payload)
   VALUES('proof-fault-event','sql-proof','wh-proof',1,'{}');
  INSERT INTO wms_commands(tenant,warehouse_id,request_id,actor,digest,result)
   VALUES('sql-proof','wh-proof','proof-fault-request','sql-proof','proof-digest','{}');
  INSERT INTO wms_outbox(id,event_id,tenant,destination)
   VALUES('proof-fault-outbox','proof-fault-event','sql-proof','invalid-destination');
  RAISE EXCEPTION 'fault injection did not reject invalid final delivery write';
 EXCEPTION WHEN check_violation THEN NULL;
 END;
 IF EXISTS(SELECT 1 FROM wms_movements WHERE id='proof-final-write-fault')
  OR EXISTS(SELECT 1 FROM wms_events WHERE id='proof-fault-event')
  OR EXISTS(SELECT 1 FROM wms_commands WHERE request_id='proof-fault-request')
  OR EXISTS(SELECT 1 FROM wms_outbox WHERE id='proof-fault-outbox') THEN
  RAISE EXCEPTION 'final write failure did not roll back movements, events, receipt, and outbox';
 END IF;
 IF (SELECT on_hand FROM wms_stock WHERE tenant='sql-proof' AND location='A')<>8
  OR (SELECT quantity FROM wms_stock_ledger WHERE tenant='sql-proof' AND location='A')<>8 THEN
  RAISE EXCEPTION 'final write failure did not roll back operational stock and trigger-owned ledger';
 END IF;
END $$;
SELECT 'PASS:atomic_outbox_rollback' AS wms_sql_proof;
