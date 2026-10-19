-- pg_restore clears the session search_path before COPY. The serial place
-- CHECK predicate calls its scalar helper from PL/pgSQL, so that lookup must
-- use the application schema even in a restore session. Preserve the original
-- predicate body and its validations; this changes no identity or custody data.
ALTER FUNCTION public.wms_serial_place_valid(JSONB)
    SET search_path = pg_catalog, public;
