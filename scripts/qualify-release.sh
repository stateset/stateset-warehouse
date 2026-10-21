#!/usr/bin/env bash
# One local/CI software release gate. All inventory and HTTP fixtures are synthetic.
set -euo pipefail
set +x
umask 077
exec 3>&1 4>&2
cd "$(dirname "$0")/.."
wms_release_repository="$PWD"
source scripts/qualification_resources.sh
wms_qualification_resource_mode

if ! command -v python3 >/dev/null 2>&1; then
  echo 'Release qualification requires Python 3.' >&2
  exit 1
fi
wms_release_root="${WMS_RELEASE_ARTIFACT_DIR:-$(mktemp -d /tmp/stateset-wms-release.XXXXXXXX)}"
mkdir -p "$wms_release_root"
if [[ -n "$(ls -A "$wms_release_root")" ]]; then
  echo 'WMS_RELEASE_ARTIFACT_DIR must be empty; existing evidence will not be overwritten.' >&2
  exit 1
fi
wms_release_root="$(cd "$wms_release_root" && pwd)"
mkdir "$wms_release_root/raw" "$wms_release_root/publish"
export WMS_RELEASE_ARTIFACT_DIR="$wms_release_root"
export WMS_RELEASE_REVISION="${WMS_RELEASE_REVISION:-unrecorded-working-tree}"
export PYTHONDONTWRITEBYTECODE=1
wms_release_stage=''
wms_release_runtime=''
wms_release_api_pid=''
wms_release_scanner_pid=''
wms_release_started="$(date +%s)"

wms_release_event() {
  python3 - "$wms_release_root/events.jsonl" "$1" "$2" "${3:-0}" <<'PY'
from datetime import datetime, timezone
import json, pathlib, sys, time
with pathlib.Path(sys.argv[1]).open("a") as stream:
    stream.write(json.dumps({"stage": sys.argv[2], "status": sys.argv[3],
                             "exit_code": int(sys.argv[4]), "unix_seconds": time.time(),
                             "recorded_at": datetime.now(timezone.utc).isoformat()}) + "\n")
PY
}

wms_release_stop_apis() {
  local wms_pid wms_pending=0
  for wms_pid in "$wms_release_api_pid" "$wms_release_scanner_pid"; do
    if [[ -n "$wms_pid" ]] && kill -0 "$wms_pid" 2>/dev/null; then
      kill -TERM "$wms_pid" 2>/dev/null || true
    fi
  done
  for ((wms_wait=0;wms_wait<20;wms_wait++)); do
    wms_pending=0
    for wms_pid in "$wms_release_api_pid" "$wms_release_scanner_pid"; do
      if [[ -n "$wms_pid" ]] && kill -0 "$wms_pid" 2>/dev/null; then wms_pending=1; fi
    done
    [[ "$wms_pending" == 0 ]] && break
    sleep 1
  done
  for wms_pid in "$wms_release_api_pid" "$wms_release_scanner_pid"; do
    if [[ -n "$wms_pid" ]]; then
      if kill -0 "$wms_pid" 2>/dev/null; then
        kill -KILL "$wms_pid" 2>/dev/null || true
        wms_pending=1
      fi
      wait "$wms_pid" 2>/dev/null || true
    fi
  done
  wms_release_api_pid=''
  wms_release_scanner_pid=''
  return "$wms_pending"
}

