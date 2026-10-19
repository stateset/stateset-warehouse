-- Exact lot review spans facilities and retained history. These indexes change
-- neither custody nor provenance; no historic fact is manufactured on upgrade.
CREATE INDEX wms_trace_stock ON wms_stock(tenant,sku,lot,warehouse_id,location);
CREATE INDEX wms_trace_allocations ON wms_allocations(tenant,sku,lot,warehouse_id,order_id,line_id,location);
CREATE INDEX wms_trace_receipts ON wms_receipt_lines(tenant,sku,lot,warehouse_id,receipt_id,id);
CREATE INDEX wms_trace_movements ON wms_movements(tenant,sku,lot,warehouse_id,seq);
CREATE INDEX wms_trace_transfers ON wms_transfer_lines(tenant,sku,lot,source_warehouse_id,destination_warehouse_id,transfer_id,id);
CREATE INDEX wms_trace_packages ON wms_package_lines(tenant,sku,lot,warehouse_id,package_id,line_id,location);
CREATE INDEX wms_trace_lot_changes ON wms_lot_changes(tenant,sku,lot,warehouse_id,version,ordinal);
CREATE INDEX wms_trace_lot_decisions ON wms_lot_decisions(tenant,sku,lot,warehouse_id,version,kind);
CREATE INDEX wms_trace_shipment_documents ON wms_shipment_evidence USING GIN(document jsonb_path_ops);
CREATE INDEX wms_trace_return_events ON wms_events(tenant,warehouse_id,(payload#>>'{result,entity_id}'))
 WHERE payload#>>'{command,type}'='create_return';

-- SQL-function inlining lets a section/keyset filter and index-only counts avoid
-- constructing every retained JSON document. Source checks apply to each fetched
-- page, not an invented global certification of records the caller has not read.
CREATE FUNCTION wms_lot_trace_shipment_source(t TEXT,w TEXT,o TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT COALESCE((SELECT wms_shipment_consistent(t,w,o) AND CASE
 WHEN h.source_kind IN('legacy_snapshot','synthetic_fixture') THEN h.event_id IS NULL
 WHEN h.source_kind='event' THEN EXISTS(
  SELECT 1 FROM wms_events e JOIN wms_commands c ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
  WHERE e.id=h.event_id AND e.tenant=t AND e.warehouse_id=w AND e.version=h.source_version
  AND e.payload->>'id'=e.id AND e.payload->>'tenant'=t AND e.payload->>'warehouse_id'=w
  AND e.payload->'version'=to_jsonb(h.source_version) AND e.payload->>'type'='fulfillment.shipped'
  AND e.payload#>>'{command,type}'='ship' AND e.payload#>>'{command,order_id}'=o
  AND e.payload#>>'{command,carrier}'=h.document->>'carrier' AND e.payload#>>'{command,handoff_reference}'=h.document->>'handoff_reference'
  AND wms_canonical_order(e.payload->'order')=h.document
  AND e.payload->>'actor'=c.actor AND e.payload->'result'=c.result AND c.result->'version'=to_jsonb(h.source_version)
 ) ELSE false END FROM wms_shipment_evidence h WHERE h.tenant=t AND h.warehouse_id=w AND h.order_id=o),false);
$$;

CREATE FUNCTION wms_lot_trace_return_source(t TEXT,w TEXT,o TEXT,original_line TEXT,s TEXT,l TEXT) RETURNS BOOLEAN LANGUAGE SQL STABLE AS $$
 SELECT CASE WHEN EXISTS(SELECT 1 FROM wms_split_orders b WHERE b.tenant=t AND b.warehouse_id=w AND b.order_id=o)
 THEN wms_split_consistent(t,w,o) AND wms_lot_order_valid(t,w,o) ELSE wms_lot_trace_shipment_source(t,w,o) END
 AND NOT EXISTS(
  SELECT 1 FROM wms_return_lines r
  LEFT JOIN wms_receipt_lines x ON x.tenant=r.tenant AND x.warehouse_id=r.warehouse_id AND x.receipt_id=r.receipt_id AND x.id=r.line_id
  LEFT JOIN wms_receipts h ON h.tenant=r.tenant AND h.warehouse_id=r.warehouse_id AND h.id=r.receipt_id
  LEFT JOIN wms_receipt_cancellations c ON c.tenant=r.tenant AND c.warehouse_id=r.warehouse_id AND c.receipt_id=r.receipt_id
  LEFT JOIN wms_receipt_cancelled_lines z ON z.tenant=r.tenant AND z.warehouse_id=r.warehouse_id AND z.receipt_id=r.receipt_id AND z.line_id=r.line_id
  WHERE r.tenant=t AND r.warehouse_id=w AND r.order_id=o AND r.order_line_id=original_line AND r.sku=s AND r.lot=l
  AND ((r.sku,r.lot,r.quantity) IS DISTINCT FROM (x.sku,x.lot,x.quantity)
   OR x.received IS DISTINCT FROM COALESCE((SELECT sum(m.quantity) FROM wms_movements m WHERE m.tenant=t AND m.warehouse_id=w AND m.receipt_id=r.receipt_id AND m.receipt_line_id=r.line_id AND m.kind='receipt'),0)
   OR CASE WHEN h.status='cancelled' THEN c.receipt_id IS NULL OR (z.sku,z.lot,z.quantity,z.received,z.cancelled) IS DISTINCT FROM (x.sku,x.lot,x.quantity,x.received,x.quantity-x.received)
      ELSE c.receipt_id IS NOT NULL OR z.line_id IS NOT NULL END)
 )
 AND NOT EXISTS(
  SELECT 1 FROM wms_return_lines r JOIN wms_receipts h ON h.tenant=r.tenant AND h.warehouse_id=r.warehouse_id AND h.id=r.receipt_id
  WHERE r.tenant=t AND r.warehouse_id=w AND r.order_id=o AND r.order_line_id=original_line AND r.sku=s AND r.lot=l
  AND NOT EXISTS(SELECT 1 FROM wms_events e JOIN wms_commands c ON c.tenant=e.tenant AND c.warehouse_id=e.warehouse_id AND c.request_id=e.payload->>'request_id'
   CROSS JOIN LATERAL jsonb_array_elements(e.payload#>'{command,lines}') a
   WHERE e.tenant=t AND e.warehouse_id=w AND e.payload#>>'{result,entity_id}'=r.receipt_id
   AND e.payload#>>'{command,type}'='create_return' AND e.payload->>'type'='return.authorized'
   AND e.payload->>'id'=e.id AND e.payload->>'tenant'=t AND e.payload->>'warehouse_id'=w AND e.payload->'version'=to_jsonb(e.version)
   AND e.payload->>'actor'=c.actor AND e.payload->'result'=c.result AND c.result->'version'=to_jsonb(e.version)
   AND e.payload#>>'{command,order_id}'=o AND e.payload#>>'{command,reference}'=h.reference AND e.payload#>>'{command,reason}'=r.reason
   AND a->>'id'=r.line_id AND a->>'order_line_id'=original_line AND a->>'lot'=l AND a->'quantity'=to_jsonb(r.quantity))
 )
 AND COALESCE((SELECT sum(r.quantity-COALESCE(z.cancelled,0)) FROM wms_return_lines r
 LEFT JOIN wms_receipt_cancelled_lines z ON z.tenant=r.tenant AND z.warehouse_id=r.warehouse_id AND z.receipt_id=r.receipt_id AND z.line_id=r.line_id
 WHERE r.tenant=t AND r.warehouse_id=w AND r.order_id=o AND r.order_line_id=original_line AND r.sku=s AND r.lot=l),0)
 <=COALESCE((SELECT sum(q.quantity) FROM wms_actual_shipped(t,w,o) q WHERE q.line_id=original_line AND q.sku=s AND q.lot=l),0);
$$;

CREATE FUNCTION wms_lot_trace_records(t TEXT,s TEXT,l TEXT,warehouses TEXT[])
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

CREATE FUNCTION wms_lot_trace_balances(t TEXT,s TEXT,l TEXT)
RETURNS TABLE(warehouse_id TEXT,location TEXT,kind TEXT,physical BIGINT,reserved BIGINT,ledger BIGINT,allocation_reserved BIGINT)
LANGUAGE SQL STABLE AS $$
-- Reconstruct physical/reserved custody from original movements and exact
-- outstanding allocation ownership, rather than cached ledger projections.
WITH movements AS (
 SELECT warehouse_id,from_location AS location,-quantity AS delta FROM wms_movements WHERE tenant=$1 AND sku=$2 AND lot=$3 AND from_location IS NOT NULL
 UNION ALL SELECT warehouse_id,to_location,quantity FROM wms_movements WHERE tenant=$1 AND sku=$2 AND lot=$3 AND to_location IS NOT NULL
), ledger AS(SELECT warehouse_id,location,sum(delta)::bigint AS quantity FROM movements GROUP BY warehouse_id,location),
ownership AS (
 SELECT warehouse_id,location,quantity-picked-released AS quantity FROM wms_allocations WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION ALL SELECT warehouse_id,'STAGE-01',picked-shipped FROM wms_allocations WHERE tenant=$1 AND sku=$2 AND lot=$3
), reserved AS(SELECT warehouse_id,location,sum(quantity)::bigint AS quantity FROM ownership GROUP BY warehouse_id,location),
keys AS(
 SELECT warehouse_id,location FROM wms_stock WHERE tenant=$1 AND sku=$2 AND lot=$3
 UNION SELECT warehouse_id,location FROM ledger UNION SELECT warehouse_id,location FROM reserved
)
SELECT k.warehouse_id,k.location,l.kind,COALESCE(s.on_hand,0)::bigint AS physical,COALESCE(s.reserved,0)::bigint AS reserved,
 COALESCE(e.quantity,0)::bigint AS ledger,COALESCE(r.quantity,0)::bigint AS allocation_reserved
FROM keys k LEFT JOIN wms_stock s ON s.tenant=$1 AND s.warehouse_id=k.warehouse_id AND s.sku=$2 AND s.lot=$3 AND s.location=k.location
LEFT JOIN wms_locations l ON l.tenant=$1 AND l.warehouse_id=k.warehouse_id AND l.code=k.location
LEFT JOIN ledger e ON e.warehouse_id=k.warehouse_id AND e.location=k.location
LEFT JOIN reserved r ON r.warehouse_id=k.warehouse_id AND r.location=k.location
ORDER BY k.warehouse_id COLLATE "C",k.location COLLATE "C"

$$;
