#!/usr/bin/env python3
"""Execute the actual companion migration/function in socket-free PostgreSQL.

Proves SQL behavior, not HTTP auth, SQLx bindings or concurrent sessions.
"""
from pathlib import Path
import os
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent
BIN = Path(os.environ.get("WMS_PG_BIN", "/usr/lib/postgresql/12/bin"))
FAILURE = re.compile(r"\b(?:ERROR|FATAL|PANIC):")


def main() -> None:
    with tempfile.TemporaryDirectory(prefix="stateset-oms-handoff-") as cluster:
        subprocess.run(
            [str(BIN / "initdb"), "-D", cluster, "-U", "warehouse", "--auth-local=trust",
             "--auth-host=reject", "--no-locale", "--encoding=UTF8", "--no-sync"],
            check=True, capture_output=True, timeout=60,
        )
        sql = "SET statement_timeout='15s';\n" + (ROOT / "proof.sql").read_text()
        sql += "\n" + (ROOT / "migrations/20261017000000_warehouse_fulfillment_receipts.sql").read_text()
        sql += "\nPREPARE receiver AS SELECT apply_warehouse_fulfillment($1,$2,$3);\n"
        sql += "\nPREPARE receiver_capability AS SELECT warehouse_fulfillment_contract() || jsonb_build_object('tenant_id',$1::text);\n"
        sql += "\n" + (ROOT / "assertions.sql").read_text()
        # --single -j treats an empty line as a query boundary even inside a
        # dollar-quoted function. Preserve SQL text but remove empty input lines.
        sql = "\n".join(line for line in sql.splitlines() if line.strip()) + "\n"
        result = subprocess.run(
            [str(BIN / "postgres"), "--single", "-j", "-D", cluster, "postgres"],
            input=sql, text=True, capture_output=True, timeout=60,
        )
        output = result.stdout + result.stderr
        if result.returncode or FAILURE.search(output) or "PASS:conditional_oms_handoff_receiver" not in result.stdout:
            print(output)
            raise SystemExit("Conditional OMS receiver SQL proof failed")
        print("PASS: conditional OMS receiver: exact repeated-SKU lines, terminal/revision fences, durable rejections and reviewed replacement, immutable replay, tenant isolation, unrelated-field preservation and final-write rollback (real PostgreSQL, one session)")


if __name__ == "__main__":
    main()
