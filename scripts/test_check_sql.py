"""Extractor contracts; the real single-user PostgreSQL check validates the SQL."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("check_sql", Path(__file__).with_name("check-sql.py"))
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class RuntimeSqlExtraction(unittest.TestCase):
    def extract(self, source, included=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "src").mkdir()
            (root / "src/query.rs").write_text(source)
            if included is not None:
                (root / "src/query.sql").write_text(included)
            with patch.object(CHECK, "ROOT", root):
                return [query for _, query in CHECK.runtime_queries()]

    def test_raw_timestamp_sql_keeps_quotes_and_does_not_extract_its_interior(self):
        sql = "SELECT to_char(clock_timestamp(), 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')"
        self.assertEqual(self.extract('sqlx::query_scalar(r#"' + sql + '"#)'), [sql])

    def test_raw_hash_delimiters_and_literal_backslashes_are_preserved(self):
        sql = r'''SELECT '\\path' AS "id", '"# is content' AS text'''
        self.assertEqual(self.extract('sqlx::query(r##"' + sql + '"##)'), [sql])

    def test_included_sql_with_header_comments_is_actually_prepared(self):
        self.assertEqual(
            self.extract('sqlx::query(include_str!("query.sql"))', '-- original header\n/* review */\nSELECT $1::text;'),
            ['SELECT $1::text'],
        )

    def test_ordinary_escaped_sql_and_whitelisted_history_expansion_remain_covered(self):
        source = r'''sqlx::query("SELECT \"name\" FROM wms_products"); let sql="SELECT * FROM {table} WHERE tenant=$1";'''
        queries = self.extract(source)
        self.assertIn('SELECT "name" FROM wms_products', queries)
        self.assertEqual(len(queries), 6)

    def test_non_sql_rust_escapes_are_not_decoded_as_json_sql(self):
        source = r'''let control="\0"; let unicode="\u{001b}"; sqlx::query("SELECT 1");'''
        self.assertEqual(self.extract(source), ["SELECT 1"])


if __name__ == "__main__":
    unittest.main()
