-- Background full reconciliation. A run executes the same complete read-only
-- proof as GET .../reconciliation, in one snapshot, on the API process that
-- accepted it, but under its own statement deadline instead of the 10-second
-- request deadline, so warehouses with long serial history still reconcile.
-- Runs are operational records, not warehouse evidence: they change no stock
-- or history, and only the newest finished runs per warehouse are kept.
CREATE TABLE wms_reconciliation_runs (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,id TEXT NOT NULL CHECK(length(id) BETWEEN 1 AND 64),
 requested_by TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN('queued','running','succeeded','failed')),
 requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 started_at TIMESTAMPTZ,finished_at TIMESTAMPTZ,
 deadline_at TIMESTAMPTZ NOT NULL CHECK(deadline_at>requested_at),
 version BIGINT CHECK(version IS NULL OR version>=0),
 result JSONB,error TEXT CHECK(error IS NULL OR length(error) BETWEEN 1 AND 1000),
 PRIMARY KEY(tenant,warehouse_id,id),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id),
 -- A finished run carries exactly its outcome: a proof, or a failure reason.
 CHECK(CASE status
  WHEN 'queued' THEN started_at IS NULL AND finished_at IS NULL AND result IS NULL AND error IS NULL
  WHEN 'running' THEN started_at IS NOT NULL AND finished_at IS NULL AND result IS NULL AND error IS NULL
  WHEN 'succeeded' THEN started_at IS NOT NULL AND finished_at IS NOT NULL AND result IS NOT NULL AND version IS NOT NULL AND error IS NULL
  ELSE finished_at IS NOT NULL AND result IS NULL AND error IS NOT NULL END)
);
CREATE INDEX wms_reconciliation_runs_recent ON wms_reconciliation_runs(tenant,warehouse_id,requested_at DESC);
