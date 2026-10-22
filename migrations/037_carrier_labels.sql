-- Carrier labels attached to packed packages. A label is recorded by the TMS
-- or by an operator copying it from a carrier system. It records the actual
-- carrier, service, tracking number, quoted rate and observed carton, and its
-- printable label. Rows are immutable apart from a single void. At most one
-- active label per package. Recording a label moves no inventory and does not
-- hand the package off; handoff remains the reviewed handoff_shipment command.
CREATE TABLE wms_carrier_labels (
 seq BIGSERIAL UNIQUE,tenant TEXT NOT NULL,warehouse_id TEXT NOT NULL,id TEXT NOT NULL,package_id TEXT NOT NULL,
 carrier TEXT NOT NULL CHECK(length(btrim(carrier)) BETWEEN 1 AND 120),
 service TEXT NOT NULL CHECK(length(btrim(service)) BETWEEN 1 AND 120),
 tracking_number TEXT NOT NULL CHECK(tracking_number ~ '^[\x21-\x7e]{1,64}$'),
 rate_amount TEXT CHECK(rate_amount IS NULL OR rate_amount ~ '^(0|[1-9][0-9]{0,11})(\.[0-9]{1,8})?$'),
 rate_currency TEXT CHECK(rate_currency IS NULL OR rate_currency ~ '^[A-Z]{3}$'),
 weight_grams BIGINT CHECK(weight_grams IS NULL OR weight_grams BETWEEN 1 AND 1000000000),
 length_mm BIGINT CHECK(length_mm IS NULL OR length_mm BETWEEN 1 AND 100000),
 width_mm BIGINT CHECK(width_mm IS NULL OR width_mm BETWEEN 1 AND 100000),
 height_mm BIGINT CHECK(height_mm IS NULL OR height_mm BETWEEN 1 AND 100000),
 label_format TEXT NOT NULL CHECK(label_format IN('zpl','url')),
 label_zpl TEXT CHECK(label_zpl IS NULL OR (left(label_zpl,3)='^XA' AND octet_length(label_zpl)<=65536)),
 label_url TEXT CHECK(label_url IS NULL OR (label_url ~ '^https://' AND length(label_url)<=2048)),
 recorded_by TEXT NOT NULL,recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
 voided_by TEXT,voided_at TIMESTAMPTZ,void_reason TEXT,
 PRIMARY KEY(tenant,warehouse_id,id),
 FOREIGN KEY(tenant,warehouse_id,package_id) REFERENCES wms_packages(tenant,warehouse_id,id),
 CHECK((rate_amount IS NULL)=(rate_currency IS NULL)),
 CHECK((label_format='zpl' AND label_zpl IS NOT NULL AND label_url IS NULL) OR (label_format='url' AND label_url IS NOT NULL AND label_zpl IS NULL)),
 CHECK((voided_at IS NULL AND voided_by IS NULL AND void_reason IS NULL)
    OR (voided_at IS NOT NULL AND voided_by IS NOT NULL AND length(btrim(void_reason)) BETWEEN 1 AND 240))
);
CREATE UNIQUE INDEX wms_carrier_label_active ON wms_carrier_labels(tenant,warehouse_id,package_id) WHERE voided_at IS NULL;

-- Everything except a first void is immutable.
CREATE FUNCTION wms_carrier_label_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' OR OLD.voided_at IS NOT NULL OR NEW.voided_at IS NULL
  OR (to_jsonb(NEW)-'voided_by'-'voided_at'-'void_reason') IS DISTINCT FROM (to_jsonb(OLD)-'voided_by'-'voided_at'-'void_reason') THEN
  RAISE EXCEPTION 'Carrier labels are immutable apart from one void' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_carrier_label_guard BEFORE UPDATE OR DELETE ON wms_carrier_labels FOR EACH ROW EXECUTE FUNCTION wms_carrier_label_guard();
CREATE TRIGGER wms_carrier_label_no_truncate BEFORE TRUNCATE ON wms_carrier_labels FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();

-- A label can only be added while its package is still open.
CREATE FUNCTION wms_carrier_label_open_package() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF EXISTS(SELECT 1 FROM wms_package_closures c WHERE c.tenant=NEW.tenant AND c.warehouse_id=NEW.warehouse_id AND c.package_id=NEW.package_id) THEN
  RAISE EXCEPTION 'Package is already handed off or unpacked' USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER wms_carrier_label_open_package BEFORE INSERT ON wms_carrier_labels FOR EACH ROW EXECUTE FUNCTION wms_carrier_label_open_package();
