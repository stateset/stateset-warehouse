-- Immutable native intent, separate from warehouse inventory transactions.
-- A committed remote request may outlive its initiating HTTP connection.
CREATE TABLE IF NOT EXISTS wms_native_requests (
 tenant TEXT NOT NULL,
 warehouse_id TEXT NOT NULL,
 request_id TEXT NOT NULL CHECK(length(request_id) BETWEEN 8 AND 100),
 kind TEXT NOT NULL CHECK(kind IN('oms_import','tms_transport')),
 actor TEXT NOT NULL,
 digest TEXT NOT NULL CHECK(length(digest)=64),
 input JSONB NOT NULL,
 expected_version BIGINT NOT NULL CHECK(expected_version>=0),
 order_id TEXT,
 source_snapshot JSONB,
 frozen JSONB NOT NULL,
 status TEXT NOT NULL DEFAULT 'frozen' CHECK(status IN('frozen','sending','uncertain','completed','local_conflict')),
 attempts BIGINT NOT NULL DEFAULT 0 CHECK(attempts>=0),
 lease_token TEXT,
 lease_until TIMESTAMPTZ,
 response JSONB,
 last_error TEXT NOT NULL DEFAULT '',
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(tenant,warehouse_id,request_id),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id),
 CHECK((status='sending' AND lease_token IS NOT NULL AND lease_until IS NOT NULL)
    OR (status<>'sending' AND lease_token IS NULL AND lease_until IS NULL)),
 CHECK(status<>'completed' OR response IS NOT NULL),
 CHECK(kind<>'tms_transport' OR order_id IS NOT NULL)
);
CREATE UNIQUE INDEX IF NOT EXISTS wms_native_one_transport_per_order
 ON wms_native_requests(tenant,warehouse_id,order_id) WHERE kind='tms_transport';
CREATE INDEX IF NOT EXISTS wms_native_requests_recent
 ON wms_native_requests(tenant,warehouse_id,created_at DESC);
CREATE TABLE IF NOT EXISTS wms_native_attempts (
 id TEXT PRIMARY KEY,
 tenant TEXT NOT NULL,
 warehouse_id TEXT NOT NULL,
 request_id TEXT NOT NULL,
 outcome TEXT NOT NULL DEFAULT 'started' CHECK(outcome IN('started','accepted','uncertain')),
 detail TEXT NOT NULL DEFAULT '',
 response JSONB,
 started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 finished_at TIMESTAMPTZ,
 FOREIGN KEY(tenant,warehouse_id,request_id)
   REFERENCES wms_native_requests(tenant,warehouse_id,request_id),
 CHECK((outcome='started' AND finished_at IS NULL) OR (outcome<>'started' AND finished_at IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS wms_native_attempts_request
 ON wms_native_attempts(tenant,warehouse_id,request_id,started_at);

CREATE OR REPLACE FUNCTION wms_native_intent_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN
  RAISE EXCEPTION 'Native intent evidence cannot be deleted';
 END IF;
 IF ROW(NEW.tenant,NEW.warehouse_id,NEW.request_id,NEW.kind,NEW.actor,
        NEW.digest,NEW.input,NEW.expected_version,NEW.source_snapshot,
        NEW.frozen,NEW.created_at)
    IS DISTINCT FROM
    ROW(OLD.tenant,OLD.warehouse_id,OLD.request_id,OLD.kind,OLD.actor,
        OLD.digest,OLD.input,OLD.expected_version,OLD.source_snapshot,
        OLD.frozen,OLD.created_at) THEN
  RAISE EXCEPTION 'Native intent identity, actor, input, and frozen evidence are immutable';
 END IF;
 IF NEW.order_id IS DISTINCT FROM OLD.order_id AND
    (OLD.kind='tms_transport' OR OLD.order_id IS NOT NULL OR NEW.status<>'completed') THEN
  RAISE EXCEPTION 'Native source order identity is immutable';
 END IF;
 IF OLD.status='completed' AND NEW IS DISTINCT FROM OLD THEN
  RAISE EXCEPTION 'Completed native receipts are immutable';
 END IF;
 RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS wms_native_intent_guard ON wms_native_requests;
CREATE TRIGGER wms_native_intent_guard BEFORE UPDATE OR DELETE ON wms_native_requests
 FOR EACH ROW EXECUTE FUNCTION wms_native_intent_immutable();

CREATE OR REPLACE FUNCTION wms_native_attempt_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN
  RAISE EXCEPTION 'Native delivery attempt evidence cannot be deleted';
 END IF;
 IF OLD.outcome<>'started' THEN
  RAISE EXCEPTION 'Finished native delivery attempts are immutable';
 END IF;
 IF ROW(NEW.id,NEW.tenant,NEW.warehouse_id,NEW.request_id,NEW.started_at)
    IS DISTINCT FROM ROW(OLD.id,OLD.tenant,OLD.warehouse_id,OLD.request_id,OLD.started_at) THEN
  RAISE EXCEPTION 'Native delivery attempt identity and start time are immutable';
 END IF;
 IF NEW.outcome NOT IN('accepted','uncertain') THEN
  RAISE EXCEPTION 'Started native attempts can only finish accepted or uncertain';
 END IF;
 RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS wms_native_attempt_guard ON wms_native_attempts;
CREATE TRIGGER wms_native_attempt_guard BEFORE UPDATE OR DELETE ON wms_native_attempts
 FOR EACH ROW EXECUTE FUNCTION wms_native_attempt_immutable();
