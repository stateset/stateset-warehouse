#!/usr/bin/env python3
"""Check runtime SQL and database invariants with isolated, socket-free Postgres.

Applies every migration in filename order, PREPAREs runtime statements, and runs
independent ledger, append-only history, and fault-injected rollback assertions.
Does not test SQLx bindings, HTTP, application transaction boundaries, or
multi-session concurrency. Run the real Postgres integration suite as well.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent
BIN_DIR = Path(os.environ.get("WMS_PG_BIN", "/usr/lib/postgresql/12/bin"))
FAILURE = re.compile(r"\b(?:ERROR|FATAL|PANIC):")
QUERY_CALL = re.compile(r"sqlx::query(?:_scalar|_as)?(?:\s*::<[^>]+>)?\(\s*")
RUST_STRING = re.compile(
    r'\br(?P<hashes>\#{0,255})"(?P<raw>.*?)"(?P=hashes)|"(?P<ordinary>(?:\\.|[^"\\])*)"',
    re.DOTALL,
)
INCLUDE_SQL = re.compile(r'include_str!\(\s*"([^"\n]+)"\s*\)')
PREPARABLE = re.compile(r"^(?:SELECT|INSERT|UPDATE|DELETE|WITH)\b", re.IGNORECASE)


def decode_rust_string(raw: str) -> str:
    """Decode ordinary Rust SQL literals, including continued source lines."""
    continued = re.sub(r"\\\n\s*", "", raw)
    return json.loads('"' + continued + '"', strict=False)


def string_value(match: re.Match) -> str:
    raw = match.group("raw")
    return raw if raw is not None else decode_rust_string(match.group("ordinary"))


def sql_body(query: str) -> str:
    """Remove leading comments for classification without rewriting SQL literals."""
    while True:
        query = query.lstrip()
        if query.startswith("--"):
            query = query.partition("\n")[2]
        elif query.startswith("/*") and "*/" in query:
            query = query.partition("*/")[2]
        else:
            return query


def runtime_queries() -> list[tuple[str, str]]:
    queries = []
    for path in sorted((ROOT / "src").rglob("*.rs")):
        source = path.read_text()
        for call in QUERY_CALL.finditer(source):
            literal = RUST_STRING.match(source, call.end())
            included = INCLUDE_SQL.match(source, call.end())
            if literal:
                query = sql_body(string_value(literal))
            elif included:
                sql_path = (path.parent / included.group(1)).resolve()
                if ROOT not in sql_path.parents:
                    raise SystemExit(f"SQL include escapes repository: {sql_path}")
                query = sql_body(sql_path.read_text())
            else:
                # Dynamic history SQL expands only whitelisted table names.
                continue
            if PREPARABLE.match(query) or query.upper().startswith("CREATE TABLE IF NOT EXISTS WMS_SCHEMA_MIGRATIONS"):
                queries.append((str(path.relative_to(ROOT)), query.rstrip(";")))

        # Also cover statements selected through a local match expression.
        # PREPARE cannot parameterize an identifier, so expand history tables.
        for literal in RUST_STRING.finditer(source):
            candidate = literal.group("raw")
            if candidate is None:
                candidate = literal.group("ordinary")
            if not PREPARABLE.match(sql_body(candidate)):
                continue
            query = sql_body(string_value(literal))
            if not PREPARABLE.match(query):
                continue
            if "{table}" not in query:
                queries.append((str(path.relative_to(ROOT)), query.rstrip(";")))
                continue
            for table in (
                "wms_activity", "wms_movements", "wms_orders",
                "wms_receipts", "wms_exceptions",
            ):
                expanded = query.replace("{table}", table)
                if re.search(r"\{[A-Za-z_]", expanded):
                    raise SystemExit(f"Unhandled dynamic SQL in {path.name}: {expanded}")
                queries.append((str(path.relative_to(ROOT)), expanded.rstrip(";")))
    return list(dict.fromkeys(queries))


def check_sql(cluster: str, sql: str, label: str, marker: str | None = None) -> None:
    result = subprocess.run(
        [str(BIN_DIR / "postgres"), "--single", "-j", "-D", cluster, "postgres"],
        input="SET statement_timeout='15s';\n" + sql + "\n",
        text=True,
        capture_output=True,
        timeout=60,
    )
    output = result.stdout + result.stderr
    # Standalone PostgreSQL can return status 0 after a SQL error. Inspect both
    # streams and require explicit SELECT markers from the assertion scripts.
    if result.returncode or FAILURE.search(output):
        print(f"PostgreSQL validation failed: {label}")
        print(output)
        raise SystemExit(1)
    if marker and marker not in result.stdout:
        print(f"PostgreSQL validation produced no assertion marker: {label}")
        print(output)
        raise SystemExit(1)


def main() -> None:
    for binary in ("initdb", "postgres"):
        if not (BIN_DIR / binary).is_file():
            raise SystemExit(f"PostgreSQL binary missing: {BIN_DIR / binary}; set WMS_PG_BIN")
    with tempfile.TemporaryDirectory(prefix="stateset-wms-sql-") as cluster:
        subprocess.run(
            [
                str(BIN_DIR / "initdb"), "-D", cluster, "-U", "warehouse",
                "--auth-local=trust", "--auth-host=reject", "--no-locale",
                "--encoding=UTF8", "--no-sync",
            ],
            check=True,
            capture_output=True,
            timeout=60,
        )
        migrations = sorted((ROOT / "migrations").glob("[0-9]*.sql"))
        if not migrations:
            raise SystemExit("No database migrations found")
        for migration in migrations:
            check_sql(cluster, migration.read_text(), migration.name)

        statements = runtime_queries()
        for source, statement in statements:
            if statement.upper().startswith("CREATE TABLE"):
                check_sql(cluster, statement + ";", f"migration bookkeeping ({source})")
        queries = [(source, statement) for source, statement in statements if PREPARABLE.match(statement)]
        if not queries:
            raise SystemExit("No runtime SQL found; refusing an empty validation")
        prepared = "\n".join(
            f"PREPARE wms_check_{i} AS {query};" for i, (_, query) in enumerate(queries)
        )
        marker = f"PASS:runtime_sql:{len(queries)}"
        prepared += (
            "\nDO $$ BEGIN IF (SELECT count(*) FROM pg_prepared_statements) <> "
            + str(len(queries))
            + " THEN RAISE EXCEPTION 'Incomplete runtime SQL validation'; END IF; END $$;\n"
            + f"SELECT '{marker}' AS wms_sql_proof;\n"
        )
        check_sql(cluster, prepared, "runtime SQL", marker)

        for name, marker in (
            ("relational-proof.sql", "PASS:relational_inventory_proofs"),
            ("fault-proof.sql", "PASS:atomic_outbox_rollback"),
        ):
            check_sql(cluster, (ROOT / "scripts" / name).read_text(), name, marker)

        # Execute the actual independent reconciliation query against the
        # receipt, repeated-SKU allocation, and partial-pick proof fixture.
        reconciliation = (ROOT / "src" / "reconciliation.sql").read_text().strip().rstrip(";")
        reconciliation = re.sub(r"\$1\b", "'sql-proof'", reconciliation)
        reconciliation = re.sub(r"\$2\b", "'wh-proof'", reconciliation)
        check_sql(
            cluster,
            "CREATE TEMP TABLE wms_actual_reconciliation AS " + reconciliation + ";\n"
            "DO $$ BEGIN "
            "IF (SELECT count(*) FROM wms_actual_reconciliation)<>3 OR EXISTS("
            "SELECT 1 FROM wms_actual_reconciliation WHERE on_hand<>ledger "
            "OR reserved<>allocation_reserved) THEN "
            "RAISE EXCEPTION 'Runtime reconciliation disagrees with physical or allocation evidence'; "
            "END IF; END $$;\n"
            "SELECT 'PASS:runtime_reconciliation' AS wms_sql_proof;\n",
            "runtime reconciliation execution",
            "PASS:runtime_reconciliation",
        )

        print(
            f"PostgreSQL {len(migrations)} migrations and {len(queries)} runtime statements "
            "validated. Runtime reconciliation, ledger, receipt evidence, append-only history, and injected "
            "outbox-failure rollback proofs passed."
        )
        print("SQLx bindings, HTTP, and concurrent sessions remain unverified by this check.")


if __name__ == "__main__":
    main()
