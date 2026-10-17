-- Conditional complete-order physical handoff evidence from StateSet WMS.
-- Additive: existing fulfillment/tracking handlers keep their own contracts.
CREATE TABLE warehouse_fulfillment_receipts (
    tenant_id TEXT NOT NULL,
    request_id UUID NOT NULL,
    event_id UUID NOT NULL,
    order_id UUID NOT NULL REFERENCES orders(id),
    request_digest TEXT NOT NULL CHECK(request_digest ~ '^[0-9a-f]{64}$'),
    request JSONB NOT NULL,
    response JSONB NOT NULL,
    applied_updated_at TIMESTAMPTZ,
    observed_updated_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(tenant_id,request_id),
    CHECK(response->>'status' IN('fulfilled','rejected')),
    CHECK((response->>'status'='fulfilled')=(applied_updated_at IS NOT NULL)),
    CHECK(response->>'request_id'=request_id::text AND response->>'event_id'=event_id::text
      AND response->>'order_id'=order_id::text AND response->>'tenant_id'=tenant_id
      AND response->>'request_digest'=request_digest)
);
CREATE UNIQUE INDEX warehouse_fulfillment_one_order ON warehouse_fulfillment_receipts(tenant_id,order_id)
 WHERE response->>'status'='fulfilled';
CREATE UNIQUE INDEX warehouse_fulfillment_one_event ON warehouse_fulfillment_receipts(tenant_id,event_id)
 WHERE response->>'status'='fulfilled';

CREATE FUNCTION warehouse_fulfillment_contract() RETURNS JSONB LANGUAGE SQL IMMUTABLE AS $$
 SELECT '{"contract":"stateset.wms-fulfillment.v1","conditional_revision":true,"terminal_state_fence":true,"durable_idempotency":true,"durable_rejections":true}'::jsonb;
$$;

CREATE FUNCTION warehouse_fulfillment_receipt_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Warehouse fulfillment receipts are immutable' USING ERRCODE='23514';
END $$;
CREATE TRIGGER warehouse_fulfillment_receipt_immutable
BEFORE UPDATE OR DELETE ON warehouse_fulfillment_receipts
FOR EACH ROW EXECUTE FUNCTION warehouse_fulfillment_receipt_immutable();

CREATE FUNCTION apply_warehouse_fulfillment(
    p_tenant TEXT, p_body JSONB, p_digest TEXT
) RETURNS JSONB LANGUAGE plpgsql AS $$
DECLARE
    source orders%ROWTYPE;
    prior warehouse_fulfillment_receipts%ROWTYPE;
    order_uuid UUID;
    request_uuid UUID;
    event_uuid UUID;
    applied_at TIMESTAMPTZ;
    expected_lines JSONB;
    submitted_lines JSONB;
    answer JSONB;
    rejection TEXT;
