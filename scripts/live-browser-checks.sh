#!/usr/bin/env bash
# The same real PostgreSQL desktop/mobile checks used by release qualification.
# This standalone entrypoint does not qualify containers, crashes or a facility.
set -euo pipefail
set +x
umask 077
cd "$(dirname "$0")/.."
wms_browser_repository="$PWD"
: "${WMS_TEST_DATABASE_URL:?Use scripts/local-postgres.sh or an explicitly isolated test database}"
python3 - <<'PYVALIDATE'
import os, re, urllib.parse
u = urllib.parse.urlsplit(os.environ['WMS_TEST_DATABASE_URL'])
name = urllib.parse.unquote(u.path.lstrip('/'))
if u.scheme not in ('postgres', 'postgresql') or not re.fullmatch(r'[A-Za-z0-9_]{1,63}', name) or 'test' not in name.lower():
    raise SystemExit('Live browser checks require an explicitly named test database')
if {key.lower() for key, _ in urllib.parse.parse_qsl(u.query)} & {'dbname', 'database', 'service', 'servicefile'}:
    raise SystemExit('Database identity overrides are not allowed')
PYVALIDATE
wms_browser_root="${WMS_BROWSER_ARTIFACT_DIR:-$(mktemp -d /tmp/stateset-wms-browsers.XXXXXXXX)}"
mkdir -p "$wms_browser_root"
[[ -z "$(ls -A "$wms_browser_root")" ]] || { echo 'Browser evidence directory must be empty.' >&2; exit 1; }
wms_browser_root="$(cd "$wms_browser_root" && pwd)"
mkdir "$wms_browser_root/raw"
wms_browser_runtime=''
wms_browser_api_pid=''
wms_browser_scanner_pid=''

wms_browser_stop_apis() {
  local wms_pid wms_pending=0
  for wms_pid in "$wms_browser_api_pid" "$wms_browser_scanner_pid"; do
    if [[ -n "$wms_pid" ]] && kill -0 "$wms_pid" 2>/dev/null; then
      kill -TERM "$wms_pid" 2>/dev/null || true
    fi
  done
  for ((wms_wait=0;wms_wait<20;wms_wait++)); do
    wms_pending=0
    for wms_pid in "$wms_browser_api_pid" "$wms_browser_scanner_pid"; do
      if [[ -n "$wms_pid" ]] && kill -0 "$wms_pid" 2>/dev/null; then wms_pending=1; fi
    done
    [[ "$wms_pending" == 0 ]] && break
    sleep 1
  done
  for wms_pid in "$wms_browser_api_pid" "$wms_browser_scanner_pid"; do
    if [[ -n "$wms_pid" ]]; then
      if kill -0 "$wms_pid" 2>/dev/null; then
        kill -KILL "$wms_pid" 2>/dev/null || true
        wms_pending=1
      fi
      wait "$wms_pid" 2>/dev/null || true
    fi
  done
  wms_browser_api_pid=''
  wms_browser_scanner_pid=''
  return "$wms_pending"
}


wms_browser_wait_api() {
  local wms_base="$1" wms_pid="$2"
  for ((wms_try=0;wms_try<60;wms_try++)); do
    kill -0 "$wms_pid" 2>/dev/null || { echo 'Owned qualification API exited before readiness.' >&2; return 1; }
    if curl --max-time 2 --silent --fail --output /dev/null "$wms_base/ready"; then return 0; fi
    sleep 1
  done
  echo 'Owned qualification API did not become ready.' >&2
  return 1
}


