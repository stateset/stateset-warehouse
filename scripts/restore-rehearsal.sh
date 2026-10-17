#!/usr/bin/env bash
# Round-trip an isolated synthetic warehouse through a real PostgreSQL archive.
# The supplied server is used only to create uniquely named rehearsal databases.
set -euo pipefail
umask 077
cd "$(dirname "$0")/.."

: "${WMS_RESTORE_ADMIN_URL:=${WMS_TEST_DATABASE_URL:-}}"
: "${WMS_RESTORE_ADMIN_URL:?Set WMS_RESTORE_ADMIN_URL to a PostgreSQL role allowed to create isolated databases}"
export WMS_RESTORE_ADMIN_URL
if [[ -n "${WMS_PG_BIN:-}" ]]; then
  wms_pg_bin="$WMS_PG_BIN"
elif command -v pg_config >/dev/null 2>&1; then
  wms_pg_bin="$(pg_config --bindir)"
else
  wms_pg_bin="$(dirname "$(command -v pg_dump)")"
fi
for wms_binary in pg_dump pg_restore; do
  if [[ ! -x "$wms_pg_bin/$wms_binary" ]]; then
    echo "Missing PostgreSQL executable: $wms_pg_bin/$wms_binary" >&2
    exit 1
  fi
done
if [[ -z "${WMS_RESTORE_PROBE:-}" ]]; then
  cargo build --locked --example restore_probe
  WMS_RESTORE_PROBE="$PWD/target/debug/examples/restore_probe"
fi
if [[ ! -x "$WMS_RESTORE_PROBE" ]]; then
  echo 'WMS_RESTORE_PROBE must identify the compiled restore_probe executable.' >&2
  exit 1
fi
wms_restore_root="${WMS_RESTORE_ARTIFACT_DIR:-$(mktemp -d /tmp/stateset-wms-restore.XXXXXXXX)}"
mkdir -p "$wms_restore_root"
if [[ -n "$(ls -A "$wms_restore_root")" ]]; then
  echo 'WMS_RESTORE_ARTIFACT_DIR must be empty; existing evidence will not be overwritten.' >&2
  exit 1
