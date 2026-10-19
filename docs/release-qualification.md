# One reproducible software release gate

Run `scripts/qualify-release.sh` locally and in GitHub Actions to execute the same release checks. The local gate remains available when hosted runners are unavailable. Local software evidence, hosted CI results, deployed OMS/Freight acceptance and physical warehouse approval are recorded separately.

The gate requires an explicitly selected isolated PostgreSQL database whose name contains `test`, a role with CREATEDB for disposable restore databases, matching `psql`/`pg_dump`/`pg_restore` clients, Rust 1.90, Node 22.12+, installed dashboard dependencies/Playwright browser, Python 3.8+, OpenSSL, curl and Docker access. The selected database receives synthetic test tenants and workload fixtures; choose a disposable database. The container and resilience stages create their own isolated PostgreSQL instances.

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
| `software_checks` | Formatting, all-target Clippy, default and real PostgreSQL Rust suites including actual MCP transport/SDK/authentication/journal recovery, qualification harness regression, strict dashboard build and offline browser contracts through `scripts/check.sh --postgres` |
| `observability_rules` | Actual syntax and behavior checks using the pinned official Prometheus image with no container network and read-only rules |
| `release_builds` | Locked API/restore-probe builds and optimized workload executable |
| `standard_workload` | Actual PostgreSQL concurrent moves, exact replays, stale versions, last-stock race, independent totals/tenant isolation and historical reconciliation |
| `archive_restore` | Custom archive into a fresh database, exact migration/table hashes, original facility/physical/cancellation/transfer dispatch-arrival replay, in-transit custody, partial shipment/active packing/backorder/early-return ownership and original replay, uncertain/rejected native recovery and a new reconciled post-restore command |
| `live_browsers` | Actual browser → API → PostgreSQL demo lifecycle, real-mode operational snapshot/export and separate scanner workspace, without HTTP interception |
| `production_container` | Production image build, immutable image/source identity, nonroot/read-only/drop-capabilities runtime, readiness/auth/UI/reconciliation, authenticated MCP discovery/session and graceful shutdown |
| `image_resilience` | Both untracked and literal serial-controlled inventory on the same immutable image, each with sustained authenticated HTTP, two API processes, SIGKILL/restart, PostgreSQL container restart, original-request recovery and independent SQL proof; the serial profile also rebuilds every literal custody chain and original observation |
| `stable_source_inputs` | Unchanged complete software inputs, identical image IDs/source labels between container and resilience reports, and identical tracked migration digests across restore/container/resilience |

There are no skip-stage switches. Any failed stage stops the gate, returns nonzero and records that stage as failed; subsequent stages remain `not_run`. An interrupt also prevents success. Missing prerequisites never become ignored tests or an apparent successful release.

The live APIs bind only to `127.0.0.1:8088` and `127.0.0.1:8089` by default. Existing listeners cause failure; the gate never kills another service. `WMS_RELEASE_DEMO_PORT` and `WMS_RELEASE_SCANNER_PORT` may select other distinct unprivileged loopback ports. Only the gate's recorded PIDs are stopped. It starts the API from a private temporary directory with no ambient `.env`, clears generic webhook configuration and explicitly disables native recipients, so acceptance fixtures cannot reach OMS/Freight. The directory links the compiled dashboard assets and is removed after the run. Container helpers use unique ownership labels and remove only their own resources.

Set `WMS_KEEP_QUALIFICATION_RESOURCES=1` to stop the uniquely labelled container services while retaining their named database volumes and networks on success, failure or interruption. The smoke database uses a named volume in this mode; its default remains temporary memory. Private `resource-disposition.jsonl`, service logs and final container states record the outcome. Ownership mismatches prevent cleanup and fail the run. Pair this option with `WMS_KEEP_TEST_DB=1 WMS_KEEP_RESTORE_DB=1` to retain the native test cluster and both restore databases. The default disposable mode removes only resources owned by that run.

