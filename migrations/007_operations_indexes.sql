-- Tenant-leading covering indexes for bounded operational aggregation. No
-- business rows, retained native evidence or delivery state are changed.
CREATE INDEX wms_outbox_operations
 ON wms_outbox(tenant,destination,status)
 INCLUDE(event_id,due_at,lease_until);
CREATE INDEX wms_native_operations
 ON wms_native_requests(tenant,kind,status)
 INCLUDE(created_at,updated_at,lease_until);
CREATE INDEX wms_open_exceptions_operations
 ON wms_exceptions(tenant,warehouse_id) WHERE status='open';
