"""Disposable after-write fault injection for an explicitly owned test database."""

import json
import re


def literal(value):
    return "'" + value.replace("'", "''") + "'"


def barrier_sql(name, marker, request, actor, baseline, lock_keys):
    """Assert exact uncommitted facts before advertising and entering the lock."""
    if not re.fullmatch(r"wms_resilience_serial_[0-9_abcdef]+", name):
        raise ValueError("Unsafe owned barrier identifier")
    first, second = lock_keys
    if not all(type(k) is int and 0 < k < 2**31 for k in lock_keys):
        raise ValueError("Invalid advisory lock keys")
    frozen = literal(json.dumps(request, separators=(",", ":")))
    before = literal(json.dumps(baseline, separators=(",", ":")))
    return f"""
CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $barrier$
DECLARE original JSONB={frozen}::jsonb; baseline JSONB={before}::jsonb;
 e wms_events; s wms_serial_steps; u wms_serial_units; h wms_serial_heads;
 c JSONB=original->'command'; expected_place JSONB;
BEGIN
 IF NEW.tenant<>'resilience-test' OR NEW.destination<>'oms' THEN RETURN NEW; END IF;
 SELECT * INTO e FROM wms_events WHERE id=NEW.event_id;
 IF e.tenant IS DISTINCT FROM 'resilience-test' OR e.warehouse_id IS DISTINCT FROM 'wh-main'
  OR e.payload->>'request_id' IS DISTINCT FROM original->>'request_id' THEN RETURN NEW; END IF;
 expected_place=wms_serial_on_hand('wh-main',c->>'to');
 SELECT * INTO u FROM wms_serial_units WHERE tenant=e.tenant AND sku=c->>'sku'
  AND serial=original->'serial_inputs'->0->'serials'->>0;
 SELECT * INTO s FROM wms_serial_steps WHERE tenant=e.tenant AND event_id=e.id AND ordinal=1;
 SELECT * INTO h FROM wms_serial_heads WHERE tenant=e.tenant AND sku=c->>'sku';
 IF (e.payload->'command'=c AND e.payload->'serial_inputs'=original->'serial_inputs'
  AND e.payload->>'actor'={literal(actor)}
  AND e.version=(original->>'expected_version')::bigint+1
  AND e.version=(SELECT version FROM wms_warehouses WHERE tenant=e.tenant AND warehouse_id=e.warehouse_id)
  AND EXISTS(SELECT 1 FROM wms_commands receipt WHERE receipt.tenant=e.tenant
   AND receipt.warehouse_id=e.warehouse_id AND receipt.request_id=original->>'request_id'
   AND receipt.actor={literal(actor)} AND receipt.result=e.payload->'result')
  AND u.lot=c->>'lot' AND u.revision=(baseline->>'revision')::bigint+1
  AND u.place=expected_place AND u.last_event_id=e.id
  AND s.sku=u.sku AND s.serial=u.serial AND s.lot=u.lot AND s.revision=u.revision
  AND s.previous_revision=(baseline->>'revision')::bigint
  AND s.before_place=wms_serial_on_hand('wh-main',c->>'from') AND s.after_place=expected_place
  AND s.actor={literal(actor)} AND s.target='[]'::jsonb AND s.warehouse_version=e.version
  AND e.payload->'serial_steps'=jsonb_build_array(to_jsonb(s)-'tenant')
  AND h.version=(baseline->>'head')::bigint+1 AND h.last_event_id=e.id AND s.sku_version=h.version
  AND (SELECT count(*) FROM wms_serial_observations WHERE tenant=e.tenant AND event_id=e.id)=1
  AND (SELECT count(*) FROM wms_serial_batches WHERE tenant=e.tenant AND event_id=e.id)=1
  AND wms_serial_unit_bound(u) AND wms_serial_head_bound(h)
  AND wms_serial_event_valid(e.tenant,e.warehouse_id,e.id)
  AND (SELECT on_hand FROM wms_stock WHERE tenant=e.tenant AND warehouse_id=e.warehouse_id
   AND sku=c->>'sku' AND lot=c->>'lot' AND location=c->>'from')=(baseline->>'source_quantity')::numeric-1
  AND (SELECT on_hand FROM wms_stock WHERE tenant=e.tenant AND warehouse_id=e.warehouse_id
   AND sku=c->>'sku' AND lot=c->>'lot' AND location=c->>'to')=(baseline->>'destination_quantity')::numeric+1
  AND NOT EXISTS(SELECT 1 FROM wms_outbox WHERE tenant=e.tenant AND event_id=e.id)
 ) IS DISTINCT FROM true THEN
  RAISE EXCEPTION 'Owned serial crash barrier did not observe the exact after-write facts';
 END IF;
 -- pg_stat_activity exposes this marker only after the assertions succeeded.
 -- The external qualifier also verifies the exact advisory lock and blocker PID.
 PERFORM set_config('application_name',{literal(marker)},true);
 PERFORM pg_advisory_xact_lock({first},{second});
 RETURN NEW;
END $barrier$;
CREATE TRIGGER {name} BEFORE INSERT ON wms_outbox FOR EACH ROW EXECUTE FUNCTION {name}();
"""
