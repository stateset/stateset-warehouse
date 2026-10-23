-- Reviewed product GTINs, so one GS1 scan can identify the product, and
-- optional per-bin capacity in whole units and distinct SKUs, which putaway
-- respects. Both are administrator-managed master data: they move no stock,
-- and each replacement is recorded in the immutable admin audit. Rows are
-- removed with their product or bin.
CREATE TABLE wms_product_gtins (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,gtin TEXT NOT NULL CHECK(gtin ~ '^[0-9]{14}$'),
 sku TEXT NOT NULL,updated_by TEXT NOT NULL,updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(tenant,warehouse_id,gtin),
 FOREIGN KEY(tenant,warehouse_id,sku) REFERENCES wms_products(tenant,warehouse_id,sku) ON DELETE CASCADE
);
CREATE INDEX wms_product_gtins_sku ON wms_product_gtins(tenant,warehouse_id,sku);

CREATE TABLE wms_location_capacity (
 tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,code TEXT NOT NULL,
 max_units BIGINT NOT NULL CHECK(max_units BETWEEN 1 AND 1000000000),
 max_skus INTEGER CHECK(max_skus IS NULL OR max_skus BETWEEN 1 AND 1000),
 updated_by TEXT NOT NULL,updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 PRIMARY KEY(tenant,warehouse_id,code),
 FOREIGN KEY(tenant,warehouse_id,code) REFERENCES wms_locations(tenant,warehouse_id,code) ON DELETE CASCADE
);
