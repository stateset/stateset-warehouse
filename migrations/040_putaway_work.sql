-- Work ownership is separate from physical custody. Only an original canonical
-- move receipt can advance putaway progress, in that receipt's transaction.
CREATE TABLE wms_work_tasks (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, id TEXT NOT NULL,
 seq BIGSERIAL UNIQUE, sku TEXT NOT NULL, lot TEXT NOT NULL, from_location TEXT NOT NULL,
 quantity BIGINT NOT NULL CHECK(quantity BETWEEN 1 AND 1000000000),
 completed_quantity BIGINT NOT NULL DEFAULT 0 CHECK(completed_quantity BETWEEN 0 AND quantity),
 status TEXT NOT NULL DEFAULT 'ready' CHECK(status IN('ready','claimed','paused','blocked','completed','cancelled')),
 owner TEXT, priority INTEGER NOT NULL DEFAULT 0 CHECK(priority BETWEEN 0 AND 2), due_at TIMESTAMPTZ,
 revision BIGINT NOT NULL DEFAULT 1, reason TEXT, source_receipt_id TEXT, source_event_id TEXT,
 created_by TEXT NOT NULL, updated_by TEXT NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(), updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 last_action TEXT NOT NULL DEFAULT 'created',
 PRIMARY KEY(tenant,warehouse_id,id),
 FOREIGN KEY(tenant,warehouse_id) REFERENCES wms_warehouses(tenant,warehouse_id),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku),
 FOREIGN KEY(tenant,warehouse_id,from_location) REFERENCES wms_locations(tenant,warehouse_id,code),
 FOREIGN KEY(tenant,warehouse_id,source_receipt_id) REFERENCES wms_receipts(tenant,warehouse_id,id),
 FOREIGN KEY(source_event_id) REFERENCES wms_events(id),
 CHECK((status='completed')=(completed_quantity=quantity)),
 CHECK(status NOT IN('claimed','paused','blocked','completed') OR owner IS NOT NULL),
 CHECK(status<>'ready' OR owner IS NULL),
 CHECK(status NOT IN('blocked','cancelled') OR COALESCE(length(btrim(reason)) BETWEEN 1 AND 500,false))
);
CREATE INDEX wms_work_tasks_queue ON wms_work_tasks(tenant,warehouse_id,status,priority DESC,due_at,seq);
CREATE INDEX wms_work_tasks_owner ON wms_work_tasks(tenant,warehouse_id,owner,status);
CREATE INDEX wms_work_tasks_source ON wms_work_tasks(tenant,warehouse_id,sku,lot,from_location)
 WHERE status NOT IN('completed','cancelled');

CREATE TABLE wms_work_executions (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, request_id TEXT NOT NULL,
 task_id TEXT NOT NULL, task_revision BIGINT NOT NULL, actor TEXT NOT NULL,
 command JSONB NOT NULL, expected_version BIGINT NOT NULL, resource_versions JSONB NOT NULL,
 created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(tenant,warehouse_id,request_id), UNIQUE(tenant,warehouse_id,task_id,task_revision),
 FOREIGN KEY(tenant,warehouse_id,task_id) REFERENCES wms_work_tasks(tenant,warehouse_id,id),
 CHECK(COALESCE(command->>'type'='move_stock' AND jsonb_typeof(resource_versions)='object',false))
);
CREATE TABLE wms_work_task_events (
 tenant TEXT NOT NULL, warehouse_id TEXT NOT NULL, task_id TEXT NOT NULL, revision BIGINT NOT NULL,
 actor TEXT NOT NULL, action TEXT NOT NULL, snapshot JSONB NOT NULL,
 occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(tenant,warehouse_id,task_id,revision),
 FOREIGN KEY(tenant,warehouse_id,task_id) REFERENCES wms_work_tasks(tenant,warehouse_id,id)
);
CREATE TRIGGER wms_work_executions_immutable BEFORE UPDATE OR DELETE ON wms_work_executions FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_work_executions_no_truncate BEFORE TRUNCATE ON wms_work_executions FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_work_events_immutable BEFORE UPDATE OR DELETE ON wms_work_task_events FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_work_events_no_truncate BEFORE TRUNCATE ON wms_work_task_events FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_work_tasks_no_delete BEFORE DELETE ON wms_work_tasks FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_work_tasks_no_truncate BEFORE TRUNCATE ON wms_work_tasks FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();