The baseline Store workload is 100 products, 20 bins, two lots, 100 open orders, four clients and ten moves per client. Optional `WMS_RELEASE_WORKLOAD_PRODUCTS`, `_BINS`, `_LOTS`, `_OPEN_ORDERS`, `_CLIENTS` and `_ACTIONS` override the corresponding fixture inputs. `WMS_WORKLOAD_MAX_P95_MS` and `WMS_WORKLOAD_MIN_ACTIONS_PER_SECOND` retain the workload's explicit failure thresholds. Set them from a facility requirement; no default pretends to be a production SLO.

Each of the two required image rehearsals defaults to 60 seconds of sustained work, 100 products, 100 open orders, two lots, 20 bins and four independent clients. The serial rehearsal defaults to sixteen literal identities per opening SKU/lot (`WMS_RELEASE_SERIAL_UNITS`, range 2–256), including identities that remain in place throughout the run. Each client keeps its own reviewed snapshot and validates a full warehouse refresh after each confirmed save; serial clients additionally review their literal source/custody. Action latency includes these reviews, while elapsed throughput also includes the exact cross-replica replay. Both reports must bind the same image, source and migration digests. Neither profile can be skipped. The standalone `resilience-rehearsal.sh` still supports `WMS_RESILIENCE_TRAFFIC_MODEL=shared-snapshot` for the older synchronized-round comparison, and `WMS_RESILIENCE_SERIAL_UNITS` selects its inventory mode. See the [traffic contract](resilience.md). For the larger local fixture:

```bash
WMS_RESILIENCE_PRODUCTS=500 WMS_RESILIENCE_OPEN_ORDERS=500 \
WMS_RESILIENCE_BINS=100 WMS_RESILIENCE_LOTS=2 \
WMS_RESILIENCE_SECONDS=60 \
WMS_RELEASE_ARTIFACT_DIR=/tmp/warehouse-release-large-fresh-run \
  bash scripts/local-postgres.sh bash scripts/qualify-release.sh
```

Setup is measured separately from the sustained window. The image comes from the immediately preceding container stage using its immutable `image.id`; a mutable tag cannot silently select another application. A crash/restart is useful recovery evidence and does not establish managed database failover, cross-region availability or facility capacity.

The lost-response fixture retains the untracked recovery SKU. Untracked crash commands retain their warehouse-row-lock barrier before inventory mutation. In serial mode, the two crash commands move one otherwise static literal label out and back. An owned `BEFORE INSERT` outbox trigger first verifies the exact uncommitted custody revision, original history/observation/batch/head, event, actor-bound receipt and physical quantities, then waits on an advisory lock. The external qualifier requires the post-assertion session marker, exact advisory-lock identity and actual blocking PID before killing the API or PostgreSQL. All 49 committed table fingerprints must remain unchanged after each interrupted transaction; the same original request then commits once and replays through the other replica. The trigger is removed before recovery. The independent literal proof accounts for both recovered moves separately from timed traffic. Earlier reports retain their original, narrower crash scope. Latency distributions in these image reports are runner aggregates; the earlier native serial diagnostic separately retains raw cycle samples.

## Current 0.1.1 serial after-write qualification — 2026-10-19

The [version 0.1.1 complete gate](qualification/release-20261019-serial-after-write-v1/summary.json) and [independent audit](qualification/release-20261019-serial-after-write-v1/independent-audit.json) pass both API and PostgreSQL serial after-write crash recovery at the unchanged 500-product / 16,000-label / eight-operator profile. Both interrupted transactions preserve all 49 committed table fingerprints; their exact original requests recover once and replay. Two recovered out-and-back serial moves are excluded from timed traffic and included separately in the literal history proof. The [prior timeout and passing repeat](qualification/attempt-20261019-serial-upgrade-index-timeout/failure-context.json) remain recorded. Production, deployed-client, physical facility and managed failover qualification remain separate.

## Review and publish evidence

The root `summary.json` records every stage, status, duration and exit code plus artifact SHA256 values. `source-inputs-before.json`/`after.json` bind the Rust, migrations, browser, tests, examples, scripts, companion integration, deployment and workflow inputs. A changing working tree invalidates the result. A supplied release revision is retained; `unrecorded-working-tree` is explicit when a Git revision is unavailable. A fingerprint alone does not assert a clean commit.