BEGIN
    IF jsonb_typeof(p_body) IS DISTINCT FROM 'object'
       OR (SELECT count(*) FROM jsonb_object_keys(p_body))<>10
       OR NOT (p_body ?& ARRAY['order_id','request_id','event_id','expected_updated_at','warehouse_id','warehouse_order_id','carrier','handoff_reference','handed_off_at','lines'])
       OR p_digest IS NULL OR p_digest !~ '^[0-9a-f]{64}$'
       OR p_tenant IS NULL OR btrim(p_tenant)=''
    THEN
        RAISE EXCEPTION 'Invalid warehouse handoff envelope' USING ERRCODE='22023';
    END IF;
    IF EXISTS (
        SELECT 1 FROM jsonb_each(p_body) f
        WHERE f.key<>'lines' AND (jsonb_typeof(f.value)<>'string' OR btrim(f.value#>>'{}')='')
    ) OR (p_body->>'warehouse_id') !~ '^[A-Za-z0-9_-]{1,100}$'
      OR octet_length(p_body->>'carrier')>240
      OR octet_length(p_body->>'handoff_reference')>240
      OR jsonb_typeof(p_body->'lines') IS DISTINCT FROM 'array'
    THEN
        RAISE EXCEPTION 'Invalid warehouse handoff fields' USING ERRCODE='22023';
    END IF;
    IF jsonb_array_length(p_body->'lines') NOT BETWEEN 1 AND 100 THEN
        RAISE EXCEPTION 'Warehouse handoff requires 1-100 lines' USING ERRCODE='22023';
    END IF;
    IF EXISTS(
        SELECT 1 FROM jsonb_array_elements(p_body->'lines') l
        WHERE jsonb_typeof(l) IS DISTINCT FROM 'object'
          OR CASE WHEN jsonb_typeof(l)='object' THEN (SELECT count(*) FROM jsonb_object_keys(l))<>3 ELSE true END
          OR NOT (l ?& ARRAY['line_id','sku','quantity_each'])
          OR (l->>'line_id') !~ '^oms-line-[0-9a-f]{64}$'
          OR jsonb_typeof(l->'sku') IS DISTINCT FROM 'string'
          OR btrim(l->>'sku')='' OR btrim(l->>'sku')<>l->>'sku' OR octet_length(l->>'sku')>100
          OR jsonb_typeof(l->'quantity_each') IS DISTINCT FROM 'number'
          OR CASE WHEN jsonb_typeof(l->'quantity_each')='number' THEN
               (l->>'quantity_each')::numeric<>trunc((l->>'quantity_each')::numeric)
               OR (l->>'quantity_each')::numeric NOT BETWEEN 1 AND 1000000 ELSE true END
    ) OR (SELECT count(*)<>count(DISTINCT l->>'line_id') FROM jsonb_array_elements(p_body->'lines') l) THEN
        RAISE EXCEPTION 'Invalid warehouse handoff line envelope' USING ERRCODE='22023';
    END IF;
    order_uuid := (p_body->>'order_id')::uuid;
    request_uuid := (p_body->>'request_id')::uuid;
    event_uuid := (p_body->>'event_id')::uuid;
    PERFORM (p_body->>'warehouse_order_id')::uuid;
    PERFORM (p_body->>'handed_off_at')::timestamptz;
    PERFORM (p_body->>'expected_updated_at')::timestamptz;

    -- Both cancellation and another handoff serialize on this canonical row.
    SELECT * INTO source FROM orders
     WHERE tenant_id=p_tenant AND id=order_uuid FOR UPDATE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'Order does not exist in this tenant' USING ERRCODE='P0002';
    END IF;
    SELECT * INTO prior FROM warehouse_fulfillment_receipts
     WHERE tenant_id=p_tenant AND request_id=request_uuid;
    IF FOUND THEN
        IF prior.order_id<>order_uuid OR prior.request_digest<>p_digest OR prior.request IS DISTINCT FROM p_body THEN
            RAISE EXCEPTION 'Warehouse request already belongs to different evidence' USING ERRCODE='40001';
        END IF;
        -- Replay remains safe after a later cancellation/status transition.
        RETURN prior.response || jsonb_build_object('replayed',true);
    END IF;
    IF EXISTS(SELECT 1 FROM warehouse_fulfillment_receipts WHERE tenant_id=p_tenant
              AND (order_id=order_uuid OR event_id=event_uuid) AND response->>'status'='fulfilled') THEN
        rejection := 'order_already_fulfilled';
    ELSIF upper(source.status) NOT IN('PENDING','PROCESSING')
       OR upper(btrim(COALESCE(source.fulfillment_status,''))) NOT IN('','UNFULFILLED','PENDING')
       OR upper(btrim(COALESCE(source.source_order_payload->>'fulfillment_status',''))) NOT IN('','UNFULFILLED','PENDING')
       OR upper(btrim(COALESCE(source.source_order_payload->>'fulfillmentStatus',''))) NOT IN('','UNFULFILLED','PENDING')
       OR NULLIF(source.source_order_payload->>'cancelled_at','') IS NOT NULL
       OR NULLIF(source.source_order_payload->>'cancelledAt','') IS NOT NULL
    THEN
        rejection := 'source_state_not_eligible';
    ELSIF source.updated_at IS DISTINCT FROM (p_body->>'expected_updated_at')::timestamptz THEN
        rejection := 'source_revision_changed';
    END IF;
    IF rejection IS NULL AND jsonb_typeof(source.line_items) IS DISTINCT FROM 'array' THEN
        rejection := 'source_lines_changed';
    END IF;
    IF rejection IS NULL AND jsonb_array_length(source.line_items) NOT BETWEEN 1 AND 100 THEN
        rejection := 'source_lines_changed';
    END IF;
    IF rejection IS NULL AND EXISTS(
        SELECT 1 FROM jsonb_array_elements(source.line_items) l
        WHERE jsonb_typeof(l)<>'object' OR jsonb_typeof(l->'sku') IS DISTINCT FROM 'string'
           OR btrim(l->>'sku')='' OR length(l->>'sku')>100
           OR jsonb_typeof(l->'quantity') IS DISTINCT FROM 'number'
           OR CASE WHEN jsonb_typeof(l->'quantity')='number' THEN
                (l->>'quantity')::numeric<>trunc((l->>'quantity')::numeric)
                OR (l->>'quantity')::numeric NOT BETWEEN 1 AND 1000000 ELSE true END
           OR (l->'shopifyLineItemId' IS NOT NULL AND l->'shopifyLineItemId'<>'null'::jsonb
               AND (jsonb_typeof(l->'shopifyLineItemId')<>'string' OR btrim(l->>'shopifyLineItemId')='' OR length(l->>'shopifyLineItemId')>255))
    ) THEN
        rejection := 'source_lines_changed';
    END IF;
    IF rejection IS NULL THEN
    WITH canonical AS (
        SELECT 'oms-line-' || encode(sha256(convert_to(
                 CASE WHEN item->>'shopifyLineItemId' IS NOT NULL
                      THEN 'source:' || (item->>'shopifyLineItemId')
                      ELSE 'position:' || (ordinality-1)::text END,'UTF8')),'hex') AS line_id,
               item->>'sku' AS sku,(item->>'quantity')::numeric::bigint AS quantity_each
          FROM jsonb_array_elements(source.line_items) WITH ORDINALITY AS l(item,ordinality)
    )
    SELECT CASE WHEN count(*)=count(DISTINCT line_id)
           THEN jsonb_agg(jsonb_build_object('line_id',line_id,'sku',sku,'quantity_each',quantity_each) ORDER BY line_id)
           ELSE NULL END INTO expected_lines FROM canonical;
    IF expected_lines IS NULL THEN
        rejection := 'source_lines_changed';
    END IF;
    SELECT jsonb_agg(l ORDER BY l->>'line_id') INTO submitted_lines
      FROM jsonb_array_elements(p_body->'lines') l;
    IF submitted_lines IS DISTINCT FROM expected_lines THEN
        rejection := 'source_lines_changed';
    END IF;
    END IF;

    IF rejection IS NULL THEN
    applied_at := GREATEST(clock_timestamp(),source.updated_at+interval '1 microsecond');
    UPDATE orders SET status='FULFILLED',fulfillment_status='FULFILLED',updated_at=applied_at
      WHERE tenant_id=p_tenant AND id=order_uuid;
    answer := jsonb_build_object(
        'contract','stateset.wms-fulfillment.v1','tenant_id',p_tenant,
        'order_id',order_uuid,'request_id',request_uuid,'event_id',event_uuid,'request_digest',p_digest,
        'applied_updated_at',applied_at,'status','fulfilled','replayed',false
    );
    ELSE
        answer := jsonb_build_object(
            'contract','stateset.wms-fulfillment.v1','tenant_id',p_tenant,
            'order_id',order_uuid,'request_id',request_uuid,'event_id',event_uuid,'request_digest',p_digest,
            'status','rejected','reason',rejection,'observed_updated_at',source.updated_at,'replayed',false
        );
    END IF;
    INSERT INTO warehouse_fulfillment_receipts(tenant_id,request_id,event_id,order_id,request_digest,request,response,applied_updated_at,observed_updated_at)
      VALUES(p_tenant,request_uuid,event_uuid,order_uuid,p_digest,p_body,answer,applied_at,source.updated_at);
    RETURN answer;
END $$;
