-- Native imports represent complete OMS orders, not warehouse-assigned splits.
CREATE UNIQUE INDEX IF NOT EXISTS wms_orders_native_source_owner
 ON wms_orders(tenant,source_order_id) WHERE source_order_id LIKE 'oms:%';

-- Reserve ownership before the local command commits, across warehouses too.
-- A definitively uncommitted local conflict may be reviewed under a new request.
CREATE UNIQUE INDEX IF NOT EXISTS wms_native_import_source_owner
 ON wms_native_requests(tenant,(frozen#>>'{command_request,command,source_order_id}'))
 WHERE kind='oms_import' AND status IN('frozen','completed')
   AND frozen#>>'{command_request,command,source_order_id}' LIKE 'oms:%';
