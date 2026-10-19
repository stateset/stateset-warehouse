-- Exact request recovery depends on retaining the original actor, digest and
-- result. These records have the same durability boundary as physical history.
CREATE TRIGGER wms_commands_immutable BEFORE UPDATE OR DELETE ON wms_commands
 FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_commands_no_truncate BEFORE TRUNCATE ON wms_commands
 FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_admin_audit_immutable BEFORE UPDATE OR DELETE ON wms_admin_audit
 FOR EACH ROW EXECUTE FUNCTION wms_immutable_history();
CREATE TRIGGER wms_admin_audit_no_truncate BEFORE TRUNCATE ON wms_admin_audit
 FOR EACH STATEMENT EXECUTE FUNCTION wms_immutable_history();
