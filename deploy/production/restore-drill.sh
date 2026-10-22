#!/usr/bin/env bash
# Prove the latest backup restores: restore it into a scratch database on a
# server where WMS_DRILL_ADMIN_URL may create databases, start the release
# image against it, and require /ready plus a passing full reconciliation
# for every warehouse. The scratch database is dropped afterwards.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "$0")/lib.sh"
WMS_DRILL_ADMIN_URL="$(env_value /etc/stateset-wms/drill.env WMS_DRILL_ADMIN_URL)"
WMS_DRILL_ADMIN_KEY="$(env_value /etc/stateset-wms/drill.env WMS_DRILL_ADMIN_KEY)"
dir="${WMS_BACKUP_DIR:-/var/backups/stateset-wms}"
image="${WMS_PG_CLIENT_IMAGE:-postgres:16-alpine}"
release="$(cat /var/lib/stateset-wms/current-release)"
latest="$(ls -1t "$dir"/stateset-wms-*.dump | head -1)"
(cd "$dir" && sha256sum --check --quiet "$(basename "$latest").sha256")
scratch="wms_restore_drill_$(date -u +%Y%m%d%H%M%S)"
# Swap only the database name; keep any ?sslmode=... query.
base="${WMS_DRILL_ADMIN_URL%%\?*}"
query="${WMS_DRILL_ADMIN_URL#"$base"}"
target="${base%/*}/$scratch$query"
cleanup() {
  docker rm -f stateset-wms-drill >/dev/null 2>&1 || true
  docker run --rm --network host "$image" psql "$WMS_DRILL_ADMIN_URL" -qc "DROP DATABASE IF EXISTS $scratch" || true
}
trap cleanup EXIT
docker run --rm --network host "$image" psql "$WMS_DRILL_ADMIN_URL" -qc "CREATE DATABASE $scratch"
docker run --rm --network host -v "$dir:/backup:ro" "$image" \
  pg_restore --no-owner --single-transaction --exit-on-error --dbname="$target" "/backup/$(basename "$latest")"
docker run -d --name stateset-wms-drill --network host --read-only --tmpfs /tmp \
  --env-file /etc/stateset-wms/app.env -e DATABASE_URL="$target" -e WMS_BIND=127.0.0.1:18088 -e WMS_DEMO=false \
  "stateset-warehouse:$release" > /dev/null
for _ in $(seq 1 60); do curl --silent --fail http://127.0.0.1:18088/ready > /dev/null && break; sleep 2; done
curl --silent --fail http://127.0.0.1:18088/ready > /dev/null
WMS_DRILL_ADMIN_KEY="$WMS_DRILL_ADMIN_KEY" python3 - <<'PY'
import json, os, urllib.parse, urllib.request
base = "http://127.0.0.1:18088"
headers = {"Authorization": f"Bearer {os.environ['WMS_DRILL_ADMIN_KEY']}"}
def get(path):
    with urllib.request.urlopen(urllib.request.Request(base + path, headers=headers), timeout=60) as r:
        return json.load(r)
after, checked = "", 0
while True:
    page = get("/v1/warehouses?limit=100" + (f"&after={urllib.parse.quote(after)}" if after else ""))
    for item in page["warehouses"]:
        warehouse = item["warehouse"]["id"]
        report = get(f"/v1/warehouses/{urllib.parse.quote(warehouse, safe='')}/reconciliation")
        if report.get("ok") is not True:
            raise SystemExit(f"Restore drill FAILED: reconciliation of {warehouse} is not ok")
        print(f"Restored {warehouse} reconciles")
        checked += 1
    after = page.get("next_after")
    if not after:
        break
if checked == 0:
    raise SystemExit("Restore drill FAILED: the admin key sees no warehouses")
PY
echo "Restore drill passed for $(basename "$latest") with release $release"
