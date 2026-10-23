#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
node -e 'const [major,minor]=process.versions.node.split(".").map(Number); if(major<22 || (major===22 && minor<12)){console.error("Node.js >=22.12 is required. Select Node 22 before running checks.");process.exit(1)}'
if [[ -z "${WMS_CHROME_PATH:-}" ]] && ! node --input-type=module -e 'import fs from "node:fs"; import {chromium} from "./dashboard/node_modules/@playwright/test/index.mjs"; process.exit(fs.existsSync(chromium.executablePath())?0:1)' 2>/dev/null; then
  for wms_browser in /opt/google/chrome/chrome /usr/bin/google-chrome /usr/bin/chromium /snap/bin/chromium; do
    if [[ -x "$wms_browser" ]]; then export WMS_CHROME_PATH="$wms_browser"; break; fi
  done
  if [[ -z "${WMS_CHROME_PATH:-}" ]]; then
    echo 'Install dashboard dependencies and a Playwright browser, or set WMS_CHROME_PATH to an installed Chromium browser.' >&2
    exit 1
  fi
fi
cargo fmt --package stateset-warehouse -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --lib --test domain --test planning_units --test http_auth --test mcp --test console --test native_contracts --test native_api --test oms_client --test oms_fulfillment_api --test operations --test startup --test serial_admission --test icommerce
python3 -m unittest discover -s scripts -p 'test_*.py'
python3 scripts/dashboard_notices.py --check
node --test cli/test/*.test.mjs
if [[ "${1:-}" == '--postgres' ]]; then
  : "${WMS_TEST_DATABASE_URL:?Set WMS_TEST_DATABASE_URL to an isolated Postgres database with test in its name}"
  cargo test --locked --lib --test postgres --test persistence_acceptance --test mcp --test console --test native_api --test persistence_guards --test oms_fulfillment_api --test oms_receiver --test operations --test startup --test serial_admission --test serial_registry --test serial_scale --test sql_catalog --test warehouse_planning --test icommerce -- --ignored --test-threads=1
fi
npm --prefix dashboard run build
cargo build --locked --example domain_driver
(cd dashboard && npx playwright test --project=offline-contract)
