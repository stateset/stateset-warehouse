# One reproducible software release gate

Run `scripts/qualify-release.sh` locally and in GitHub Actions to execute the same release checks. A GitHub account billing block prevents hosted execution; it does not prevent this local gate. Passing local checks is recorded as local software evidence and never presented as a successful hosted run, deployed OMS/Freight acceptance or physical warehouse approval.

The gate requires an explicitly selected isolated PostgreSQL database whose name contains `test`, a role with CREATEDB for disposable restore databases, matching `psql`/`pg_dump`/`pg_restore` clients, Rust 1.90, Node 22, installed dashboard dependencies/Playwright browser, Python 3.8+, OpenSSL, curl and Docker access. The selected database receives synthetic test tenants and workload fixtures; choose a disposable database. The container and resilience stages create their own isolated PostgreSQL instances.

```bash
export PATH=/path/to/node22/bin:$PATH
export WMS_CHROME_PATH=/path/to/chrome
WMS_RELEASE_ARTIFACT_DIR=/tmp/warehouse-release-fresh-run \
  bash scripts/local-postgres.sh bash scripts/qualify-release.sh
```

The existing database variant is:

```bash
WMS_TEST_DATABASE_URL='postgresql://warehouse:warehouse@127.0.0.1:5548/warehouse_test' \
WMS_PG_BIN=/usr/lib/postgresql/16/bin \
WMS_RELEASE_REVISION=ACTUAL-REVIEWED-RELEASE-SHA \
WMS_RELEASE_ARTIFACT_DIR=/tmp/warehouse-release-fresh-run \
  bash scripts/qualify-release.sh
```

The artifact directory must be absent or empty. Each run creates new mode-restricted evidence; it refuses to overwrite prior results. `WMS_RESTORE_ADMIN_URL` defaults explicitly to `WMS_TEST_DATABASE_URL` and may select another isolated test database on an approved test server. Both URLs are validated before connection, and the actual connections must report a test database and CREATEDB permission. Database-name/service query overrides are rejected. The former CI restore step omitted the administrator URL; the single gate fixes that configuration and checks it before a lengthy build. The gate also reads actual Cargo metadata and requires its canonical `target_directory` to equal this repository's `target` directory, matching the executables it qualifies. A custom Cargo target directory fails early instead of accidentally executing an older binary.

## Required stages

| Stage | Executed proof |
|---|---|
| `prerequisites` | Explicit test databases/permissions, required tools/Docker, loopback port settings, tool versions and source fingerprints |
| `software_checks` | Formatting, all-target Clippy, default and real PostgreSQL Rust suites, qualification harness regression, strict dashboard build and offline browser contracts through `scripts/check.sh --postgres` |
| `observability_rules` | Actual syntax and behavior checks using the pinned official Prometheus image with no container network and read-only rules |
| `release_builds` | Locked API/restore-probe builds and optimized workload executable |
| `standard_workload` | Actual PostgreSQL concurrent moves, exact replays, stale versions, last-stock race, independent totals/tenant isolation and historical reconciliation |
| `archive_restore` | Custom archive into a fresh database, exact migration/table hashes, original replay, uncertain/rejected native recovery and a new reconciled post-restore command |
| `live_browsers` | Actual browser → API → PostgreSQL demo lifecycle, real-mode operational snapshot/export and separate scanner workspace, without HTTP interception |
| `production_container` | Production image build, immutable image/source identity, nonroot/read-only/drop-capabilities runtime, readiness/auth/UI/reconciliation and graceful shutdown |
| `image_resilience` | Same immutable image under sustained actual authenticated HTTP, multiple API processes, SIGKILL/restart and PostgreSQL container restart, original-request recovery and independent SQL proof |
| `stable_source_inputs` | Unchanged complete software inputs, identical image IDs/source labels between container and resilience reports, and identical tracked migration digests across restore/container/resilience |

There are no skip-stage switches. Any failed stage stops the gate, returns nonzero and records that stage as failed; subsequent stages remain `not_run`. An interrupt also prevents success. Missing prerequisites never become ignored tests or an apparent successful release.

The live APIs bind only to `127.0.0.1:8088` and `127.0.0.1:8089` by default. Existing listeners cause failure; the gate never kills another service. `WMS_RELEASE_DEMO_PORT` and `WMS_RELEASE_SCANNER_PORT` may select other distinct unprivileged loopback ports. Only the gate's recorded PIDs are stopped. It starts the API from a private temporary directory with no ambient `.env`, clears generic webhook configuration and explicitly disables native recipients, so acceptance fixtures cannot reach OMS/Freight. The directory links the compiled dashboard assets and is removed after the run. Container helpers use unique ownership labels and remove only their own resources.

The baseline Store workload is 100 products, 20 bins, two lots, 100 open orders, four clients and ten moves per client. Optional `WMS_RELEASE_WORKLOAD_PRODUCTS`, `_BINS`, `_LOTS`, `_OPEN_ORDERS`, `_CLIENTS` and `_ACTIONS` override the corresponding fixture inputs. `WMS_WORKLOAD_MAX_P95_MS` and `WMS_WORKLOAD_MIN_ACTIONS_PER_SECOND` retain the workload's explicit failure thresholds. Set them from a facility requirement; no default pretends to be a production SLO.

The separate image rehearsal defaults to 60 seconds of sustained work, 100 products, 100 open orders, two lots, 20 bins and four clients. For the larger local fixture:

```bash
WMS_RESILIENCE_PRODUCTS=500 WMS_RESILIENCE_OPEN_ORDERS=500 \
WMS_RESILIENCE_BINS=100 WMS_RESILIENCE_LOTS=2 \
WMS_RESILIENCE_SECONDS=60 \
WMS_RELEASE_ARTIFACT_DIR=/tmp/warehouse-release-large-fresh-run \
  bash scripts/local-postgres.sh bash scripts/qualify-release.sh
```

Setup is measured separately from the sustained window. The image comes from the immediately preceding container stage using its immutable `image.id`; a mutable tag cannot silently select another application. A crash/restart is useful recovery evidence and does not establish managed database failover, cross-region availability or facility capacity.

## Review and publish evidence

The root `summary.json` records every stage, status, duration and exit code plus artifact SHA256 values. `source-inputs-before.json`/`after.json` bind the Rust, migrations, browser, tests, examples, scripts, companion integration, deployment and workflow inputs. A changing working tree invalidates the result. A supplied release revision is retained; `unrecorded-working-tree` is explicit when a Git revision is unavailable. A fingerprint alone does not assert a clean commit.

Private raw evidence remains under the fresh artifact root: stage/API logs, image inspect data, generated synthetic credentials, browser traces and the synthetic database archive. Do not upload that entire directory. The `publish/` subdirectory contains only the known sanitized reports, source/tool/provenance data and stage logs with PostgreSQL URLs and credential-like environment values redacted. No raw image inspect files, credential files, database archives or browser traces are copied into `publish/`. The workflow uploads this subdirectory for 14 days even when a stage fails.

Review `publish/summary.json` first, then its individual workload, archive, observability, container and resilience reports. Every scenario uses synthetic inventory and credentials. `production_qualified` stays false. Use the [native service acceptance](native-acceptance.md) and [signed facility acceptance](facility-acceptance.md) procedures to supply the additional independent deployment and physical observations.
