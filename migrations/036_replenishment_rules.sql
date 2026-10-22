-- Saved pick-face replenishment rules, so the due work queue can be
-- evaluated at any time without resubmitting rules. Rules are planning
-- configuration: they reserve and move nothing, and every replenishment
-- task is still an ordinary reviewed move_stock command. Each replacement
-- of a warehouse's rule set is recorded in the immutable admin audit.
CREATE TABLE wms_replenishment_rules (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,sku TEXT NOT NULL,destination TEXT NOT NULL,
 minimum BIGINT NOT NULL CHECK(minimum>=1),target BIGINT NOT NULL CHECK(target>=minimum AND target<=1000000),
 source_zone TEXT CHECK(source_zone IS NULL OR length(source_zone) BETWEEN 1 AND 240),
 updated_by TEXT NOT NULL,updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(tenant,warehouse_id,sku,destination),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku) ON DELETE CASCADE,
 FOREIGN KEY(tenant,warehouse_id,destination) REFERENCES wms_locations(tenant,warehouse_id,code) ON DELETE CASCADE
);
