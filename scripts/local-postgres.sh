#!/usr/bin/env bash
# Run the real database acceptance suite against a disposable, socket-only server.
# No existing database, network listener, container, or external service is used.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ "$(id -u)" == 0 ]]; then
  echo 'Run this script as an unprivileged user; PostgreSQL refuses to run as root.' >&2
  exit 1
fi

if [[ -n "${WMS_PG_BIN:-}" ]]; then
  wms_pg_bin="$WMS_PG_BIN"
elif command -v pg_config >/dev/null 2>&1; then
  wms_pg_bin="$(pg_config --bindir)"
else
  echo 'Install PostgreSQL server binaries, or set WMS_PG_BIN to their directory.' >&2
  exit 1
fi
for wms_binary in initdb pg_ctl createdb; do
  if [[ ! -x "$wms_pg_bin/$wms_binary" ]]; then
    echo "Missing PostgreSQL executable: $wms_pg_bin/$wms_binary" >&2
    exit 1
  fi
done

wms_test_root="$(mktemp -d /tmp/stateset-wms-test.XXXXXXXX)"
wms_test_port=55439
mkdir -m 700 "$wms_test_root/socket"
cleanup() {
  local result=$?
  trap - EXIT INT TERM
  "$wms_pg_bin/pg_ctl" -D "$wms_test_root/data" -m immediate -w stop >/dev/null 2>&1 || true
  if [[ "$result" != 0 && -f "$wms_test_root/postgres.log" ]]; then
    cat "$wms_test_root/postgres.log" >&2
  fi
  if [[ "${WMS_KEEP_TEST_DB:-0}" == 1 ]]; then
    echo "Stopped test database retained at $wms_test_root" >&2
  else
    rm -rf -- "$wms_test_root"
  fi
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

"$wms_pg_bin/initdb" -D "$wms_test_root/data" --auth-local=trust --auth-host=reject --no-locale --encoding=UTF8 --no-sync >/dev/null
"$wms_pg_bin/pg_ctl" -D "$wms_test_root/data" -l "$wms_test_root/postgres.log" -t 15 \
  -o "-c listen_addresses='' -c unix_socket_directories='$wms_test_root/socket' -c max_connections=100 -p $wms_test_port" -w start >/dev/null
"$wms_pg_bin/createdb" --host "$wms_test_root/socket" --port "$wms_test_port" wms_local_test
WMS_TEST_DATABASE_URL="postgresql:///wms_local_test?host=$wms_test_root/socket&port=$wms_test_port&user=$(id -un)"
export WMS_TEST_DATABASE_URL

if [[ "$#" == 0 ]]; then
  cargo test --locked --lib --test postgres --test persistence_acceptance --test mcp --test persistence_guards --test native_api --test oms_fulfillment_api --test oms_receiver --test operations --test startup --test serial_admission --test serial_registry -- --ignored --test-threads=1
else
  "$@"
fi