CREATE FUNCTION wms_work_task_guard() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE confirmed BIGINT;
BEGIN
 IF TG_OP='INSERT' THEN
  IF NEW.status<>'ready' OR NEW.completed_quantity<>0 OR NEW.owner IS NOT NULL THEN
   RAISE EXCEPTION 'New work must be unclaimed and uncompleted' USING ERRCODE='23514';
  END IF;
  NEW.revision=1;
 ELSE
  IF OLD.status IN('completed','cancelled') OR
     (NEW.tenant,NEW.warehouse_id,NEW.id,NEW.seq,NEW.sku,NEW.lot,NEW.from_location,NEW.quantity,
      NEW.source_receipt_id,NEW.source_event_id,NEW.created_by,NEW.created_at)
      IS DISTINCT FROM
     (OLD.tenant,OLD.warehouse_id,OLD.id,OLD.seq,OLD.sku,OLD.lot,OLD.from_location,OLD.quantity,
      OLD.source_receipt_id,OLD.source_event_id,OLD.created_by,OLD.created_at) THEN
   RAISE EXCEPTION 'Work identity and terminal history are immutable' USING ERRCODE='23514';
  END IF;
  IF NEW.owner IS DISTINCT FROM OLD.owner AND NOT
    (OLD.status='ready' AND NEW.status='claimed' AND NEW.owner=NEW.updated_by OR NEW.status='ready' AND NEW.owner IS NULL) THEN
   RAISE EXCEPTION 'Claim or release work before changing its owner' USING ERRCODE='23514';
  END IF;
  SELECT COALESCE(sum((x.command->>'quantity')::bigint),0) INTO confirmed
   FROM wms_work_executions x JOIN wms_commands c USING(tenant,warehouse_id,request_id)
   WHERE x.tenant=NEW.tenant AND x.warehouse_id=NEW.warehouse_id AND x.task_id=NEW.id;
  IF NEW.completed_quantity<>confirmed OR NEW.completed_quantity<OLD.completed_quantity THEN
   RAISE EXCEPTION 'Work progress requires its original move receipts' USING ERRCODE='23514';
  END IF;
  NEW.revision=OLD.revision+1;
 END IF;
 NEW.updated_at=clock_timestamp();
 RETURN NEW;
END
$$;
CREATE TRIGGER wms_work_task_guard BEFORE INSERT OR UPDATE ON wms_work_tasks FOR EACH ROW EXECUTE FUNCTION wms_work_task_guard();
CREATE FUNCTION wms_work_task_audit() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 PERFORM wms_bump_resource(NEW.tenant,NEW.warehouse_id,jsonb_build_array('work_task',NEW.id));
 INSERT INTO wms_work_task_events(tenant,warehouse_id,task_id,revision,actor,action,snapshot)
 VALUES(NEW.tenant,NEW.warehouse_id,NEW.id,NEW.revision,NEW.updated_by,NEW.last_action,to_jsonb(NEW));
 RETURN NULL;
END
$$;
CREATE TRIGGER wms_work_task_audit AFTER INSERT OR UPDATE ON wms_work_tasks FOR EACH ROW EXECUTE FUNCTION wms_work_task_audit();