wms_release_finalize() {
  local wms_result=$?
  trap - EXIT INT TERM
  set +e
  exec 1>&3 2>&4
  wms_release_stop_apis || wms_result=1
  if [[ -n "$wms_release_runtime" && "$wms_release_runtime" == /tmp/stateset-wms-release-runtime.* ]]; then
    rm -rf -- "$wms_release_runtime"
  fi
  if [[ -n "$wms_release_stage" ]]; then
    [[ "$wms_result" == 0 ]] && wms_result=1
    wms_release_event "$wms_release_stage" failed "$wms_result"
  fi
  if ! WMS_RELEASE_RESULT="$wms_result" WMS_RELEASE_STARTED="$wms_release_started" python3 - "$wms_release_root" <<'PY'
from datetime import datetime, timezone
import hashlib, json, os, pathlib, re, sys, time
root = pathlib.Path(sys.argv[1])
events = [json.loads(line) for line in (root / "events.jsonl").read_text().splitlines()] if (root / "events.jsonl").exists() else []
names = ["prerequisites", "software_checks", "observability_rules", "release_builds", "standard_workload", "archive_restore",
         "live_browsers", "production_container", "image_resilience", "stable_source_inputs"]
stages = []
for name in names:
    observed = [event for event in events if event["stage"] == name]
    stage = {"name": name, "status": observed[-1]["status"] if observed else "not_run"}
    if observed:
        stage.update(exit_code=observed[-1]["exit_code"], elapsed_seconds=round(observed[-1]["unix_seconds"]-observed[0]["unix_seconds"], 3))
    stages.append(stage)
passed = os.environ["WMS_RELEASE_RESULT"] == "0" and all(stage["status"] == "passed" for stage in stages)
artifacts = []
reports = {"standard_workload": root / "workload.json", "archive_restore": root / "restore/release-qualification.json",
           "observability_rules": root / "observability.json", "production_container": root / "container/container-qualification.json",
           "image_resilience": root / "resilience/resilience.json",
           "image_serial_resilience": root / "serial-resilience/resilience.json"}
for name, path in reports.items():
    if path.is_file():
        # Only the known sanitized report crosses into publish; inspect files,
        # generated credentials, database dumps and browser traces stay private.
        raw = path.read_bytes()
        output = root / "publish" / (name + ".json")
        output.write_bytes(raw)
        artifacts.append({"stage": name, "path": output.name, "sha256": hashlib.sha256(raw).hexdigest()})
for name in ("source-inputs-before.json", "source-inputs-after.json", "tool-versions.json", "provenance.json", "dependency-licenses.json", "host-preflight.json"):
    path = root / name
    if path.is_file():
        raw = path.read_bytes()
        (root / "publish" / name).write_bytes(raw)
        artifacts.append({"path": name, "sha256": hashlib.sha256(raw).hexdigest()})
# Redact the current environment's credential-like values and all PostgreSQL
# URLs from stage transcripts. Never publish raw environment/inspect files.
secrets = set()
for name, value in os.environ.items():
    if re.search(r"DATABASE_URL|ADMIN_URL|TOKEN|SECRET|PASSWORD|API_KEY|CREDENTIAL|INTEGRATIONS_JSON", name) and len(value) >= 8:
        secrets.add(value)
        try:
            parsed = json.loads(value)
            def collect(item):
                if isinstance(item, str) and len(item) >= 8: secrets.add(item)
                elif isinstance(item, dict):
                    for key, nested in item.items():
                        if len(key) >= 16: secrets.add(key)
                        collect(nested)
                elif isinstance(item, list):
                    for nested in item: collect(nested)
            collect(parsed)
        except (ValueError, TypeError):
            pass
for path in sorted((root / "raw").glob("*.log")):
    output = root / "publish" / path.name
    with path.open(errors="replace") as source, output.open("w") as target:
        for line in source:
            line = re.sub(r"postgres(?:ql)?://[^\s\"']+", "[REDACTED_POSTGRES_URL]", line)
            for secret in sorted(secrets, key=len, reverse=True):
                line = line.replace(secret, "[REDACTED]")
            target.write(line)
    artifacts.append({"path": output.name, "sha256": hashlib.sha256(output.read_bytes()).hexdigest()})
report = {"schema_version": 1, "scope": "software_release_gate", "recorded_at": datetime.now(timezone.utc).isoformat(),
          "result": "passed" if passed else "failed", "production_qualified": False, "synthetic": True,
          "release_revision": os.environ.get("WMS_RELEASE_REVISION", "unrecorded-working-tree"),
          "elapsed_seconds": int(time.time()) - int(os.environ["WMS_RELEASE_STARTED"]),
          "stages": stages, "artifacts": artifacts,
          "not_performed": ["Deployed WorkOS sign-in and membership lifecycle", "Deployed OMS/Freight qualification", "Physical warehouse and scanner acceptance", "Organization-hosted GitHub execution"],
          "limits": ["All inventory and native-service fixtures are synthetic", "Performance results apply to the recorded profiles and host",
                     "Process/database restart is not managed database failover", "Published logs are sanitized; private raw artifacts stay in the fresh evidence directory"]}
serialized = json.dumps(report, indent=2) + "\n"
(root / "summary.json").write_text(serialized)
(root / "publish/summary.json").write_text(serialized)
print("Software release gate: " + report["result"])
raise SystemExit(0 if passed else 1)
PY
  then
    wms_result=1
  fi
  printf 'Release evidence: %s\nPublishable evidence: %s/publish\n' "$wms_release_root" "$wms_release_root"
  exit "$wms_result"
}
trap wms_release_finalize EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

