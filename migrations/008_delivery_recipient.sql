-- Freeze the exact receiver URL before the first possible external effect.
-- Store a digest, never credentials or a potentially sensitive URL. Historical
-- sends cannot be assigned today's configuration without independent evidence.
ALTER TABLE wms_outbox ADD COLUMN recipient_fingerprint TEXT
 CHECK (recipient_fingerprint = 'unverified' OR recipient_fingerprint ~ '^[0-9a-f]{64}$');
CREATE INDEX wms_delivery_attempts_outbox ON wms_delivery_attempts(outbox_id);
UPDATE wms_outbox o SET recipient_fingerprint='unverified'
 WHERE attempts > 0 OR EXISTS (SELECT 1 FROM wms_delivery_attempts a WHERE a.outbox_id=o.id);

CREATE FUNCTION wms_guard_delivery_recipient() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 -- Older binaries do not supply this transaction-local receiver assertion.
 -- Reject their claim before commit, so their HTTP send is never reached.
 IF NEW.attempts > OLD.attempts AND (
     NEW.status <> 'delivering' OR NEW.recipient_fingerprint IS NULL
     OR NEW.recipient_fingerprint = 'unverified'
     OR current_setting('wms.delivery_recipient', true)
        IS DISTINCT FROM NEW.recipient_fingerprint) THEN
   RAISE EXCEPTION 'Delivery claim requires its original receiver assertion'
     USING ERRCODE='23514';
 END IF;
 IF NEW.event_id IS DISTINCT FROM OLD.event_id
    OR NEW.tenant IS DISTINCT FROM OLD.tenant
    OR NEW.destination IS DISTINCT FROM OLD.destination
    OR (OLD.recipient_fingerprint IS NOT NULL
        AND NEW.recipient_fingerprint IS DISTINCT FROM OLD.recipient_fingerprint)
    OR (OLD.recipient_fingerprint IS NULL AND NEW.recipient_fingerprint IS NOT NULL
        AND (OLD.attempts > 0 OR EXISTS (
            SELECT 1 FROM wms_delivery_attempts WHERE outbox_id=OLD.id))) THEN
   RAISE EXCEPTION 'Delivery event and original receiver identity are immutable'
     USING ERRCODE='23514';
 END IF;
 RETURN NEW;
END;
$$;
CREATE TRIGGER wms_outbox_recipient_guard BEFORE UPDATE ON wms_outbox
 FOR EACH ROW EXECUTE FUNCTION wms_guard_delivery_recipient();
