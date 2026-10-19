#!/usr/bin/env bash
# Build and run the production image against a private, disposable PostgreSQL.
# All created containers/networks carry a unique ownership label for cleanup.
set -euo pipefail
umask 077
cd "$(dirname "$0")/.."
source scripts/qualification_resources.sh
wms_qualification_resource_mode
wms_container_root="${WMS_CONTAINER_ARTIFACT_DIR:-$(mktemp -d /tmp/stateset-wms-container.XXXXXXXX)}"
mkdir -p "$wms_container_root"
if [[ -n "$(ls -A "$wms_container_root")" ]]; then
  echo 'WMS_CONTAINER_ARTIFACT_DIR must be empty; existing evidence will not be overwritten.' >&2
  exit 1
fi
wms_container_root="$(cd "$wms_container_root" && pwd)"
wms_run="$(python3 -c 'import secrets,time; print(str(int(time.time()))+"-"+secrets.token_hex(4))')"
wms_network="wms-qualification-$wms_run"
wms_postgres="wms-qualification-pg-$wms_run"
wms_api="wms-qualification-api-$wms_run"
wms_volume=''
wms_image="${WMS_QUALIFICATION_IMAGE:-stateset-warehouse:qualification}"
wms_pg_image="${WMS_QUALIFICATION_POSTGRES_IMAGE:-postgres:16-alpine}"
wms_label=org.stateset.wms.qualification
cleanup() {
  local result=$?
  trap - EXIT INT TERM
  wms_qualification_finish_resources "$wms_container_root" "$wms_label" "$wms_run" \
    "$wms_network" "$wms_volume" "$wms_api" "$wms_postgres" || result=1
  printf 'Container qualification evidence: %s\n' "$wms_container_root"
  if [[ "$result" != 0 ]]; then
    echo 'Container qualification failed; inspect the retained build/runtime evidence.' >&2
  fi
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
printf 'Building production image; evidence: %s\n' "$wms_container_root"

# Fingerprint the actual Rust/web/container build inputs independently of Git.
# A changing working tree cannot silently qualify a different application.
wms_source_fingerprint() {
  python3 - "$1" <<'PY'
import hashlib, json, pathlib, sys
root = pathlib.Path.cwd()
files = [root / p for p in ("Dockerfile", ".dockerignore", "docker-compose.yml", "Cargo.toml", "Cargo.lock")]
excluded = {"node_modules", "dist", "test-results", "playwright-report"}
for name in ("src", "migrations", "dashboard", "vendor"):
    files += [p for p in (root / name).rglob("*") if p.is_file() and not excluded.intersection(p.parts)
              and not p.name.startswith(".env") and not p.name.endswith(".log")]
inputs = [{"path": str(p.relative_to(root)), "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
          for p in sorted(set(files))]
digest = hashlib.sha256(json.dumps(inputs, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
pathlib.Path(sys.argv[1]).write_text(json.dumps({"sha256": digest, "inputs": inputs}, indent=2) + "\n")
print(digest)
PY
}
wms_source_before="$(wms_source_fingerprint "$wms_container_root/source-inputs.json")"
wms_revision="${WMS_RELEASE_REVISION:-unrecorded-working-tree}"
wms_started="$(date +%s)"
docker build --progress=plain --iidfile "$wms_container_root/image.id" \
  --label "org.opencontainers.image.revision=$wms_revision" \
  --label "org.stateset.wms.source-sha256=$wms_source_before" \
  --tag "$wms_image" . > "$wms_container_root/build.log" 2>&1
wms_source_after="$(wms_source_fingerprint "$wms_container_root/source-inputs-after.json")"
if [[ "$wms_source_before" != "$wms_source_after" ]]; then
  echo 'Application build inputs changed during the image build; rerun qualification on stable inputs.' >&2
  exit 1
fi
wms_image_id="$(cat "$wms_container_root/image.id")"
docker image inspect "$wms_image_id" > "$wms_container_root/image-inspect.json"
docker version --format '{{json .}}' > "$wms_container_root/docker-version.json"
docker network create --label "$wms_label=$wms_run" "$wms_network" > "$wms_container_root/network.id"
wms_pg_storage=(--tmpfs /var/lib/postgresql/data:rw,nosuid,nodev,size=512m)
if [[ "${WMS_KEEP_QUALIFICATION_RESOURCES:-0}" == 1 ]]; then
  wms_volume="wms-qualification-data-$wms_run"
  docker volume create --label "$wms_label=$wms_run" "$wms_volume" > "$wms_container_root/volume.id"
  wms_pg_storage=(--mount "type=volume,source=$wms_volume,target=/var/lib/postgresql/data")
fi
docker run --detach --name "$wms_postgres" --network "$wms_network" \
  --label "$wms_label=$wms_run" "${wms_pg_storage[@]}" \
  --env POSTGRES_USER=warehouse --env POSTGRES_PASSWORD=warehouse-qualification-only \
  --env POSTGRES_DB=warehouse_container_test \
  --health-cmd 'pg_isready -U warehouse -d warehouse_container_test' \
  --health-interval 2s --health-timeout 3s --health-retries 30 \
  "$wms_pg_image" > "$wms_container_root/postgres-container.id"
for ((wms_attempt=0;wms_attempt<60;wms_attempt++)); do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' "$wms_postgres")" == healthy ]]; then break; fi
  sleep 1
done
test "$(docker inspect --format '{{.State.Health.Status}}' "$wms_postgres")" = healthy
docker run --detach --name "$wms_api" --network "$wms_network" \
  --label "$wms_label=$wms_run" --publish 127.0.0.1::8088 \
  --read-only --cap-drop=ALL --security-opt=no-new-privileges:true \
  --tmpfs /tmp:rw,nosuid,nodev,size=16m \
  --env "DATABASE_URL=postgres://warehouse:warehouse-qualification-only@$wms_postgres:5432/warehouse_container_test" \
  --env WMS_DEMO=false --env WMS_BIND=0.0.0.0:8088 \
  --env 'WMS_API_KEYS_JSON={"release-smoke-key-at-least-32-characters":{"tenant":"release-smoke","actor":"release:admin","role":"admin"}}' \
  "$wms_image_id" > "$wms_container_root/api-container.id"
for ((wms_attempt=0;wms_attempt<90;wms_attempt++)); do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' "$wms_api")" == healthy ]]; then break; fi
  sleep 1
done
test "$(docker inspect --format '{{.State.Health.Status}}' "$wms_api")" = healthy
docker inspect "$wms_api" > "$wms_container_root/container-inspect.json"
wms_pg_image_id="$(docker inspect --format '{{.Image}}' "$wms_postgres")"
docker image inspect "$wms_pg_image_id" > "$wms_container_root/postgres-image-inspect.json"
docker exec "$wms_api" cat /proc/1/status > "$wms_container_root/process-status.txt"
wms_binding="$(docker port "$wms_api" 8088/tcp)"
if [[ ! "$wms_binding" =~ ^127\.0\.0\.1:[0-9]+$ ]]; then
  echo 'Qualification API must publish only a random loopback port.' >&2
  exit 1
fi
wms_base="http://$wms_binding"
test "$(curl --silent --output /dev/null --write-out '%{http_code}' "$wms_base/v1/warehouses")" = 401
test "$(curl --silent --output /dev/null --write-out '%{http_code}' --header 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' "$wms_base/mcp")" = 401
for wms_mcp_method in session catalog; do
  if [[ "$wms_mcp_method" == session ]]; then
    wms_mcp_body='{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"wms_session","arguments":{}}}'
  else
    wms_mcp_body='{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  fi
  curl --fail --silent --show-error --header 'Authorization: Bearer release-smoke-key-at-least-32-characters' \
    --header 'Content-Type: application/json' --header 'Accept: application/json, text/event-stream' \
    --header 'MCP-Protocol-Version: 2025-11-25' --data "$wms_mcp_body" \
    "$wms_base/mcp" > "$wms_container_root/mcp-$wms_mcp_method.json"
done
curl --fail --silent --show-error "$wms_base/ready" > "$wms_container_root/readiness.json"
curl --fail --silent --show-error "$wms_base/" > "$wms_container_root/dashboard.html"
curl --fail --silent --show-error --header 'Authorization: Bearer release-smoke-key-at-least-32-characters' \
  "$wms_base/v1/session" > "$wms_container_root/session.json"
curl --fail --silent --show-error --header 'Authorization: Bearer release-smoke-key-at-least-32-characters' \
  "$wms_base/v1/warehouses/wh-main/reconciliation" > "$wms_container_root/reconciliation.json"
docker exec "$wms_postgres" psql -U warehouse -d warehouse_container_test -At \
  -c "SELECT jsonb_agg(jsonb_build_object('name',name,'sha256',digest) ORDER BY name) FROM wms_schema_migrations" \
  > "$wms_container_root/migrations.json"
docker exec "$wms_postgres" psql -U warehouse -d warehouse_container_test -At \
  -c 'SHOW server_version' > "$wms_container_root/postgres-version.txt"
docker stop --time 15 "$wms_api" > "$wms_container_root/shutdown.log"
docker inspect --format '{{json .State}}' "$wms_api" > "$wms_container_root/shutdown.json"
wms_finished="$(date +%s)"
WMS_CONTAINER_SECONDS="$((wms_finished-wms_started))" python3 - "$wms_container_root" <<'PY'
import hashlib, json, os, pathlib, re, sys
root = pathlib.Path(sys.argv[1])
image = json.loads((root / "image-inspect.json").read_text())[0]
container = json.loads((root / "container-inspect.json").read_text())[0]
postgres = json.loads((root / "postgres-image-inspect.json").read_text())[0]
inputs = json.loads((root / "source-inputs.json").read_text())
session = json.loads((root / "session.json").read_text())
mcp_session = json.loads((root / "mcp-session.json").read_text())["result"]["structuredContent"]
mcp_catalog = json.loads((root / "mcp-catalog.json").read_text())["result"]["tools"]
mcp_catalog_bytes = (root / "mcp-catalog.json").stat().st_size
proof = json.loads((root / "reconciliation.json").read_text())
process = dict(line.split(":", 1) for line in (root / "process-status.txt").read_text().splitlines() if ":" in line)
assert container["Config"]["User"] == "10001"
assert container["HostConfig"]["ReadonlyRootfs"] is True
assert "ALL" in container["HostConfig"]["CapDrop"]
assert "no-new-privileges:true" in container["HostConfig"]["SecurityOpt"]
assert container["State"]["Health"]["Status"] == "healthy"
assert process["NoNewPrivs"].strip() == "1" and int(process["CapEff"].strip(), 16) == 0
assert all(int(uid) == 10001 for uid in process["Uid"].split())
assert session["demo"] is False and session["identity"] == {"tenant":"release-smoke","actor":"release:admin","role":"admin"}
assert mcp_session["http_status"] == 200 and mcp_session["body"]["identity"] == session["identity"]
assert len(mcp_catalog) > 45 and any(t["name"] == "wms_pack_shipment" for t in mcp_catalog)
assert mcp_catalog_bytes < 256 * 1024, "Catalog must exclude unused command definitions"
assert proof["ok"] is True and proof["method"] == "full_append_only_ledger_and_order_line_allocations"
assert '<div id="root"></div>' in (root / "dashboard.html").read_text()
assert image["Id"] == (root / "image.id").read_text().strip()
assert container["Image"] == image["Id"], "Running container must use the fingerprinted immutable image"
assert image["Config"]["Labels"]["org.stateset.wms.source-sha256"] == inputs["sha256"]
shutdown = json.loads((root / "shutdown.json").read_text())
assert shutdown["Status"] == "exited" and shutdown["ExitCode"] == 0
migrations = json.loads((root / "migrations.json").read_text())
for migration in migrations:
    file = pathlib.Path("migrations") / (migration["name"] + ".sql")
    assert hashlib.sha256(file.read_bytes()).hexdigest() == migration["sha256"]
assert len(migrations) == len(list(pathlib.Path("migrations").glob("[0-9][0-9][0-9]_*.sql")))
report = {
    "ok": True, "synthetic": True, "image_id": image["Id"],
    "source_inputs_sha256": inputs["sha256"],
    "release_revision": image["Config"]["Labels"]["org.opencontainers.image.revision"],
    "os": image["Os"], "architecture": image["Architecture"],
    "docker_server_version": json.loads((root / "docker-version.json").read_text())["Server"]["Version"],
    "postgres_server_version": (root / "postgres-version.txt").read_text().strip(),
    "postgres_image_id": postgres["Id"], "postgres_repo_digests": postgres.get("RepoDigests", []),
    "qualifier_script_sha256": hashlib.sha256(pathlib.Path("scripts/container-smoke.sh").read_bytes()).hexdigest(),
    "resource_helper_sha256": hashlib.sha256(pathlib.Path("scripts/qualification_resources.sh").read_bytes()).hexdigest(),
    "elapsed_build_and_runtime_seconds": int(os.environ["WMS_CONTAINER_SECONDS"]),
    "runtime": {"uid": 10001, "read_only_rootfs": True, "effective_capabilities": 0,
                "no_new_privileges": True, "health": "healthy", "random_loopback_host_port": True,
                "unauthenticated_api_status": 401, "real_mode_authenticated_session": True,
                "mcp": {"unauthenticated_status": 401, "authenticated_session_matches_rest": True,
                        "catalog_tools": len(mcp_catalog), "catalog_bytes": mcp_catalog_bytes,
                        "partial_fulfillment_tools_present": True},
                "immutable_image_id_bound_to_running_container": True, "graceful_sigterm_exit_code": 0,
                "compiled_dashboard_served": True, "independent_reconciliation": proof},
    "migrations": migrations,
    "qualification_limit": "Production image build and isolated synthetic PostgreSQL runtime; no facility or deployed integration acceptance claimed",
}
(root / "container-qualification.json").write_text(json.dumps(report, indent=2) + "\n")
print("PASS: production image healthy under UID 10001, read-only filesystem, zero effective capabilities, no new privileges, real authentication and independent PostgreSQL reconciliation.")
PY
