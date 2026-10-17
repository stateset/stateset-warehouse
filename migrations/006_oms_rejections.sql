-- A tenant-bound, durable rejection proves that an OMS intent did not apply.
-- It permits a separately reviewed new intent for the same physical event;
-- an uncertain outcome never releases that ownership.
ALTER TABLE wms_native_requests DROP CONSTRAINT wms_native_requests_status_check;
ALTER TABLE wms_native_requests ADD CONSTRAINT wms_native_requests_status_check
 CHECK(status IN('frozen','sending','uncertain','completed','local_conflict','rejected'));
ALTER TABLE wms_native_requests ADD CONSTRAINT wms_native_terminal_response
 CHECK(status NOT IN('completed','rejected') OR response IS NOT NULL);
ALTER TABLE wms_native_attempts DROP CONSTRAINT wms_native_attempts_outcome_check;
ALTER TABLE wms_native_attempts ADD CONSTRAINT wms_native_attempts_outcome_check
 CHECK(outcome IN('started','accepted','uncertain','rejected'));

DROP INDEX wms_native_one_fulfillment_per_order;
CREATE UNIQUE INDEX wms_native_one_fulfillment_per_order
 ON wms_native_requests(tenant,warehouse_id,order_id)
 WHERE kind='oms_fulfillment' AND status<>'rejected';
DROP INDEX wms_native_one_fulfillment_per_event;
CREATE UNIQUE INDEX wms_native_one_fulfillment_per_event
 ON wms_native_requests(tenant,warehouse_id,(frozen->>'event_id'))
 WHERE kind='oms_fulfillment' AND status<>'rejected';

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
 IF OLD.status IN('completed','rejected') AND NEW IS DISTINCT FROM OLD THEN
  RAISE EXCEPTION 'Terminal native receipts are immutable';
 END IF;
 RETURN NEW;
END $$;

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
 IF NEW.outcome NOT IN('accepted','uncertain','rejected') THEN
  RAISE EXCEPTION 'Started native attempts can only finish accepted, uncertain or rejected';
 END IF;
 RETURN NEW;
END $$;
