-- Preserve migration021's exact target rows and bigint conversion behavior.
-- Direct counts/moves need no receipt/order/transfer lookups. Dispatch before
-- planning that general UNION, while every other command retains the original
-- resolver and whitelist. Extra quantity/line fields also use that resolver:
-- its UNION planning can reject their malformed casts even in inactive arms.
-- No source verdict is cached or skipped.
CREATE OR REPLACE FUNCTION wms_serial_targets(t TEXT,w TEXT,c JSONB)
 RETURNS TABLE(target JSONB,sku TEXT,each_quantity BIGINT,allow_zero BOOLEAN)
 LANGUAGE plpgsql STABLE AS $$
BEGIN
 IF c->>'type'='move_stock' AND NOT (c ?| ARRAY['counted','lines']) THEN
  RETURN QUERY SELECT '[]'::jsonb,c->>'sku',(c->>'quantity')::bigint,false;
 ELSIF c->>'type'='count_stock' AND NOT (c ?| ARRAY['quantity','lines']) THEN
  RETURN QUERY SELECT '[]'::jsonb,c->>'sku',(c->>'counted')::bigint,true;
 ELSE
  RETURN QUERY SELECT q.* FROM wms_unit_targets(t,w,c) q WHERE c->>'type' IN(
   'count_stock','move_stock','receive','pick','unpick','create_return','pack_shipment',
   'dispatch_transfer','receive_transfer','record_transfer_loss','reverse_transfer_loss');
 END IF;
END $$;
