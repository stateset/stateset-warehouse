#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --test domain --test http_auth --test native_contracts --test native_api
if [[ "${1:-}" == '--postgres' ]]; then
  : "${WMS_TEST_DATABASE_URL:?Set WMS_TEST_DATABASE_URL to an isolated Postgres database with test in its name}"
  cargo test --locked --test postgres --test persistence_acceptance --test native_api --test persistence_guards -- --ignored --test-threads=1
fi
npm --prefix dashboard run build
cargo build --locked --example domain_driver
(cd dashboard && npx playwright test --project=offline-contract)
