#!/usr/bin/env python3
"""Parse/type-check actual runtime SQL with an isolated single-user Postgres.

Useful where sockets are prohibited. This does NOT test SQLx bindings, HTTP,
multi-session concurrency or transaction behavior. Run the Postgres suite too.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

root = Path(__file__).resolve().parent.parent
bin_dir = Path(os.environ.get("WMS_PG_BIN", "/usr/lib/postgresql/12/bin"))
with tempfile.TemporaryDirectory(prefix="stateset-wms-sql-") as tmp:
    subprocess.run([str(bin_dir / "initdb"), "-D", tmp, "-U", "warehouse", "-A", "trust", "--no-locale", "-E", "UTF8"], check=True, capture_output=True)
    queries = []
    for path in sorted((root / "src").glob("*.rs")):
        for raw in re.findall(r'sqlx::query(?:_scalar)?\(\s*"((?:\\.|[^"\\])*)"', path.read_text()):
            query = json.loads('"' + raw + '"')
            if not query.startswith("SET "):
                queries.append((path.name, query))
    schema = (root / "migrations/001_initial.sql").read_text()
    sql = schema + "\n" + "\n".join(f"PREPARE wms_check_{i} AS {q};" for i, (_, q) in enumerate(queries))
    sql += "\nSELECT count(*) AS checked_statements FROM pg_prepared_statements;\n"
    sql += """
    DO $$ BEGIN
      BEGIN
        INSERT INTO wms_workspaces(tenant,warehouse_id,version,state)
        VALUES ('smoke','wh',0,'{"version":0,"warehouse":{"id":"wh"},"stocks":[{"sku":"A","lot":"L","location":"A","on_hand":1,"reserved":2}]}'::jsonb);
        RAISE EXCEPTION 'Database accepted reserved stock exceeding physical stock';
      EXCEPTION WHEN check_violation THEN NULL;
      END;
    END $$;
    """
    result = subprocess.run([str(bin_dir / "postgres"), "--single", "-j", "-D", tmp, "postgres"], input=sql, text=True, capture_output=True)
    if result.returncode or re.search(r"ERROR:|FATAL:", result.stderr):
        print(result.stderr)
        raise SystemExit(1)
    print(f"Postgres schema and {len(queries)} runtime statements parsed and type-checked. Concurrency and client bindings remain unverified.")
