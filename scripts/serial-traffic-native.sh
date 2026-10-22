#!/usr/bin/env bash
# Owned development processes only; retains every database and evidence file.
set -euo pipefail
umask 077
cd "$(dirname "$0")/.."
wms_serial_repo="$PWD"
: "${WMS_TEST_DATABASE_URL:?Supply an owned synthetic PostgreSQL test server}"
wms_serial_root="${WMS_SERIAL_TRAFFIC_ARTIFACT_DIR:-$(mktemp -d /tmp/wms-serial-traffic.XXXXXXXX)}"
mkdir -p "$wms_serial_root"
[[ -z "$(ls -A "$wms_serial_root")" ]] || { echo 'Evidence directory must be empty.' >&2; exit 1; }
wms_serial_root="$(cd "$wms_serial_root" && pwd)"
chmod 700 "$wms_serial_root"
wms_serial_pids=()
cleanup() {
 local result=$?
 trap - EXIT INT TERM
 for wms_serial_pid in "${wms_serial_pids[@]}"; do kill "$wms_serial_pid" 2>/dev/null || true; done
 for wms_serial_pid in "${wms_serial_pids[@]}"; do wait "$wms_serial_pid" 2>/dev/null || true; done
 echo "Stopped owned native APIs; retained serial traffic evidence at $wms_serial_root (exit $result)"
 exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
export WMS_SERIAL_TRAFFIC_ROOT="$wms_serial_root" WMS_SERIAL_TRAFFIC_REPO="$wms_serial_repo"
python3 - <<'PY'
import hashlib,json,os,pathlib,re,secrets,socket,urllib.parse
p=pathlib.Path(os.environ['WMS_SERIAL_TRAFFIC_ROOT']);repo=pathlib.Path(os.environ['WMS_SERIAL_TRAFFIC_REPO'])
u=urllib.parse.urlsplit(os.environ['WMS_TEST_DATABASE_URL']);q=dict(urllib.parse.parse_qsl(u.query))
assert u.scheme=='postgresql' and not u.netloc and 'test' in u.path
assert re.fullmatch(r'/tmp/stateset-wms-test\.[A-Za-z0-9]+/socket',q.get('host',''))
assert not any((x/'.env').exists() for x in (p,*p.parents))
clients=int(os.environ.get('WMS_SERIAL_TRAFFIC_CLIENTS','4'));assert 2<=clients<=16
identities=[{'tenant':'resilience-test','actor':'qualification:admin','role':'admin'}]
identities += [{'tenant':'resilience-test','actor':f'operator:serial-{i}','role':'operator'} for i in range(clients)]
identities += [{'tenant':'resilience-outsider','actor':'qualification:outsider','role':'admin'}]
(p/'credentials.json').write_text(json.dumps({secrets.token_urlsafe(32):v for v in identities}))
name='wms_serial_load_test_'+secrets.token_hex(6)
database=u.scheme+'://'+u.netloc+'/'+name+('?' +u.query if u.query else '')
sockets=[socket.socket(),socket.socket()]
for s in sockets:s.bind(('127.0.0.1',0))
ports=[s.getsockname()[1] for s in sockets]
for s in sockets:s.close()
binary=pathlib.Path(os.environ.get('WMS_SERIAL_TRAFFIC_BINARY',str(repo/'target/debug/stateset-warehouse'))).resolve()
assert binary.is_file() and os.access(binary,os.X_OK), 'Supply an executable native API binary'
(p/'ownership.json').write_text(json.dumps({'database':database,'database_name':name,'bases':[f'http://127.0.0.1:{n}' for n in ports],'binary':str(binary),'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest()},indent=2)+'\n')
(p/'database-name.txt').write_text(name)
PY
wms_serial_dbname="$(cat "$wms_serial_root/database-name.txt")"
wms_serial_binary="$(python3 -c 'import json,os;print(json.load(open(os.path.join(os.environ["WMS_SERIAL_TRAFFIC_ROOT"],"ownership.json")))["binary"])')"
psql "$WMS_TEST_DATABASE_URL" -XqAt -v ON_ERROR_STOP=1 -c "CREATE DATABASE $wms_serial_dbname"
ln -s "$wms_serial_repo/dashboard" "$wms_serial_root/dashboard"
for wms_serial_index in 0 1; do
 (
  cd "$wms_serial_root"
  unset WMS_WORKOS_JSON WMS_OMS_WEBHOOK_URL WMS_OMS_WEBHOOK_TOKEN WMS_TMS_WEBHOOK_URL WMS_TMS_WEBHOOK_TOKEN WMS_WEBHOOK_TENANT
  export DATABASE_URL WMS_BIND WMS_API_KEYS_JSON WMS_DEMO=false WMS_NATIVE_INTEGRATIONS_JSON='{}'
  DATABASE_URL="$(python3 -c 'import json;print(json.load(open("ownership.json"))["database"])')"
  WMS_BIND="$(python3 - "$wms_serial_index" <<'PY'
import json,sys,urllib.parse
print(urllib.parse.urlsplit(json.load(open('ownership.json'))['bases'][int(sys.argv[1])]).netloc)
PY
)"
  WMS_API_KEYS_JSON="$(cat credentials.json)"
  exec "$wms_serial_binary"
 ) > "$wms_serial_root/api-$wms_serial_index.log" 2>&1 &
 wms_serial_pids+=("$!")
done
wms_traffic_options=()
if [[ -n "${WMS_SERIAL_TRAFFIC_MAX_P95_MS:-}" ]]; then wms_traffic_options+=(--max-action-p95-ms "$WMS_SERIAL_TRAFFIC_MAX_P95_MS"); fi
if [[ -n "${WMS_SERIAL_TRAFFIC_MIN_MOVES_PER_SECOND:-}" ]]; then wms_traffic_options+=(--min-moves-per-second "$WMS_SERIAL_TRAFFIC_MIN_MOVES_PER_SECOND"); fi
python3 scripts/serial_traffic.py --artifacts "$wms_serial_root" "${wms_traffic_options[@]}" \
 --seconds "${WMS_SERIAL_TRAFFIC_SECONDS:-300}" --products "${WMS_SERIAL_TRAFFIC_PRODUCTS:-8}" \
 --lots "${WMS_SERIAL_TRAFFIC_LOTS:-1}" --bins "${WMS_SERIAL_TRAFFIC_BINS:-8}" \
 --open-orders "${WMS_SERIAL_TRAFFIC_OPEN_ORDERS:-8}" --serial-units "${WMS_SERIAL_TRAFFIC_UNITS:-16}" \
 --serial-pattern "${WMS_SERIAL_TRAFFIC_PATTERN:-rotating}" \
 --min-serial-coverage "${WMS_SERIAL_TRAFFIC_MIN_COVERAGE:-1}"
