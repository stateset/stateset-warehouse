-- Event proofs count scoped batches and bind exact original event/SKU heads.
-- SKU-leading keys cannot bound those per-event history lookups. Keep this
-- independent access path without rewriting rows or changing a source guard.
CREATE INDEX wms_serial_batches_event ON wms_serial_batches(tenant,event_id,warehouse_id);
