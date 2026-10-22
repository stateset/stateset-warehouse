-- Resolve one command's original target rows once, with the exact exception
-- outcome of wms_serial_inputs_match_command from migration 023. NULL means
-- resolution raised one of the conversion errors that function maps to false;
-- every other error still propagates. Reconciliation shares these rows across
-- the input classes of one command inside its own read-only statement and
-- evaluates the remaining input predicates in set form. The original matcher,
-- write guards and standalone checks are unchanged.
CREATE FUNCTION wms_serial_command_targets(t TEXT,w TEXT,c JSONB) RETURNS JSONB LANGUAGE plpgsql STABLE AS $$
DECLARE target_rows JSONB;
BEGIN
 SELECT COALESCE(jsonb_agg(to_jsonb(q)),'[]'::jsonb) INTO target_rows FROM wms_serial_targets(t,w,c) q;
 RETURN target_rows;
EXCEPTION WHEN invalid_text_representation OR numeric_value_out_of_range THEN RETURN NULL;
END $$;
