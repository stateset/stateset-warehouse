-- Isolated, real PostgreSQL proof. No mocks and no existing application DB.
CREATE TABLE orders (
 id UUID PRIMARY KEY,tenant_id TEXT NOT NULL,status TEXT NOT NULL,
 line_items JSONB NOT NULL,updated_at TIMESTAMPTZ NOT NULL,
 fulfillment_status TEXT,netsuite_id TEXT,tracking JSONB,
 source_order_payload JSONB
);

-- Migration is inserted here by check.py, before the actual assertions.