fi
wms_restore_root="$(cd "$wms_restore_root" && pwd)"
wms_suffix="$(python3 -c 'import secrets,time; print(str(int(time.time()))+"_"+secrets.token_hex(4))')"
wms_source="wms_restore_source_test_$wms_suffix"
wms_target="wms_restore_target_test_$wms_suffix"
wms_source_created=0
wms_target_created=0
cleanup() {
  local result=$?
  trap - EXIT INT TERM
  if [[ "${WMS_KEEP_RESTORE_DB:-0}" != 1 ]]; then
    if [[ "$wms_target_created" == 1 ]]; then
      "$WMS_RESTORE_PROBE" drop "$wms_target" >> "$wms_restore_root/cleanup.log" 2>&1 || result=1
    fi
    if [[ "$wms_source_created" == 1 ]]; then
      "$WMS_RESTORE_PROBE" drop "$wms_source" >> "$wms_restore_root/cleanup.log" 2>&1 || result=1
    fi
  else
    printf 'Synthetic rehearsal databases retained: %s %s\n' "$wms_source" "$wms_target" >&2
  fi
  printf 'Restore rehearsal evidence: %s\n' "$wms_restore_root"
  if [[ "$result" != 0 ]]; then
    echo 'Restore qualification failed; retain the evidence and inspect the failed stage.' >&2
  fi
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Preserve credentials/query options without writing URLs into the evidence.
wms_database_url() {
  WMS_RESTORE_DATABASE_NAME="$1" python3 - <<'PY'
import os, urllib.parse
url = urllib.parse.urlsplit(os.environ["WMS_RESTORE_ADMIN_URL"])
if url.scheme not in ("postgres", "postgresql"):
    raise SystemExit("A PostgreSQL URL is required")
database = urllib.parse.quote(os.environ["WMS_RESTORE_DATABASE_NAME"], safe="")
# urlunsplit removes // for schemes without an authority. PostgreSQL's Unix
# socket URI still requires postgresql:///name, including its empty authority.
print(url.scheme + "://" + url.netloc + "/" + database + ("?" + url.query if url.query else ""))
PY
}
wms_source_url="$(wms_database_url "$wms_source")"
wms_target_url="$(wms_database_url "$wms_target")"
"$WMS_RESTORE_PROBE" create "$wms_source"
wms_source_created=1
DATABASE_URL="$wms_source_url" "$WMS_RESTORE_PROBE" seed "$wms_restore_root/recovery-point.json" \
  > "$wms_restore_root/seed.log" 2>&1
"$wms_pg_bin/pg_dump" --version > "$wms_restore_root/pg-dump-version.txt"
"$wms_pg_bin/pg_restore" --version > "$wms_restore_root/pg-restore-version.txt"
python3 - "$wms_restore_root" <<'PY'
import json, pathlib, re, sys
root = pathlib.Path(sys.argv[1])
server = json.loads((root / "recovery-point.json").read_text())["evidence"]["postgres_version"].split(".")[0]
for name in ("pg-dump-version.txt", "pg-restore-version.txt"):
    match = re.search(r"PostgreSQL\) (\d+)", (root / name).read_text())
    if not match or match[1] != server:
        raise SystemExit("Use pg_dump/pg_restore matching the server major version; set WMS_PG_BIN")
PY
wms_started="$(date +%s)"
"$wms_pg_bin/pg_dump" --dbname "$wms_source_url" --format=custom \
  --file "$wms_restore_root/warehouse.dump" > "$wms_restore_root/dump.log" 2>&1
"$wms_pg_bin/pg_restore" --list "$wms_restore_root/warehouse.dump" \
  > "$wms_restore_root/archive.list"
(cd "$wms_restore_root" && sha256sum warehouse.dump > warehouse.dump.sha256)
"$WMS_RESTORE_PROBE" create "$wms_target"
wms_target_created=1
"$wms_pg_bin/pg_restore" --dbname "$wms_target_url" --exit-on-error --single-transaction \
  --no-owner --no-privileges "$wms_restore_root/warehouse.dump" > "$wms_restore_root/restore.log" 2>&1
DATABASE_URL="$wms_target_url" "$WMS_RESTORE_PROBE" verify \
  "$wms_restore_root/recovery-point.json" "$wms_restore_root/verification.json" \
  > "$wms_restore_root/verification.log" 2>&1
wms_finished="$(date +%s)"
WMS_REHEARSAL_SECONDS="$((wms_finished-wms_started))" WMS_PROBE_PATH="$WMS_RESTORE_PROBE" \
  python3 - "$wms_restore_root" <<'PY'
import hashlib, json, os, pathlib, subprocess, sys
root = pathlib.Path(sys.argv[1])
revision = os.environ.get("WMS_RELEASE_REVISION")
if not revision:
    result = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True, text=True)
    revision = result.stdout.strip() if result.returncode == 0 else "unrecorded-working-tree"
report = {
    "ok": True, "synthetic": True, "release_revision": revision,
    "elapsed_archive_restore_verify_seconds": int(os.environ["WMS_REHEARSAL_SECONDS"]),
    "cargo_lock_sha256": hashlib.sha256(pathlib.Path("Cargo.lock").read_bytes()).hexdigest(),
    "probe_binary_sha256": hashlib.sha256(pathlib.Path(os.environ["WMS_PROBE_PATH"]).read_bytes()).hexdigest(),
    "archive_sha256": hashlib.sha256((root / "warehouse.dump").read_bytes()).hexdigest(),
    "verification": json.loads((root / "verification.json").read_text()),
    "qualification_limit": "Synthetic database recovery and application invariants; no facility or external service acceptance claimed",
}
(root / "release-qualification.json").write_text(json.dumps(report, indent=2) + "\n")
print("PASS: custom archive restored into a fresh isolated database and independent proofs/retries passed.")
PY
