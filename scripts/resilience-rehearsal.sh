#!/usr/bin/env bash
# Local synthetic crash/recovery qualification. Never connects to external data.
set -euo pipefail
umask 077
cd "$(dirname "$0")/.."
source scripts/qualification_resources.sh
wms_qualification_resource_mode
wms_image="${WMS_RESILIENCE_IMAGE:-}"
case "${WMS_RESILIENCE_TRAFFIC_MODEL:-independent-operators}" in
  shared-snapshot|independent-operators) ;;
  *) echo 'WMS_RESILIENCE_TRAFFIC_MODEL must be shared-snapshot or independent-operators.' >&2; exit 1 ;;
esac
if [[ ! "$wms_image" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  echo 'Set WMS_RESILIENCE_IMAGE to the immutable sha256 ID of a built production image.' >&2
  exit 1
fi
wms_root="${WMS_RESILIENCE_ARTIFACT_DIR:-$(mktemp -d /tmp/stateset-wms-resilience.XXXXXXXX)}"
mkdir -p "$wms_root"
if [[ -n "$(ls -A "$wms_root")" ]]; then
  echo 'WMS_RESILIENCE_ARTIFACT_DIR must be empty; existing evidence is never overwritten.' >&2
  exit 1
fi
wms_root="$(cd "$wms_root" && pwd)"
wms_run="$(python3 -c 'import secrets,time; print(str(int(time.time()))+"-"+secrets.token_hex(4))')"
wms_label=org.stateset.wms.resilience
wms_network="wms-resilience-$wms_run"
wms_volume="wms-resilience-data-$wms_run"
wms_pg="wms-resilience-pg-$wms_run"
wms_api_a="wms-resilience-api-a-$wms_run"
wms_api_b="wms-resilience-api-b-$wms_run"
cleanup() {
  local result=$?
  trap - EXIT INT TERM
  wms_qualification_finish_resources "$wms_root" "$wms_label" "$wms_run" \
    "$wms_network" "$wms_volume" "$wms_api_a" "$wms_api_b" "$wms_pg" || result=1
  printf 'Resilience qualification evidence: %s\n' "$wms_root"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
docker image inspect "$wms_image" > "$wms_root/image-inspect.json"
docker network create --label "$wms_label=$wms_run" "$wms_network" > "$wms_root/network.id"
# A named volume deliberately survives the PostgreSQL container's SIGKILL/start.
# It is uniquely owned by this run; retention mode keeps it after qualification.
docker volume create --label "$wms_label=$wms_run" "$wms_volume" > "$wms_root/volume.id"
python3 - "$wms_root/credentials.json" <<'PY'
import json, os, pathlib, secrets, sys
clients = int(os.environ.get("WMS_RESILIENCE_CLIENTS", "4"))
if not 2 <= clients <= 16:
    raise SystemExit("WMS_RESILIENCE_CLIENTS must be between 2 and 16")
identities = [{"tenant": "resilience-test", "actor": "qualification:admin", "role": "admin"}]
identities += [{"tenant": "resilience-test", "actor": f"operator:resilience-{i}", "role": "operator"} for i in range(clients)]
identities += [{"tenant": "resilience-outsider", "actor": "qualification:outsider", "role": "admin"}]
pathlib.Path(sys.argv[1]).write_text(json.dumps({secrets.token_urlsafe(32): identity for identity in identities}))
PY
docker run --detach --name "$wms_pg" --network "$wms_network" \
  --label "$wms_label=$wms_run" --mount "type=volume,source=$wms_volume,target=/var/lib/postgresql/data" \
  --env POSTGRES_USER=warehouse --env POSTGRES_PASSWORD=warehouse-resilience-disposable-only \
  --env POSTGRES_DB=warehouse_resilience_test \
  --health-cmd 'pg_isready -U warehouse -d warehouse_resilience_test' \
  --health-interval 1s --health-timeout 3s --health-retries 30 \
  "${WMS_RESILIENCE_POSTGRES_IMAGE:-postgres:16-alpine}" > "$wms_root/postgres.id"
for ((wms_attempt=0;wms_attempt<60;wms_attempt++)); do
  if [[ "$(docker inspect --format '{{.State.Health.Status}}' "$wms_pg")" == healthy ]]; then break; fi
  sleep 1
done
test "$(docker inspect --format '{{.State.Health.Status}}' "$wms_pg")" = healthy
for wms_api in "$wms_api_a" "$wms_api_b"; do
  docker run --detach --name "$wms_api" --network "$wms_network" \
    --label "$wms_label=$wms_run" --publish 127.0.0.1::8088 \
    --read-only --cap-drop=ALL --security-opt=no-new-privileges:true \
    --tmpfs /tmp:rw,nosuid,nodev,size=16m \
    --env "DATABASE_URL=postgres://warehouse:warehouse-resilience-disposable-only@$wms_pg:5432/warehouse_resilience_test" \
    --env WMS_DEMO=false --env WMS_BIND=0.0.0.0:8088 \
    --env "WMS_API_KEYS_JSON=$(cat "$wms_root/credentials.json")" \
    "$wms_image" >> "$wms_root/api-containers.ids"
done
python3 scripts/resilience_probe.py --artifacts "$wms_root" --run "$wms_run" \
  --postgres "$wms_pg" --api "$wms_api_a" --api "$wms_api_b" \
  --seconds "${WMS_RESILIENCE_SECONDS:-60}" \
  --review-mode "${WMS_RESILIENCE_REVIEW_MODE:-resources}" \
  --traffic-model "${WMS_RESILIENCE_TRAFFIC_MODEL:-independent-operators}" \
  --products "${WMS_RESILIENCE_PRODUCTS:-100}" \
  --open-orders "${WMS_RESILIENCE_OPEN_ORDERS:-100}" \
  --lots "${WMS_RESILIENCE_LOTS:-2}" --bins "${WMS_RESILIENCE_BINS:-20}" \
  --serial-units "${WMS_RESILIENCE_SERIAL_UNITS:-0}"