-- A different SKU filling the recommended destination must invalidate the
-- review too. These independent counters never alter warehouse inventory.
CREATE FUNCTION wms_work_bin_revision() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE r RECORD; location_code TEXT;
BEGIN
 IF TG_OP='DELETE' THEN r=OLD; ELSE r=NEW; END IF;
 IF TG_OP='UPDATE' AND NEW IS NOT DISTINCT FROM OLD THEN RETURN NULL; END IF;
 IF TG_OP='UPDATE' THEN
  IF TG_TABLE_NAME='wms_stock' THEN
   IF (OLD.tenant,OLD.warehouse_id,OLD.location) IS DISTINCT FROM (NEW.tenant,NEW.warehouse_id,NEW.location) THEN
    PERFORM wms_bump_resource(OLD.tenant,OLD.warehouse_id,jsonb_build_array('putaway_bin',OLD.location));
   END IF;
  ELSIF (OLD.tenant,OLD.warehouse_id,OLD.code) IS DISTINCT FROM (NEW.tenant,NEW.warehouse_id,NEW.code) THEN
   PERFORM wms_bump_resource(OLD.tenant,OLD.warehouse_id,jsonb_build_array('putaway_bin',OLD.code));
  END IF;
 END IF;
 IF TG_TABLE_NAME='wms_stock' THEN location_code=r.location; ELSE location_code=r.code; END IF;
 PERFORM wms_bump_resource(r.tenant,r.warehouse_id,jsonb_build_array('putaway_bin',location_code));
 RETURN NULL;
END
$$;
CREATE TRIGGER wms_work_stock_revision AFTER INSERT OR UPDATE OR DELETE ON wms_stock FOR EACH ROW EXECUTE FUNCTION wms_work_bin_revision();
CREATE TRIGGER wms_work_capacity_revision AFTER INSERT OR UPDATE OR DELETE ON wms_location_capacity FOR EACH ROW EXECUTE FUNCTION wms_work_bin_revision();

CREATE FUNCTION wms_work_after_command() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE e JSONB; x wms_work_executions; t wms_work_tasks; q BIGINT; line RECORD;
BEGIN
 SELECT payload INTO e FROM wms_events WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id
  AND version=(NEW.result->>'version')::bigint AND payload->>'request_id'=NEW.request_id;
 SELECT * INTO x FROM wms_work_executions WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id AND request_id=NEW.request_id;
 IF FOUND THEN
  SELECT * INTO t FROM wms_work_tasks WHERE tenant=x.tenant AND warehouse_id=x.warehouse_id AND id=x.task_id FOR UPDATE;
  q=(x.command->>'quantity')::bigint;
  IF t.status<>'claimed' OR t.owner<>NEW.actor OR x.actor<>NEW.actor OR t.revision<>x.task_revision
    OR e->'command' IS DISTINCT FROM x.command OR q<=0 OR q>t.quantity-t.completed_quantity
    OR x.command->>'sku'<>t.sku OR x.command->>'lot'<>t.lot OR x.command->>'from'<>t.from_location THEN
   RAISE EXCEPTION 'Putaway requires its original owned review and physical move' USING ERRCODE='23514';
  END IF;
  UPDATE wms_work_tasks SET completed_quantity=completed_quantity+q,
   status=CASE WHEN completed_quantity+q=quantity THEN 'completed' ELSE 'claimed' END,
   updated_by=NEW.actor,last_action='putaway_confirmed',reason=NULL
   WHERE tenant=x.tenant AND warehouse_id=x.warehouse_id AND id=x.task_id;
 END IF;
 IF e->'command'->>'type'='receive' THEN
  SELECT * INTO line FROM wms_receipt_lines WHERE tenant=NEW.tenant AND warehouse_id=NEW.warehouse_id
   AND receipt_id=e->'command'->>'receipt_id' AND id=e->'command'->>'line_id';
  INSERT INTO wms_work_tasks(tenant,warehouse_id,id,sku,lot,from_location,quantity,source_receipt_id,source_event_id,created_by,updated_by)
   VALUES(NEW.tenant,NEW.warehouse_id,'putaway-'||(e->>'id'),line.sku,line.lot,e->'command'->>'location',
    (e->'command'->>'quantity')::bigint,line.receipt_id,e->>'id',NEW.actor,NEW.actor);
 END IF;
 RETURN NULL;
END
$$;
CREATE TRIGGER wms_work_after_command AFTER INSERT ON wms_commands FOR EACH ROW EXECUTE FUNCTION wms_work_after_command();