wms_browser_live_browsers() {
  python3 - <<'PY'
import os, socket
for name in ("WMS_BROWSER_DEMO_PORT", "WMS_BROWSER_SCANNER_PORT"):
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", int(os.environ[name])))
PY
  wms_browser_runtime="$(mktemp -d /tmp/stateset-wms-browser-runtime.XXXXXXXX)"
  python3 - "$wms_browser_runtime" <<'PY'
import pathlib, sys
directory = pathlib.Path(sys.argv[1]).resolve()
if any((parent / ".env").exists() for parent in (directory,) + tuple(directory.parents)):
    raise SystemExit("Live qualification runtime must not inherit an ambient .env file")
PY
  ln -s "$wms_browser_repository/dashboard" "$wms_browser_runtime/dashboard"
  WMS_SCANNER_OPERATOR_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
  WMS_SCANNER_COUNTER_TOKEN="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
  export WMS_SCANNER_OPERATOR_TOKEN WMS_SCANNER_COUNTER_TOKEN
  wms_browser_scanner_keys="$(python3 - <<'PY'
import json, os
print(json.dumps({os.environ["WMS_SCANNER_OPERATOR_TOKEN"]: {"tenant":"scanner-qualification","actor":"qualification:operator","role":"admin"},
                  os.environ["WMS_SCANNER_COUNTER_TOKEN"]: {"tenant":"scanner-qualification","actor":"qualification:counter","role":"admin"}}))
PY
)"
  (
    cd "$wms_browser_runtime"
    unset WMS_WORKOS_JSON WMS_OMS_WEBHOOK_URL WMS_OMS_WEBHOOK_TOKEN WMS_TMS_WEBHOOK_URL WMS_TMS_WEBHOOK_TOKEN WMS_WEBHOOK_TENANT
    # Each owned API intentionally receives its own isolated process environment.
    # shellcheck disable=SC2030
    export DATABASE_URL="$WMS_TEST_DATABASE_URL" WMS_DEMO=true WMS_BIND="127.0.0.1:$WMS_BROWSER_DEMO_PORT" WMS_NATIVE_INTEGRATIONS_JSON='{}'
    exec "$wms_browser_repository/target/debug/stateset-warehouse"
  ) > "$wms_browser_root/raw/demo-api.log" 2>&1 &
  wms_browser_api_pid=$!
  (
    cd "$wms_browser_runtime"
    unset WMS_WORKOS_JSON WMS_OMS_WEBHOOK_URL WMS_OMS_WEBHOOK_TOKEN WMS_TMS_WEBHOOK_URL WMS_TMS_WEBHOOK_TOKEN WMS_WEBHOOK_TENANT
    # The demo process's environment must not propagate into the real scanner API.
    # shellcheck disable=SC2031
    export DATABASE_URL="$WMS_TEST_DATABASE_URL" WMS_DEMO=false WMS_BIND="127.0.0.1:$WMS_BROWSER_SCANNER_PORT" WMS_NATIVE_INTEGRATIONS_JSON='{}' WMS_API_KEYS_JSON="$wms_browser_scanner_keys"
    exec "$wms_browser_repository/target/debug/stateset-warehouse"
  ) > "$wms_browser_root/raw/scanner-api.log" 2>&1 &
  wms_browser_scanner_pid=$!
  wms_browser_wait_api "http://127.0.0.1:$WMS_BROWSER_DEMO_PORT" "$wms_browser_api_pid"
  wms_browser_wait_api "http://127.0.0.1:$WMS_BROWSER_SCANNER_PORT" "$wms_browser_scanner_pid"
  (
    cd dashboard
    WMS_BROWSER_URL="http://127.0.0.1:$WMS_BROWSER_DEMO_PORT" \
      WMS_SCANNER_BROWSER_URL="http://127.0.0.1:$WMS_BROWSER_SCANNER_PORT" \
      npx playwright test --project=live-postgres --output "$wms_browser_root/live-browser-results"
  )
  wms_browser_stop_apis
}

cleanup() {
  local result=$?
  trap - EXIT INT TERM
  wms_browser_stop_apis || result=1
  if [[ -n "$wms_browser_runtime" && "$wms_browser_runtime" == /tmp/stateset-wms-browser-runtime.* ]]; then
    rm -rf -- "$wms_browser_runtime"
  fi
  echo "Live browser evidence: $wms_browser_root (exit $result)"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# Reserve/check both candidates together, then the APIs must actually bind them.
# A bind race fails explicitly; never connect tests to an unowned listener.
read -r WMS_BROWSER_DEMO_PORT WMS_BROWSER_SCANNER_PORT < <(python3 - <<'PYPORTS'
import os, socket
sockets = [socket.socket(), socket.socket()]
for sock, name in zip(sockets, ('WMS_BROWSER_DEMO_PORT', 'WMS_BROWSER_SCANNER_PORT')):
    sock.bind(('127.0.0.1', int(os.environ.get(name, '0'))))
print(*(sock.getsockname()[1] for sock in sockets))
for sock in sockets: sock.close()
PYPORTS
)
export WMS_BROWSER_DEMO_PORT WMS_BROWSER_SCANNER_PORT
cargo build --locked --bin stateset-warehouse
npm --prefix dashboard run build
wms_browser_live_browsers
