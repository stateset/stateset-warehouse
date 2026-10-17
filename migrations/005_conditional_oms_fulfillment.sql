-- A physical shipment can create one immutable conditional OMS fulfillment.
-- Existing native and transport contracts remain unchanged.
ALTER TABLE wms_native_requests DROP CONSTRAINT wms_native_requests_kind_check;
ALTER TABLE wms_native_requests ADD CONSTRAINT wms_native_requests_kind_check
 CHECK(kind IN('oms_import','tms_transport','oms_fulfillment'));
ALTER TABLE wms_native_requests ADD CONSTRAINT wms_native_fulfillment_identity
 CHECK(kind<>'oms_fulfillment' OR COALESCE(
   order_id IS NOT NULL
   AND frozen->>'tenant'=tenant
   AND frozen->>'warehouse_id'=warehouse_id
   AND frozen->>'order_id'=order_id
   AND length(frozen->>'event_id')>0,
 false));
CREATE UNIQUE INDEX wms_native_one_fulfillment_per_order
 ON wms_native_requests(tenant,warehouse_id,order_id) WHERE kind='oms_fulfillment';
CREATE UNIQUE INDEX wms_native_one_fulfillment_per_event
 ON wms_native_requests(tenant,warehouse_id,(frozen->>'event_id')) WHERE kind='oms_fulfillment';

CREATE FUNCTION wms_native_fulfillment_shipment() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.kind='oms_fulfillment' AND NOT EXISTS(
   SELECT 1 FROM wms_events e
   WHERE e.id=NEW.frozen->>'event_id'
     AND e.tenant=NEW.tenant AND e.warehouse_id=NEW.warehouse_id
     AND e.payload->>'type'='fulfillment.shipped'
     AND e.payload->'order'->>'id'=NEW.order_id
     AND e.payload->'order'->>'status'='shipped'
     AND NEW.frozen#>>'{payload,event_id}'=e.id
     AND NEW.frozen#>>'{payload,warehouse_id}'=NEW.warehouse_id
     AND NEW.frozen#>>'{payload,warehouse_order_id}'=NEW.order_id
     AND NEW.frozen#>>'{payload,carrier}'=e.payload#>>'{order,carrier}'
     AND NEW.frozen#>>'{payload,handoff_reference}'=e.payload#>>'{order,handoff_reference}'
     AND NEW.frozen#>>'{payload,handed_off_at}'=e.payload#>>'{order,shipped_at}'
     AND e.payload#>>'{order,source_order_id}'=
       'oms:'||(NEW.frozen->>'recipient_tenant_id')||':'||(NEW.frozen#>>'{payload,order_id}')
     AND jsonb_typeof(NEW.frozen#>'{payload,lines}')='array'
     AND (
       SELECT jsonb_agg(jsonb_build_array(l->>'line_id',l->>'sku',l->'quantity_each') ORDER BY l->>'line_id')
       FROM jsonb_array_elements(NEW.frozen#>'{payload,lines}') l
     ) = (
       SELECT jsonb_agg(jsonb_build_array(l->>'id',l->>'sku',l->'quantity') ORDER BY l->>'id')
       FROM jsonb_array_elements(e.payload#>'{order,lines}') l
     )
 ) THEN
   RAISE EXCEPTION 'Conditional OMS fulfillment requires its committed physical shipment event';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_native_fulfillment_shipment_guard
 BEFORE INSERT ON wms_native_requests
 FOR EACH ROW EXECUTE FUNCTION wms_native_fulfillment_shipment();
