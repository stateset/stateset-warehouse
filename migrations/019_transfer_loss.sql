-- A confirmed loss settles only original in-transit ownership. It never adds
-- stock or an arrival. Reversals retain the exact original loss and reopen only
-- its un-reversed quantity; actual recovered goods still require an arrival.
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM wms_transfers h WHERE NOT wms_transfer_consistent(h.tenant,h.id)) THEN
  RAISE EXCEPTION 'Repair original transfer evidence before enabling loss settlement' USING ERRCODE='23514';
 END IF;
END $$;

CREATE TABLE wms_transfer_loss_records (
 tenant TEXT NOT NULL, id TEXT NOT NULL, transfer_id TEXT NOT NULL, line_id TEXT NOT NULL,
 warehouse_id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN('loss','reversal')),
 quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000),
 original_loss_id TEXT, incident_reference TEXT,
 reason TEXT NOT NULL CHECK(octet_length(reason) BETWEEN 1 AND 240 AND btrim(reason)=reason AND reason !~ '[[:cntrl:]]'),
 actor TEXT NOT NULL CHECK(octet_length(actor) BETWEEN 1 AND 240 AND btrim(actor)=actor AND actor !~ '[[:cntrl:]]'),
 recorded_at TEXT NOT NULL, version BIGINT NOT NULL CHECK(version>0),
 event_id TEXT NOT NULL UNIQUE REFERENCES wms_events(id) DEFERRABLE INITIALLY DEFERRED,
 PRIMARY KEY(tenant,id),
 UNIQUE(tenant,warehouse_id,version),
 FOREIGN KEY(tenant,transfer_id,line_id) REFERENCES wms_transfer_lines(tenant,transfer_id,id),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id),
 FOREIGN KEY(tenant,original_loss_id) REFERENCES wms_transfer_loss_records(tenant,id),
 CHECK((kind='loss' AND original_loss_id IS NULL AND incident_reference IS NOT NULL
        AND octet_length(incident_reference) BETWEEN 1 AND 240 AND btrim(incident_reference)=incident_reference AND incident_reference !~ '[[:cntrl:]]')
    OR (kind='reversal' AND original_loss_id IS NOT NULL AND incident_reference IS NULL))
);
CREATE INDEX wms_transfer_loss_lines ON wms_transfer_loss_records(tenant,transfer_id,line_id,kind);
CREATE INDEX wms_transfer_loss_page ON wms_transfer_loss_records(tenant,transfer_id,version);
CREATE INDEX wms_transfer_loss_original ON wms_transfer_loss_records(tenant,original_loss_id) WHERE original_loss_id IS NOT NULL;
CREATE TRIGGER wms_transfer_loss_immutable BEFORE UPDATE OR DELETE ON wms_transfer_loss_records FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_transfer_loss_no_truncate BEFORE TRUNCATE ON wms_transfer_loss_records FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();

CREATE FUNCTION wms_transfer_lost(t TEXT,transfer TEXT,line TEXT) RETURNS BIGINT LANGUAGE SQL STABLE AS $$
 SELECT COALESCE(sum(CASE WHEN kind='loss' THEN quantity ELSE -quantity END),0)::bigint
 FROM wms_transfer_loss_records WHERE tenant=t AND transfer_id=transfer AND line_id=line;
$$;
CREATE FUNCTION wms_transfer_loss_original(r wms_transfer_loss_records) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c
 ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
 WHERE e.id=r.event_id AND e.tenant=r.tenant AND e.warehouse_id=r.warehouse_id AND e.version=r.version
 AND r.version<=(SELECT version FROM wms_warehouses WHERE tenant=r.tenant AND warehouse_id=r.warehouse_id)
 AND e.payload->>'id'=e.id AND e.payload->>'tenant'=r.tenant AND e.payload->>'warehouse_id'=r.warehouse_id
 AND e.payload->'version'=to_jsonb(r.version) AND e.payload->>'actor'=r.actor AND c.actor=r.actor
 AND e.payload->>'occurred_at'=r.recorded_at AND e.payload->'result'=c.result
 AND c.result=jsonb_build_object('entity_id',r.id,'version',r.version,'type',CASE r.kind WHEN 'loss' THEN 'transfer.loss_recorded' ELSE 'transfer.loss_reversed' END)
 AND e.payload->>'type'=c.result->>'type'
 AND e.payload->'transfer_loss_record'=to_jsonb(r)-'tenant'
 AND e.payload->'command'=CASE r.kind WHEN 'loss' THEN
  jsonb_build_object('type','record_transfer_loss','transfer_id',r.transfer_id,'line_id',r.line_id,'quantity',r.quantity,'incident_reference',r.incident_reference,'reason',r.reason)
 ELSE jsonb_build_object('type','reverse_transfer_loss','loss_id',r.original_loss_id,'quantity',r.quantity,'reason',r.reason) END);