Private raw evidence remains under the fresh artifact root: stage/API logs, image inspect data, generated synthetic credentials, browser traces and the synthetic database archive. Do not upload that entire directory. The `publish/` subdirectory contains only the known sanitized reports, source/tool/provenance data and stage logs with PostgreSQL URLs and credential-like environment values redacted. No raw image inspect files, credential files, database archives or browser traces are copied into `publish/`. The workflow uploads this subdirectory for 14 days even when a stage fails.

Review `publish/summary.json` first, then its individual workload, archive, observability, container, `image_resilience.json` and `image_serial_resilience.json` reports. Every scenario uses synthetic inventory and credentials. `production_qualified` stays false. Use the [native service acceptance](native-acceptance.md) and [signed facility acceptance](facility-acceptance.md) procedures to supply the additional independent deployment and physical observations.


## Reviewed-pack release — 2026-10-18

The [complete pack-unit gate](qualification/release-20261018-pack-units/summary.json) and its dated bundle passed all ten stages on 161 frozen software inputs (`cf2a64ddd67e59ed2943606a61da781263b0d7491753f497459a569a8959f609`), twenty migrations, 43 tables and immutable image `sha256:7c74f8c03d743e2a8dd921fc55275dd9927501de55114ebc81695ef2e2034eee`. Counts are 257 Rust, 26 Python, 137 intercepted browser contracts and thirteen actual browser/API/PostgreSQL workflows. The source archive and original custom database archive remain privately retained and independently rehashed; all previous migrations and qualification bundles remain preserved.

New coverage includes eleven pack PostgreSQL cases, default legacy-request byte compatibility, actual typed MCP conversion/replay, browser source/staleness/form checks, scanned recovery, 390px layout and nonempty cross-timezone restoration of four definition changes, six observations and fifteen original requests. Later sizes and disabled codes preserve original factors and each custody. Fresh obsolete definitions reject without mutations; a separate current-factor request succeeds. The [interrupted earlier gate](qualification/attempt-20261018-pack-units-pattern-validation/failure-context.json) remains explicitly failed, with its original artifacts and first source capsule retained.

The container and resilience stages use this same image; other stages build the same frozen inputs. Timed traffic is REST each-unit bin moves and does not qualify pack-heavy throughput. Serial/fractional measurement, deployed identity/native/MCP clients, real scanner/facility observations, managed failover and controlled capacity remain open. `production_qualified=false`.

## Current facility-evidence software release — 2026-10-18

The [current complete gate](qualification/release/summary.json) and [dated bundle](qualification/release-20261018-facility-evidence-v2/summary.json) passed all ten stages in 844 seconds on 167 frozen software inputs (`4014e191ac8cb4e2fbb1bfe3ddc432981c0e483faa0b0dff0f5d36f21625475c`), twenty migrations and 43 tables. Immutable image: `sha256:5740c4e4fd3a89c47fba21f6770f4c17a18f2a4afa8d6c38e904fbf3efec74aa`. Counts are 257 Rust, 51 Python, 137 intercepted browser contracts and thirteen actual browser/API/PostgreSQL workflows. Source/restore archives, all previous dated bundles and earlier failures remain retained and independently hashed.

The default offline facility gate now binds exact qualified source/image/schema and a predeclared signed plan, requires independent operator/controller signatures, and checks captured sources for 23 witness-attested scenarios. Original receipt/digest/custody relationships, complete trace pages, native replay consistency, sample-derived peak/concurrency and nanosecond recovery arithmetic have positive and tamper regressions. The [runbook](facility-acceptance.md) explains private nonperformed templates and the limits of these checks. Its synthetic tests supply no actual hardware, facility or receiver acceptance. The explicitly limited legacy profile remains available for historical evidence.

Container and resilience stages use the same immutable image; other stages build the same frozen inputs. Sustained traffic remains REST bin moves. Physical/deployed identity/native/MCP-client acceptance, serial/fractional workflows, managed failover and controlled capacity remain open. `production_qualified=false`.
