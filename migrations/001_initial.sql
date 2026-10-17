-- Independent database backstop for physical stock and reservation quantities.
-- The application additionally validates movement/line reconciliation.
CREATE OR REPLACE FUNCTION wms_valid_balances(document JSONB) RETURNS BOOLEAN
LANGUAGE SQL IMMUTABLE AS $$
    SELECT CASE WHEN jsonb_typeof(document->'stocks') IS DISTINCT FROM 'array' THEN false ELSE
      NOT EXISTS (
        SELECT 1 FROM jsonb_array_elements(document->'stocks') AS b
        WHERE NOT COALESCE(
          jsonb_typeof(b->'on_hand') = 'number' AND jsonb_typeof(b->'reserved') = 'number'
          AND (b->>'on_hand')::numeric = trunc((b->>'on_hand')::numeric)
          AND (b->>'reserved')::numeric = trunc((b->>'reserved')::numeric)
          AND (b->>'on_hand')::numeric BETWEEN 0 AND 1000000000
          AND (b->>'reserved')::numeric BETWEEN 0 AND (b->>'on_hand')::numeric,
          false
        )
      ) AND (
        SELECT count(*) = count(DISTINCT jsonb_build_array(b->'sku', b->'lot', b->'location'))
        FROM jsonb_array_elements(document->'stocks') AS b
      )
    END;
$$;
CREATE TABLE IF NOT EXISTS wms_workspaces (
    tenant TEXT NOT NULL,
    warehouse_id TEXT NOT NULL,
    version BIGINT NOT NULL DEFAULT 0 CHECK (version >= 0),
    state JSONB NOT NULL CHECK (wms_valid_balances(state)),
    CHECK (COALESCE((state->>'version')::BIGINT = version, false)),
    CHECK (COALESCE(state#>>'{warehouse,id}' = warehouse_id, false)),
    PRIMARY KEY (tenant, warehouse_id)
);
CREATE TABLE IF NOT EXISTS wms_commands (
    tenant TEXT NOT NULL,
    warehouse_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    actor TEXT NOT NULL,
    digest TEXT NOT NULL,
    result JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant, warehouse_id, request_id)
);
CREATE TABLE IF NOT EXISTS wms_events (
    id TEXT PRIMARY KEY,
    tenant TEXT NOT NULL,
    warehouse_id TEXT NOT NULL,
    version BIGINT NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant, warehouse_id, version)
);
CREATE TABLE IF NOT EXISTS wms_outbox (
    id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL REFERENCES wms_events(id),
    tenant TEXT NOT NULL,
    destination TEXT NOT NULL CHECK (destination IN ('oms','tms')),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','delivering','delivered','dead_letter')),
    attempts BIGINT NOT NULL DEFAULT 0,
    due_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_token TEXT,
    lease_until TIMESTAMPTZ,
    last_error TEXT NOT NULL DEFAULT '',
    delivered_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS wms_outbox_due ON wms_outbox (due_at) WHERE status IN ('pending','delivering');
CREATE INDEX IF NOT EXISTS wms_outbox_tenant ON wms_outbox (tenant, event_id);
CREATE TABLE IF NOT EXISTS wms_delivery_attempts (
    id TEXT PRIMARY KEY,
    outbox_id TEXT NOT NULL REFERENCES wms_outbox(id),
    outcome TEXT NOT NULL DEFAULT 'started',
    detail TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);
CREATE TABLE IF NOT EXISTS wms_admin_audit (
    id TEXT PRIMARY KEY,
    tenant TEXT NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    reference TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