$$;

CREATE OR REPLACE FUNCTION wms_transfer_consistent(t TEXT,transfer TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
BEGIN
 IF NOT wms_transfer_original_consistent(t,transfer) THEN RETURN false; END IF;
 IF EXISTS(SELECT 1 FROM wms_transfer_loss_records r JOIN wms_transfers h ON h.tenant=r.tenant AND h.id=r.transfer_id
  LEFT JOIN wms_transfer_loss_records original ON original.tenant=r.tenant AND original.id=r.original_loss_id
  WHERE r.tenant=t AND r.transfer_id=transfer AND (
   r.warehouse_id<>h.destination_warehouse_id OR NOT wms_transfer_loss_original(r)
   OR (r.kind='reversal' AND (original.kind IS DISTINCT FROM 'loss'
    OR (r.transfer_id,r.line_id,r.warehouse_id) IS DISTINCT FROM (original.transfer_id,original.line_id,original.warehouse_id)
    OR r.version<=original.version))
   OR (r.kind='loss' AND (SELECT COALESCE(sum(x.quantity),0) FROM wms_transfer_loss_records x WHERE x.tenant=t AND x.original_loss_id=r.id)>r.quantity)
  )) THEN RETURN false; END IF;
 IF EXISTS(SELECT 1 FROM wms_transfer_lines l WHERE l.tenant=t AND l.transfer_id=transfer AND (
  wms_transfer_lost(t,transfer,l.id)<0 OR wms_transfer_lost(t,transfer,l.id)+
  (SELECT COALESCE(sum(m.quantity),0) FROM wms_movements m WHERE m.tenant=t AND m.transfer_id=transfer AND m.transfer_line_id=l.id AND m.kind='transfer_receive')>l.quantity
 )) THEN RETURN false; END IF;
 -- Retained events must not outlive a missing normalized original record.
 IF EXISTS(SELECT 1 FROM wms_events e WHERE e.tenant=t AND e.payload#>>'{transfer_loss_record,transfer_id}'=transfer
 AND (e.payload->>'type' IN('transfer.loss_recorded','transfer.loss_reversed')
   OR e.payload#>>'{command,type}' IN('record_transfer_loss','reverse_transfer_loss'))
 AND NOT EXISTS(SELECT 1 FROM wms_transfer_loss_records r WHERE r.tenant=t AND r.transfer_id=transfer AND r.event_id=e.id)) THEN RETURN false; END IF;
 RETURN true;
END $$;

CREATE OR REPLACE FUNCTION wms_transfer_document(t TEXT,transfer TEXT) RETURNS JSONB LANGUAGE SQL STABLE AS $$
 SELECT to_jsonb(h)-'tenant'||jsonb_build_object('lines',(
  SELECT jsonb_agg(to_jsonb(l)-'tenant'-'transfer_id'-'source_warehouse_id'-'destination_warehouse_id'||jsonb_build_object(
   'received',(SELECT COALESCE(sum(m.quantity),0)::bigint FROM wms_movements m WHERE m.tenant=t AND m.transfer_id=transfer AND m.transfer_line_id=l.id AND m.kind='transfer_receive'),
   'lost',wms_transfer_lost(t,transfer,l.id),'loss_record_count',(SELECT count(*) FROM wms_transfer_loss_records r WHERE r.tenant=t AND r.transfer_id=transfer AND r.line_id=l.id)) ORDER BY l.id)
  FROM wms_transfer_lines l WHERE l.tenant=t AND l.transfer_id=transfer))
 FROM wms_transfers h WHERE h.tenant=t AND h.id=transfer;
$$;
CREATE OR REPLACE FUNCTION wms_transfer_open(t TEXT,transfer TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM wms_transfer_lines l WHERE l.tenant=t AND l.transfer_id=transfer AND l.quantity>wms_transfer_lost(t,transfer,l.id)+
  (SELECT COALESCE(sum(m.quantity),0) FROM wms_movements m WHERE m.tenant=t AND m.transfer_id=transfer AND m.transfer_line_id=l.id AND m.kind='transfer_receive'));
$$;

CREATE FUNCTION wms_transfer_loss_lock() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN PERFORM wms_serialize_shipment(NEW.tenant,NEW.warehouse_id); RETURN NEW; END $$;
CREATE TRIGGER wms_transfer_loss_lock BEFORE INSERT ON wms_transfer_loss_records FOR EACH ROW EXECUTE FUNCTION wms_transfer_loss_lock();
CREATE CONSTRAINT TRIGGER wms_transfer_loss_proof AFTER INSERT ON wms_transfer_loss_records DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION wms_transfer_commit_proof();

-- A trace transfer section retains the same original line identity. Its new
-- loss fields come from the immutable journal, never dispatched-minus-arrived.

CREATE FUNCTION wms_transfer_original_consistent(t TEXT,transfer TEXT) RETURNS BOOLEAN LANGUAGE plpgsql STABLE AS $$
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

CREATE OR REPLACE FUNCTION wms_lot_trace_records(t TEXT,s TEXT,l TEXT,warehouses TEXT[])
RETURNS TABLE(section TEXT,warehouse_id TEXT,sort_key JSONB,document JSONB,source_ok BOOLEAN)
LANGUAGE SQL STABLE AS $$
 SELECT 'allocations',a.warehouse_id,jsonb_build_array(a.order_id,a.line_id,a.location),
 jsonb_build_object('order_id',a.order_id,'order_reference',o.reference,'order_status',o.status,'line_id',a.line_id,
 'location',a.location,'quantity',a.quantity,'picked',a.picked,'shipped',a.shipped,'released',a.released,
 'storage_reserved',a.quantity-a.picked-a.released,'staged',a.picked-a.shipped,
 'original_line_quantity',ol.quantity,'source_kind','normalized_allocation'),
 wms_lot_order_valid(a.tenant,a.warehouse_id,a.order_id)
 AND (SELECT sum(x.quantity-x.released) FROM wms_allocations x WHERE x.tenant=a.tenant AND x.warehouse_id=a.warehouse_id AND x.order_id=a.order_id AND x.line_id=a.line_id)<=ol.quantity
 AND CASE WHEN o.data->>'shipment_mode'='packages' THEN wms_split_consistent(a.tenant,a.warehouse_id,a.order_id)
 WHEN o.status='shipped' THEN wms_shipment_consistent(a.tenant,a.warehouse_id,a.order_id) ELSE true END
 FROM wms_allocations a JOIN wms_orders o ON o.tenant=a.tenant AND o.warehouse_id=a.warehouse_id AND o.id=a.order_id
 JOIN wms_order_lines ol ON ol.tenant=a.tenant AND ol.warehouse_id=a.warehouse_id AND ol.order_id=a.order_id AND ol.id=a.line_id AND ol.sku=a.sku
 WHERE a.tenant=t AND a.sku=s AND a.lot=l AND a.warehouse_id=ANY(warehouses)
 UNION ALL
 SELECT 'receipts',r.warehouse_id,jsonb_build_array(r.receipt_id,r.id),
 jsonb_build_object('receipt_id',r.receipt_id,'reference',h.reference,'status',h.status,'line_id',r.id,
 'expected',r.quantity,'received',r.received,'cancelled',COALESCE(c.cancelled,0),
 'return_order_id',o.order_id,'return_order_line_id',o.order_line_id,'source_kind',CASE WHEN o.receipt_id IS NULL THEN 'expected_receipt' ELSE 'authorized_customer_return' END),
 r.received=COALESCE((SELECT sum(m.quantity) FROM wms_movements m WHERE m.tenant=r.tenant AND m.warehouse_id=r.warehouse_id AND m.receipt_id=r.receipt_id AND m.receipt_line_id=r.id AND m.kind='receipt'),0)
 AND CASE WHEN h.status='cancelled' THEN c.line_id IS NOT NULL AND (c.sku,c.lot,c.quantity,c.received,c.cancelled) IS NOT DISTINCT FROM (r.sku,r.lot,r.quantity,r.received,r.quantity-r.received) ELSE c.line_id IS NULL END
 AND (o.receipt_id IS NULL OR ((o.sku,o.lot,o.quantity) IS NOT DISTINCT FROM (r.sku,r.lot,r.quantity) AND wms_lot_trace_return_source(o.tenant,o.warehouse_id,o.order_id,o.order_line_id,o.sku,o.lot)))
 FROM wms_receipt_lines r JOIN wms_receipts h ON h.tenant=r.tenant AND h.warehouse_id=r.warehouse_id AND h.id=r.receipt_id
 LEFT JOIN wms_receipt_cancelled_lines c ON c.tenant=r.tenant AND c.warehouse_id=r.warehouse_id AND c.receipt_id=r.receipt_id AND c.line_id=r.id
 LEFT JOIN wms_return_lines o ON o.tenant=r.tenant AND o.warehouse_id=r.warehouse_id AND o.receipt_id=r.receipt_id AND o.line_id=r.id
 WHERE r.tenant=t AND r.sku=s AND r.lot=l AND r.warehouse_id=ANY(warehouses)
 UNION ALL
 SELECT 'shipments',p.warehouse_id,jsonb_build_array('package',p.package_id,p.line_id,p.location),
 jsonb_build_object('package_id',p.package_id,'package_reference',h.reference,'order_id',p.order_id,'line_id',p.line_id,
 'source_location',p.location,'quantity',p.quantity,'state',COALESCE(c.kind,'packed'),
 'source_kind','package_event','packed_event_id',h.source_event_id,'packed_version',h.source_version,'packed_at',h.packed_at,
 'closure_event_id',c.event_id,'closure_version',c.version,'closed_at',c.closed_at,'carrier',c.carrier,'handoff_reference',c.handoff_reference),
 wms_split_consistent(p.tenant,p.warehouse_id,p.order_id) AND wms_lot_order_valid(p.tenant,p.warehouse_id,p.order_id)
 FROM wms_package_lines p JOIN wms_packages h ON h.tenant=p.tenant AND h.warehouse_id=p.warehouse_id AND h.id=p.package_id
 LEFT JOIN wms_package_closures c ON c.tenant=p.tenant AND c.warehouse_id=p.warehouse_id AND c.package_id=p.package_id
 WHERE p.tenant=t AND p.sku=s AND p.lot=l AND p.warehouse_id=ANY(warehouses)
 UNION ALL
 SELECT 'shipments',h.warehouse_id,jsonb_build_array('original',h.order_id,a->>'line_id',a->>'location'),
 jsonb_build_object('order_id',h.order_id,'line_id',a->>'line_id','source_location',a->>'location',
 'quantity',(a->>'shipped')::bigint,'state','handed_off','source_kind',h.source_kind,
 'event_id',h.event_id,'source_version',h.source_version,'carrier',h.document->'carrier','handoff_reference',h.document->'handoff_reference','shipped_at',h.document->'shipped_at'),
 wms_lot_trace_shipment_source(h.tenant,h.warehouse_id,h.order_id) AND wms_lot_order_valid(h.tenant,h.warehouse_id,h.order_id)
 FROM wms_shipment_evidence h CROSS JOIN LATERAL jsonb_array_elements(h.document->'allocations') a
 WHERE h.tenant=t AND h.warehouse_id=ANY(warehouses) AND a->>'sku'=s AND a->>'lot'=l
 AND h.document @> jsonb_build_object('allocations',jsonb_build_array(jsonb_build_object('sku',s,'lot',l)))
 UNION ALL
 SELECT 'transfers',x.source_warehouse_id,jsonb_build_array(x.transfer_id,x.id),
 jsonb_build_object('transfer_id',x.transfer_id,'reference',h.reference,'line_id',x.id,'source_warehouse_id',x.source_warehouse_id,
 'destination_warehouse_id',x.destination_warehouse_id,'source_location',x.location,'dispatched',x.quantity,
 'received',COALESCE((SELECT sum(m.quantity) FROM wms_movements m WHERE m.tenant=x.tenant AND m.transfer_id=x.transfer_id AND m.transfer_line_id=x.id AND m.kind='transfer_receive'),0),
 'lost',wms_transfer_lost(x.tenant,x.transfer_id,x.id),'loss_record_count',(SELECT count(*) FROM wms_transfer_loss_records r WHERE r.tenant=x.tenant AND r.transfer_id=x.transfer_id AND r.line_id=x.id),
 'movement_id',x.movement_id,'source_event_id',h.source_event_id,'source_version',h.source_version,
 'carrier',h.carrier,'handoff_reference',h.handoff_reference,'dispatched_at',h.dispatched_at,'source_kind','transfer_event'),
 wms_transfer_consistent(x.tenant,x.transfer_id) AND wms_lot_dispatch_valid(x.tenant,x.source_warehouse_id,h.source_version,x.sku,x.lot)
 FROM wms_transfer_lines x JOIN wms_transfers h ON h.tenant=x.tenant AND h.id=x.transfer_id
 WHERE x.tenant=t AND x.sku=s AND x.lot=l AND x.source_warehouse_id=ANY(warehouses)
 UNION ALL
 SELECT 'movements',m.warehouse_id,jsonb_build_array(lpad(m.seq::text,20,'0')),
 jsonb_build_object('id',m.id,'sequence',m.seq,'kind',m.kind,'quantity',m.quantity,'from_location',m.from_location,'to_location',m.to_location,
 'reference',m.reference,'receipt_id',m.receipt_id,'receipt_line_id',m.receipt_line_id,'transfer_id',m.transfer_id,'transfer_line_id',m.transfer_line_id,
 'transfer_event_id',m.transfer_event_id,'package_id',m.package_id,'order_line_id',m.order_line_id,'allocation_location',m.allocation_location,
 'shipment_event_id',m.shipment_event_id,'at',m.data->'at','actor',m.data->'actor','reason',m.data->'reason','source_kind','append_only_movement'),
 (m.data->>'id',m.data->>'sku',m.data->>'lot',m.data->>'from',m.data->>'to',m.data->>'kind',m.data->>'reference')
 IS NOT DISTINCT FROM (m.id,m.sku,m.lot,m.from_location,m.to_location,m.kind,m.reference)
 AND m.data->'quantity'=to_jsonb(m.quantity)
 AND (m.data->>'receipt_id',m.data->>'receipt_line_id',m.data->>'transfer_id',m.data->>'transfer_line_id',m.data->>'transfer_event_id',m.data->>'package_id',m.data->>'order_line_id',m.data->>'allocation_location',m.data->>'shipment_event_id')
 IS NOT DISTINCT FROM (m.receipt_id,m.receipt_line_id,m.transfer_id,m.transfer_line_id,m.transfer_event_id,m.package_id,m.order_line_id,m.allocation_location,m.shipment_event_id)
 FROM wms_movements m WHERE m.tenant=t AND m.sku=s AND m.lot=l AND m.warehouse_id=ANY(warehouses)
 UNION ALL
 SELECT 'policies',h.warehouse_id,jsonb_build_array(lpad(h.version::text,20,'0'),lpad(h.ordinal::text,10,'0')),
 jsonb_build_object('version',h.version,'ordinal',h.ordinal,'event_id',h.source_event_id,'source_kind','policy_event','change',h.document),
 wms_lot_change_valid(h.tenant,h.warehouse_id,h.version,h.ordinal) AND wms_lot_event_valid(h.tenant,h.warehouse_id,h.version)
 FROM wms_lot_changes h WHERE h.tenant=t AND h.sku=s AND (h.lot=l OR h.lot IS NULL) AND h.warehouse_id=ANY(warehouses)
 UNION ALL
 SELECT 'decisions',d.warehouse_id,jsonb_build_array(lpad(d.version::text,20,'0'),d.kind),
 jsonb_build_object('version',d.version,'event_id',e.id,'kind',d.kind,'source_kind','eligibility_decision',
 'decision',wms_lot_decision_document(d.kind,d.sku,d.lot,d.checked_at,d.required,d.document)),
 wms_lot_decision_valid(d.tenant,d.warehouse_id,d.version,d.kind,d.sku,d.lot) AND wms_lot_event_valid(d.tenant,d.warehouse_id,d.version)
 FROM wms_lot_decisions d LEFT JOIN wms_events e ON e.tenant=d.tenant AND e.warehouse_id=d.warehouse_id AND e.version=d.version
 WHERE d.tenant=t AND d.sku=s AND d.lot=l AND d.warehouse_id=ANY(warehouses);
$$;

