-- Common printable ASCII identifiers have the same literal validity under the
-- original Unicode rules. Accept this strict subset with one native predicate;
-- all other input retains the complete original Unicode/control/byte rules.
-- No source verdict is stored. The original command target, member shape,
-- duplicate and quantity checks continue to validate every input label.
CREATE OR REPLACE FUNCTION wms_serial_text(s TEXT) RETURNS BOOLEAN LANGUAGE SQL IMMUTABLE AS $$
 SELECT CASE WHEN octet_length(s) BETWEEN 1 AND 240
  AND s COLLATE "C" ~ '^[!-~]([ -~]*[!-~])?$' THEN true
 ELSE COALESCE(octet_length(s) BETWEEN 1 AND 240
  AND s=btrim(s,U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000')
  AND s !~ '[[:cntrl:]]' AND s !~ U&'[\007F-\009F]',false) END;
$$;
