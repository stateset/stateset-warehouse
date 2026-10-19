-- Database-owned revisions fence even an ABA balance change. Existing resources
-- start at revision zero; no synthetic historical revisions are invented.
CREATE TABLE wms_resource_versions (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, resource TEXT NOT NULL,
 revision BIGINT NOT NULL CHECK(revision > 0),
 PRIMARY KEY(tenant,warehouse_id,resource),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id)
);
-- jsonb::text inserts spaces; the stored key uses the same compact encoding as
-- JSON.stringify/serde_json without removing spaces inside identifier strings.
CREATE FUNCTION wms_bump_resource(t TEXT,w TEXT,k JSONB) RETURNS VOID LANGUAGE SQL AS $$
 INSERT INTO wms_resource_versions VALUES(t,w,
   '['||(SELECT string_agg(to_json(value)::text,',' ORDER BY ordinality)
          FROM jsonb_array_elements_text(k) WITH ORDINALITY)||']',1)
 ON CONFLICT(tenant,warehouse_id,resource) DO UPDATE SET revision=wms_resource_versions.revision+1;
$$;
CREATE FUNCTION wms_revision_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' OR NEW.tenant<>OLD.tenant OR NEW.warehouse_id<>OLD.warehouse_id
    OR NEW.resource<>OLD.resource OR NEW.revision<>OLD.revision+1 THEN
  RAISE EXCEPTION 'Resource revisions are monotonic and cannot be removed' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_resource_revision_guard BEFORE UPDATE OR DELETE ON wms_resource_versions
 FOR EACH ROW EXECUTE FUNCTION wms_revision_guard();
CREATE TRIGGER wms_resource_no_truncate BEFORE TRUNCATE ON wms_resource_versions
 FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE FUNCTION wms_track_resources() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE r RECORD;
BEGIN
 IF TG_OP='UPDATE' AND NEW IS NOT DISTINCT FROM OLD THEN RETURN NULL; END IF;
 IF TG_OP='UPDATE' AND TG_TABLE_NAME='wms_orders' THEN
  IF (to_jsonb(NEW)-'data') IS NOT DISTINCT FROM (to_jsonb(OLD)-'data')
   AND (NEW.data-'lines'-'allocations') IS NOT DISTINCT FROM (OLD.data-'lines'-'allocations') THEN RETURN NULL; END IF;
 ELSIF TG_OP='UPDATE' AND TG_TABLE_NAME='wms_receipts' THEN
  IF (to_jsonb(NEW)-'data') IS NOT DISTINCT FROM (to_jsonb(OLD)-'data')
   AND (NEW.data-'lines'-'return_source') IS NOT DISTINCT FROM (OLD.data-'lines'-'return_source') THEN RETURN NULL; END IF;
 END IF;
 IF TG_OP='DELETE' THEN r=OLD; ELSE r=NEW; END IF;
 IF TG_TABLE_NAME='wms_stock' THEN
  PERFORM wms_bump_resource(r.tenant,r.warehouse_id,jsonb_build_array('stock',r.sku,r.lot,r.location));
  PERFORM wms_bump_resource(r.tenant,r.warehouse_id,jsonb_build_array('inventory',r.sku));
 ELSIF TG_TABLE_NAME='wms_products' THEN
  PERFORM wms_bump_resource(r.tenant,r.warehouse_id,jsonb_build_array('product',r.sku));
 ELSIF TG_TABLE_NAME='wms_locations' THEN
  PERFORM wms_bump_resource(r.tenant,r.warehouse_id,jsonb_build_array('location',r.code));
 ELSIF TG_TABLE_NAME IN('wms_orders','wms_receipts') THEN
  PERFORM wms_bump_resource(r.tenant,r.warehouse_id,jsonb_build_array(
    CASE WHEN TG_TABLE_NAME='wms_orders' THEN 'order' ELSE 'receipt' END,r.id));
 ELSIF TG_TABLE_NAME IN('wms_order_lines','wms_allocations') THEN
  PERFORM wms_bump_resource(r.tenant,r.warehouse_id,jsonb_build_array('order',r.order_id));
 ELSIF TG_TABLE_NAME IN('wms_receipt_lines','wms_return_lines') THEN
  PERFORM wms_bump_resource(r.tenant,r.warehouse_id,jsonb_build_array('receipt',r.receipt_id));
 ELSIF TG_TABLE_NAME='wms_exceptions' THEN
  IF r.order_id IS NOT NULL THEN
   PERFORM wms_bump_resource(r.tenant,r.warehouse_id,jsonb_build_array('order',r.order_id));
  END IF;
  IF TG_OP='UPDATE' AND OLD.order_id IS DISTINCT FROM NEW.order_id AND OLD.order_id IS NOT NULL THEN
   PERFORM wms_bump_resource(OLD.tenant,OLD.warehouse_id,jsonb_build_array('order',OLD.order_id));
  END IF;
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER wms_stock_revision AFTER INSERT OR UPDATE OR DELETE ON wms_stock FOR EACH ROW EXECUTE FUNCTION wms_track_resources();
CREATE TRIGGER wms_order_revision AFTER INSERT OR UPDATE OR DELETE ON wms_orders FOR EACH ROW EXECUTE FUNCTION wms_track_resources();
CREATE TRIGGER wms_order_line_revision AFTER INSERT OR UPDATE OR DELETE ON wms_order_lines FOR EACH ROW EXECUTE FUNCTION wms_track_resources();
CREATE TRIGGER wms_allocation_revision AFTER INSERT OR UPDATE OR DELETE ON wms_allocations FOR EACH ROW EXECUTE FUNCTION wms_track_resources();
CREATE TRIGGER wms_receipt_revision AFTER INSERT OR UPDATE OR DELETE ON wms_receipts FOR EACH ROW EXECUTE FUNCTION wms_track_resources();
CREATE TRIGGER wms_receipt_line_revision AFTER INSERT OR UPDATE OR DELETE ON wms_receipt_lines FOR EACH ROW EXECUTE FUNCTION wms_track_resources();
CREATE TRIGGER wms_exception_revision AFTER INSERT OR UPDATE OR DELETE ON wms_exceptions FOR EACH ROW EXECUTE FUNCTION wms_track_resources();
CREATE TRIGGER wms_product_revision AFTER INSERT OR UPDATE OR DELETE ON wms_products FOR EACH ROW EXECUTE FUNCTION wms_track_resources();
CREATE TRIGGER wms_location_revision AFTER INSERT OR UPDATE OR DELETE ON wms_locations FOR EACH ROW EXECUTE FUNCTION wms_track_resources();

-- A witnessed count invalidates a prior physical review even when it reports
-- the same quantity and therefore creates no balance delta/movement. Attach to
-- the committed event so legacy writers and no-op counts follow the same rule.
CREATE FUNCTION wms_track_count_review() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF NEW.payload->'command'->>'type'='count_stock' THEN
  PERFORM wms_bump_resource(NEW.tenant,NEW.warehouse_id,jsonb_build_array('stock',
    NEW.payload->'command'->>'sku',NEW.payload->'command'->>'lot',NEW.payload->'command'->>'location'));
  PERFORM wms_bump_resource(NEW.tenant,NEW.warehouse_id,jsonb_build_array('inventory',NEW.payload->'command'->>'sku'));
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_count_review_revision BEFORE INSERT ON wms_events FOR EACH ROW EXECUTE FUNCTION wms_track_count_review();
