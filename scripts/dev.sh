#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "$(node -p 'Number(process.versions.node.split(".")[0])')" -lt 22 ]]; then
  echo 'Node.js 22+ is required. Activate your Node 22 environment first.' >&2
  exit 1
fi
if [[ ! -f .env ]]; then cp .env.example .env; fi
# Load only for the child API; never print configuration or credentials.
if [[ ! -d dashboard/node_modules ]]; then npm --prefix dashboard ci --no-audit --no-fund; fi
cargo build --locked
./target/debug/stateset-warehouse &
wms_api_pid=$!
cleanup() { kill "$wms_api_pid" 2>/dev/null || true; wait "$wms_api_pid" 2>/dev/null || true; }
trap cleanup EXIT INT TERM
for _ in {1..40}; do
  if curl --silent --fail http://127.0.0.1:8088/ready >/dev/null; then
    npm --prefix dashboard run dev
    exit $?
  fi
  if ! kill -0 "$wms_api_pid" 2>/dev/null; then echo 'API failed to start. Check DATABASE_URL and authentication configuration.' >&2; exit 1; fi
  sleep 0.25
done
echo 'API did not become ready on 127.0.0.1:8088.' >&2
exit 1