wms_release_run() {
  wms_release_stage="$1"
  shift
  printf 'Qualifying %s\n' "$wms_release_stage"
  wms_release_event "$wms_release_stage" running
  # This is deliberately not placed inside an if/! condition: Bash must keep
  # errexit enabled inside stage functions and stop on the first failed check.
  "$@" > "$wms_release_root/raw/$wms_release_stage.log" 2>&1
  wms_release_event "$wms_release_stage" passed
  wms_release_stage=''
}

wms_release_fingerprint() {
  python3 - "$1" <<'PY'
import hashlib, json, pathlib, sys
root = pathlib.Path.cwd()
files = [root / path for path in ("Cargo.toml", "Cargo.lock", "Dockerfile", ".dockerignore", "docker-compose.yml", ".nvmrc", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "SECURITY.md", "CONTRIBUTING.md", "THIRD_PARTY_NOTICES.md")]
excluded = {"node_modules", "dist", "test-results", "playwright-report", "__pycache__"}
for name in ("src", "migrations", "dashboard", "vendor", "cli", "examples", "tests", "scripts", "integrations", "deploy", ".github"):
    files += [path for path in (root / name).rglob("*") if path.is_file() and not excluded.intersection(path.parts)
              and not path.name.startswith(".env") and not path.name.endswith((".log", ".pyc"))]
inputs = [{"path": str(path.relative_to(root)), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()} for path in sorted(set(files))]
digest = hashlib.sha256(json.dumps(inputs, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
pathlib.Path(sys.argv[1]).write_text(json.dumps({"sha256": digest, "inputs": inputs}, indent=2) + "\n")
print(digest)
PY
}

wms_release_prerequisites() {
  python3 scripts/qualification_host.py --output "$wms_release_root/host-preflight.json" \
    --path "workspace=$PWD" --path "artifacts=$wms_release_root" --path temporary=/tmp
  : "${WMS_TEST_DATABASE_URL:?Set WMS_TEST_DATABASE_URL to an isolated PostgreSQL database whose name contains test}"
  wms_qualification_validate_network_subnets "${WMS_RELEASE_CONTAINER_SUBNET:-}" \
    "${WMS_RELEASE_UNTRACKED_SUBNET:-}" "${WMS_RELEASE_SERIAL_SUBNET:-}"
  for wms_tool in cargo rustc node npm docker curl openssl; do
    command -v "$wms_tool" >/dev/null 2>&1 || { echo "Missing required tool: $wms_tool" >&2; return 1; }
  done
  if [[ -z "${WMS_PG_BIN:-}" ]]; then
    WMS_PG_BIN="$(pg_config --bindir)"
  fi
  export WMS_PG_BIN
  for wms_tool in psql pg_dump pg_restore; do
    [[ -x "$WMS_PG_BIN/$wms_tool" ]] || { echo "Missing matching PostgreSQL tool: $wms_tool" >&2; return 1; }
  done
  # Restore uses the same explicitly selected test server by default. The
  # original CI stage omitted its required administrator URL.
  export WMS_RESTORE_ADMIN_URL="${WMS_RESTORE_ADMIN_URL:-$WMS_TEST_DATABASE_URL}"
  python3 - <<'PY'
import os, re, urllib.parse
for name in ("WMS_TEST_DATABASE_URL", "WMS_RESTORE_ADMIN_URL"):
    url = urllib.parse.urlsplit(os.environ[name])
    database = urllib.parse.unquote(url.path.lstrip("/"))
    if url.scheme not in ("postgres", "postgresql") or not re.fullmatch(r"[A-Za-z0-9_]{1,63}", database) or "test" not in database.lower():
        raise SystemExit("Both qualification URLs must explicitly name isolated PostgreSQL test databases")
    if {key.lower() for key, _ in urllib.parse.parse_qsl(url.query)} & {"dbname", "database", "service", "servicefile"}:
        raise SystemExit("Qualification URLs cannot override their database through query/service options")
for name in ("WMS_RELEASE_DEMO_PORT", "WMS_RELEASE_SCANNER_PORT"):
    value = os.environ[name]
    if not value.isdigit() or not 1024 <= int(value) <= 65535:
        raise SystemExit("Qualification API ports must be unprivileged numeric loopback ports")
if os.environ["WMS_RELEASE_DEMO_PORT"] == os.environ["WMS_RELEASE_SCANNER_PORT"]:
    raise SystemExit("Qualification APIs require distinct loopback ports")
serial_units = os.environ.get("WMS_RELEASE_SERIAL_UNITS", "16")
if not serial_units.isdigit() or not 2 <= int(serial_units) <= 256:
    raise SystemExit("WMS_RELEASE_SERIAL_UNITS must be between 2 and 256")
if os.environ.get("WMS_RESILIENCE_TRAFFIC_MODEL", "independent-operators") != "independent-operators":
    raise SystemExit("The complete release gate requires independent operators; use the standalone rehearsal for shared-snapshot comparisons")
PY
  # Verify the actual connection and CREATEDB permission without printing URLs.
  for wms_connection_name in WMS_TEST_DATABASE_URL WMS_RESTORE_ADMIN_URL; do
    "$WMS_PG_BIN/psql" --dbname "${!wms_connection_name}" -X --no-password --tuples-only --no-align \
      --command "SELECT CASE WHEN current_database() ILIKE '%test%' AND (rolsuper OR rolcreatedb) THEN 'isolated_test_with_createdb' ELSE 'invalid' END FROM pg_roles WHERE rolname=current_user" \
      | python3 -c 'import sys; assert sys.stdin.read().strip() == "isolated_test_with_createdb", "Actual test database and CREATEDB permission required"'
  done
  docker info --format '{{.ServerVersion}}' >/dev/null
  python3 - <<'PY'
import json, os, pathlib, subprocess
metadata = json.loads(subprocess.run(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
                                     check=True, capture_output=True, text=True, timeout=30).stdout)
if pathlib.Path(metadata["target_directory"]).resolve() != (pathlib.Path.cwd() / "target").resolve():
    raise SystemExit("Release qualification requires Cargo target_directory to be the repository target directory")
commands = {"rustc": ["rustc", "--version"], "cargo": ["cargo", "--version"], "node": ["node", "--version"],
            "npm": ["npm", "--version"], "python": ["python3", "--version"],
            "postgres_client": [str(pathlib.Path(os.environ["WMS_PG_BIN"]) / "pg_dump"), "--version"],
            "docker_server": ["docker", "version", "--format", "{{.Server.Version}}"]}
versions = {name: subprocess.run(command, check=True, capture_output=True, text=True, timeout=15).stdout.strip() for name, command in commands.items()}
(pathlib.Path(os.environ["WMS_RELEASE_ARTIFACT_DIR"]) / "tool-versions.json").write_text(json.dumps(versions, indent=2) + "\n")
PY
  wms_release_source_before="$(wms_release_fingerprint "$wms_release_root/source-inputs-before.json")"
  python3 scripts/open_source_preflight.py --output "$wms_release_root/dependency-licenses.json"
}

wms_release_builds() {
  cargo build --locked --bin stateset-warehouse --example restore_probe
  cargo build --locked --release --example workload
}

wms_release_observability() {
  bash deploy/monitoring/check.sh
  python3 - "$wms_release_root/observability.json" <<'PY'
from datetime import datetime, timezone
import hashlib, json, os, pathlib, sys
paths = ["deploy/monitoring/check.sh", "deploy/monitoring/alerts.yml", "deploy/monitoring/alerts.test.yml"]
report = {"schema_version": 1, "ok": True, "synthetic": True, "recorded_at": datetime.now(timezone.utc).isoformat(),
          "release_revision": os.environ["WMS_RELEASE_REVISION"], "check": "Official pinned promtool syntax and behavior tests",
          "source_sha256": {path: hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest() for path in paths},
          "limit": "Example alert policies; facility alert thresholds require operator review"}
pathlib.Path(sys.argv[1]).write_text(json.dumps(report, indent=2) + "\n")
PY
}

wms_release_workload() {
  WMS_WORKLOAD_REPORT="$wms_release_root/workload.json" \
    WMS_WORKLOAD_PRODUCTS="${WMS_RELEASE_WORKLOAD_PRODUCTS:-100}" \
    WMS_WORKLOAD_BINS="${WMS_RELEASE_WORKLOAD_BINS:-20}" \
    WMS_WORKLOAD_LOTS="${WMS_RELEASE_WORKLOAD_LOTS:-2}" \
    WMS_WORKLOAD_OPEN_ORDERS="${WMS_RELEASE_WORKLOAD_OPEN_ORDERS:-100}" \
    WMS_WORKLOAD_CLIENTS="${WMS_RELEASE_WORKLOAD_CLIENTS:-4}" \
    WMS_WORKLOAD_ACTIONS="${WMS_RELEASE_WORKLOAD_ACTIONS:-10}" \
    ./target/release/examples/workload
}

wms_release_wait_api() {
  local wms_base="$1" wms_pid="$2"
  for ((wms_try=0;wms_try<60;wms_try++)); do
    kill -0 "$wms_pid" 2>/dev/null || { echo 'Owned qualification API exited before readiness.' >&2; return 1; }
    if curl --max-time 2 --silent --fail --output /dev/null "$wms_base/ready"; then return 0; fi
    sleep 1
  done
  echo 'Owned qualification API did not become ready.' >&2
  return 1
}

wms_release_live_browsers() {
  python3 - <<'PY'
import os, socket
for name in ("WMS_RELEASE_DEMO_PORT", "WMS_RELEASE_SCANNER_PORT"):
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", int(os.environ[name])))
PY
  wms_release_runtime="$(mktemp -d /tmp/stateset-wms-release-runtime.XXXXXXXX)"
  python3 - "$wms_release_runtime" <<'PY'
import pathlib, sys
directory = pathlib.Path(sys.argv[1]).resolve()
if any((parent / ".env").exists() for parent in (directory,) + tuple(directory.parents)):
    raise SystemExit("Live qualification runtime must not inherit an ambient .env file")
PY
  ln -s "$wms_release_repository/dashboard" "$wms_release_runtime/dashboard"
  WMS_SCANNER_OPERATOR_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
  WMS_SCANNER_COUNTER_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
  export WMS_SCANNER_OPERATOR_TOKEN WMS_SCANNER_COUNTER_TOKEN
  wms_release_scanner_keys="$(python3 - <<'PY'
import json, os
print(json.dumps({os.environ["WMS_SCANNER_OPERATOR_TOKEN"]: {"tenant":"scanner-qualification","actor":"qualification:operator","role":"admin"},
                  os.environ["WMS_SCANNER_COUNTER_TOKEN"]: {"tenant":"scanner-qualification","actor":"qualification:counter","role":"admin"}}))
PY
)"
  (
    cd "$wms_release_runtime"
    unset WMS_WORKOS_JSON WMS_OMS_WEBHOOK_URL WMS_OMS_WEBHOOK_TOKEN WMS_TMS_WEBHOOK_URL WMS_TMS_WEBHOOK_TOKEN WMS_WEBHOOK_TENANT
    # Each owned API intentionally receives its own isolated process environment.
    # shellcheck disable=SC2030
    export DATABASE_URL="$WMS_TEST_DATABASE_URL" WMS_DEMO=true WMS_BIND="127.0.0.1:$WMS_RELEASE_DEMO_PORT" WMS_NATIVE_INTEGRATIONS_JSON='{}'
    exec "$wms_release_repository/target/debug/stateset-warehouse"
  ) > "$wms_release_root/raw/demo-api.log" 2>&1 &
  wms_release_api_pid=$!
  (
    cd "$wms_release_runtime"
    unset WMS_WORKOS_JSON WMS_OMS_WEBHOOK_URL WMS_OMS_WEBHOOK_TOKEN WMS_TMS_WEBHOOK_URL WMS_TMS_WEBHOOK_TOKEN WMS_WEBHOOK_TENANT
    # The demo process's environment must not propagate into the real scanner API.
    # shellcheck disable=SC2031
    export DATABASE_URL="$WMS_TEST_DATABASE_URL" WMS_DEMO=false WMS_BIND="127.0.0.1:$WMS_RELEASE_SCANNER_PORT" WMS_NATIVE_INTEGRATIONS_JSON='{}' WMS_API_KEYS_JSON="$wms_release_scanner_keys"
    exec "$wms_release_repository/target/debug/stateset-warehouse"
  ) > "$wms_release_root/raw/scanner-api.log" 2>&1 &
  wms_release_scanner_pid=$!
  wms_release_wait_api "http://127.0.0.1:$WMS_RELEASE_DEMO_PORT" "$wms_release_api_pid"
  wms_release_wait_api "http://127.0.0.1:$WMS_RELEASE_SCANNER_PORT" "$wms_release_scanner_pid"
  (
    cd dashboard
    WMS_BROWSER_URL="http://127.0.0.1:$WMS_RELEASE_DEMO_PORT" \
      WMS_SCANNER_BROWSER_URL="http://127.0.0.1:$WMS_RELEASE_SCANNER_PORT" \
      npx playwright test --project=live-postgres --output "$wms_release_root/live-browser-results"
  )
  wms_release_stop_apis
}

wms_release_restore() {
  WMS_RESTORE_PROBE="$wms_release_repository/target/debug/examples/restore_probe" \
    WMS_RESTORE_ARTIFACT_DIR="$wms_release_root/restore" bash scripts/restore-rehearsal.sh
}

wms_release_container() {
  WMS_QUALIFICATION_SUBNET="${WMS_RELEASE_CONTAINER_SUBNET:-}" \
    WMS_CONTAINER_ARTIFACT_DIR="$wms_release_root/container" bash scripts/container-smoke.sh
}

wms_release_resilience() {
  WMS_RESILIENCE_IMAGE="$(cat "$wms_release_root/container/image.id")" \
    WMS_QUALIFICATION_SUBNET="${WMS_RELEASE_UNTRACKED_SUBNET:-}" \
    WMS_RESILIENCE_SERIAL_UNITS=0 \
    WMS_RESILIENCE_ARTIFACT_DIR="$wms_release_root/resilience" bash scripts/resilience-rehearsal.sh
  WMS_RESILIENCE_IMAGE="$(cat "$wms_release_root/container/image.id")" \
    WMS_QUALIFICATION_SUBNET="${WMS_RELEASE_SERIAL_SUBNET:-}" \
    WMS_RESILIENCE_SERIAL_UNITS="${WMS_RELEASE_SERIAL_UNITS:-16}" \
    WMS_RESILIENCE_ARTIFACT_DIR="$wms_release_root/serial-resilience" bash scripts/resilience-rehearsal.sh
}

wms_release_stable_inputs() {
  local wms_release_source_after
  wms_release_source_after="$(wms_release_fingerprint "$wms_release_root/source-inputs-after.json")"
  if [[ "$wms_release_source_before" != "$wms_release_source_after" ]]; then
    echo 'Software inputs changed while qualifying; rerun the complete gate on frozen inputs.' >&2
    return 1
  fi
  python3 - "$wms_release_root" <<'PY'
import hashlib, json, pathlib, sys
sys.path.insert(0, "scripts")
from crash_journal import verify_journal
root = pathlib.Path(sys.argv[1])
container = json.loads((root / "container/container-qualification.json").read_text())
resilience = json.loads((root / "resilience/resilience.json").read_text())
serial = json.loads((root / "serial-resilience/resilience.json").read_text())
restore = json.loads((root / "restore/release-qualification.json").read_text())
workload = json.loads((root / "workload.json").read_text())
assert container["ok"] is True and resilience["ok"] is True and restore["ok"] is True
assert serial["ok"] is True and serial["independent_proof"]["serial_identity_proof"]["ok"] is True
assert serial["phases"]["sustained_http"]["serial_units_per_bin_lot"] >= 2
assert serial["phases"]["sustained_http"]["serial_current_source_reads"] == serial["phases"]["sustained_http"]["committed_moves"]
assert serial["independent_proof"]["serial_identity_proof"]["recovered_moves_outside_timed_interval"] == 2
for folder, report in (("resilience", resilience), ("serial-resilience", serial)):
    for phase in ("api_sigkill", "postgres_sigkill"):
        evidence = report["phases"][phase]
        checkpoints = verify_journal(root / folder, evidence["crash_journal"], phase)
        original = checkpoints[0]["facts"]
        if folder == "serial-resilience":
            barrier = evidence["serial_after_write_barrier"]
            assert original["original_request"] == barrier["original_request"]
            assert original["actor"] == barrier["actor"]
            facts = {record["stage"]: record["facts"] for record in checkpoints}
            stages = [record["stage"] for record in checkpoints]
            assert stages.index("barrier_planned") < stages.index("barrier_installed") < stages.index("original_send_requested")
            assert stages.index("fault_verified") < stages.index("barrier_removed") < stages.index("original_recovery_requested")
            assert stages.index("rollback_verified") < stages.index("original_recovery_requested")
            assert facts["baseline_captured"]["fingerprints"] == evidence["all_table_fingerprints_unchanged"]
            assert facts["barrier_planned"]["baseline"] == barrier["before"]
            assert facts["writer_blocked"]["evidence"] == barrier["blocking_evidence"]
for phase in ("api_sigkill", "postgres_sigkill"):
    evidence = serial["phases"][phase]
    assert evidence["barrier_stage"] == "after_serial_custody_history_event_receipt_before_outbox_commit"
    assert evidence["serial_after_write_barrier"]["owned_fault_trigger_removed"] is True
    assert evidence["serial_after_write_barrier"]["blocking_evidence"]["wait_event"] == "advisory"
    assert len(evidence["all_table_fingerprints_unchanged"]) == 49
assert workload["configured_thresholds_passed"] is True and workload["proof"]["independent_reconciliation"] is True
assert container["image_id"] == resilience["image_id"] == (root / "container/image.id").read_text().strip()
assert container["source_inputs_sha256"] == resilience["image_source_inputs_sha256"], "Image source identities differ across proofs"
assert serial["image_id"] == container["image_id"] and serial["image_source_inputs_sha256"] == container["source_inputs_sha256"]
expected = {path.stem: hashlib.sha256(path.read_bytes()).hexdigest() for path in pathlib.Path("migrations").glob("[0-9][0-9][0-9]_*.sql")}
for evidence in (container["migrations"], resilience["migrations"], serial["migrations"], restore["verification"]["migrations"]):
    assert {row["name"]: row["sha256"] for row in evidence} == expected, "Proof did not use the exact tracked migration chain"
inputs = json.loads((root / "source-inputs-before.json").read_text())
report = {"schema_version": 1, "ok": True, "stable_software_inputs_sha256": inputs["sha256"],
          "image_id": container["image_id"], "image_source_inputs_sha256": container["source_inputs_sha256"],
          "restore_container_resilience_migrations_identical": True, "migrations": expected,
          "untracked_and_serial_image_resilience_passed": True,
          "limit": "The source fingerprint binds this working tree; it does not assert a clean Git checkout or deployed acceptance"}
(root / "provenance.json").write_text(json.dumps(report, indent=2) + "\n")
PY
}

export WMS_RELEASE_DEMO_PORT="${WMS_RELEASE_DEMO_PORT:-8088}"
export WMS_RELEASE_SCANNER_PORT="${WMS_RELEASE_SCANNER_PORT:-8089}"
wms_release_run prerequisites wms_release_prerequisites
wms_release_run software_checks bash scripts/check.sh --postgres
wms_release_run observability_rules wms_release_observability
wms_release_run release_builds wms_release_builds
wms_release_run standard_workload wms_release_workload
wms_release_run archive_restore wms_release_restore
wms_release_run live_browsers wms_release_live_browsers
wms_release_run production_container wms_release_container
wms_release_run image_resilience wms_release_resilience
wms_release_run stable_source_inputs wms_release_stable_inputs
