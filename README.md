# StateSet Warehouse

Warehouse execution and physical inventory authority for StateSet. **Sync is the OMS; Warehouse is the WMS; Freight is the TMS.**

[Desktop preview](docs/screenshots/desktop.png) · [Mobile preview](docs/screenshots/mobile.png) · [Architecture](docs/architecture.md) · [Verification evidence](docs/evidence.md) · [API contract](docs/api.md) · [Native OMS/TMS adapters](docs/native-integrations.md) · [Operations and restore](docs/operations.md) · [Release qualification](docs/release-qualification.md)

Rust 1.90+, Axum, Tokio, SQLx/Postgres, React, TypeScript and Vite, following the existing StateSet services and dashboards.

## Run

Requires Rust 1.90+, Node.js 22+, npm and Postgres 12+. Use an isolated development database.

```bash
cp .env.example .env
docker compose up -d postgres
npm --prefix dashboard ci
bash scripts/dev.sh
```

Open **http://127.0.0.1:3012**, then select **Open synthetic demo**. The API runs on **127.0.0.1:8088**. The demo uses synthetic data in its own tenant, persists changes in Postgres, and refuses external event receivers. There is no silent memory fallback. The dev script starts/stops its own API; it leaves Postgres running.

To serve the built UI and API together:

```bash
npm --prefix dashboard run build
cargo run --locked
```

Open **http://127.0.0.1:8088**. Localhost is a secure browser context; use HTTPS for remotely accessed deployments.

## Implemented warehouse work

- Catalog products and receiving/storage locations; configure facility name, code, city and IANA timezone from Locations.
- Expected receipts with immutable line IDs, explicit lot IDs, partial receiving and over-receipt rejection.
- Scanner receiving and picking with bin → SKU → lot validation, exact receipt/allocation ownership, captured warehouse versions and explicit quantity confirmation. Uncertain saves retain the original request across reloads.
- Putaway and transfers of unreserved stock. Receiving stock becomes allocatable only after it reaches storage.
- OMS-linked or manual fulfillment orders, with immutable line IDs and independent reservations even when several lines use the same SKU.
- Atomic full-order allocation, partial line picks, protected outbound staging, packing and physical carrier handoff evidence.
- Short-pick recovery: physically return an exact line's staged units, release allocations, recount, resolve the exception and allocate again. Returning packed units invalidates packing and requires another pack before shipping.
- Cancellation and reservation release before picking; blocking order exceptions and retained resolution evidence.
- Administrator physical counts with reasons, stale-version protection and rejection below reserved balances.
- Physical movement ledger, independent balance/reservation reconstruction, searchable views and CSV exports.
- Server-owned tenant/operator/role identity, stable request IDs, saved command receipts and browser recovery after uncertain saves. Definitive stale-version rejection can safely retire a request that never committed; ambiguous outcomes remain held.
- Durable OMS events and TMS readiness/handoff events, leased delivery, immutable receiver fingerprints, crash-counted retry budgets, retained delivery attempts, fenced late responses, dead letters and audited requeue.
- Source-derived Sync order import and Freight planned-load adapters, with strict tenant/source identities, exact whole-unit quantities, explicit transport facts and retained readiness evidence. Freight acknowledgments must retain an eligible canonical source as well as exact source lines.
- Opt-in conditional OMS fulfillment after physical handoff, with frozen source revisions, receiver capability checks, immutable accepted/rejected receipts and exact uncertain-request recovery. The [companion OMS receiver](integrations/stateset-sync-server/README.md) must be deployed and qualified first; the legacy endpoint stays held.
- Normalized operational rows, database-enforced movement/reservation/receipt proofs, and append-only movement/activity history with cursor pagination and archived record details.
- Tenant-wide Operations view for delivery queues, uncertain native requests and warehouse exceptions, with bounded consistent snapshots, explicit stale/offline status, current-evidence exports and administrator Prometheus metrics with tested alert policies.

Quantities are whole **each** units. Partial receipts/picks are supported; partial shipments, serial/expiry control, returns, replenishment planning and carrier purchasing are not implemented. Do not use the demo as operational inventory.

## Real workspaces

Set `WMS_DEMO=false`. For operator sign-in, configure [WorkOS AuthKit](docs/authentication.md). For service credentials or API-key mode, configure `WMS_API_KEYS_JSON` in `.env` or your secret manager:

```dotenv
WMS_API_KEYS_JSON={"replace-with-a-random-secret-of-at-least-32-characters":{"tenant":"your-org","actor":"operator:alice","role":"admin"}}
```

Real tenants start with an empty `MAIN` warehouse, a receiving bin and staging bin. Configure the physical facility, create products/storage locations, and record actual receipts or opening physical counts. This version provisions one warehouse per tenant; it does not expose a multi-facility administration product. Separate warehouses are already part of every persistence and API key.

Roles are `viewer`, `operator`, `admin` and `oms`. Operators execute warehouse work; admins additionally manage facilities/catalog/locations and count adjustments. OMS credentials may create source-linked orders and cancel them before physical picking. Use individual keys for attributable actions. Browser credentials are session-scoped; sign-out removes the local credential. Server revocation is by removing/rotating the configured key and restarting instances. Optional [WorkOS AuthKit sign-in](docs/authentication.md) maps verified organization memberships to explicit WMS tenants and roles, with signed access tokens and browser refresh/sign-out. It requires actual WorkOS configuration and deployed qualification; it is not enabled by default. API keys remain available for service identities.

Integrations are opt-in server configuration. The [native adapters](docs/native-integrations.md) use Sync's tenant-bound order API and Freight's `/v1/sync/import` planned-load API. Configure their explicit tenant map with `WMS_NATIVE_INTEGRATIONS_JSON`. These contracts have source-derived fixtures and local HTTP tests; deployed credentials, tenant mappings and live external services still require qualification. OMS writeback defaults to held. After deploying the included companion receiver, explicitly enable `shipment_writeback: "conditional_v1"` for the qualified tenant. The WMS never falls back to the unsafe legacy fulfillment endpoint.

The separately configured event webhooks use the WMS [producer contract](docs/api.md). They require compatible consumers; an existing application's `/events` endpoint is not assumed to accept warehouse events.

## Verify

```bash
npm --prefix dashboard ci
cd dashboard && npx playwright install chromium && cd ..
bash scripts/check.sh
```

For an existing Chrome installation, set `WMS_CHROME_PATH=/usr/bin/google-chrome`. Browser contract tests intercept requests and call the real Rust reducer; they do not prove database/HTTP integration.

Run the required live Postgres suite against a separate database with `test` in its name:

```bash
WMS_TEST_DATABASE_URL=postgres://warehouse:warehouse@127.0.0.1:5548/warehouse_test bash scripts/check.sh --postgres
```

The live suite includes persistence, concurrency, HTTP warehouse lifecycle, native API recovery, conditional OMS receiver races and database guard tests. Explicit invocation fails without a database; default `cargo test` marks these tests ignored. The default check runs domain, authorization, native contract and offline browser tests. CI is configured to run both live database and live browser checks; its current GitHub run is blocked by organization billing/spending limits.

For disposable socket-only PostgreSQL verification, `bash scripts/local-postgres.sh` initializes an isolated temporary cluster and runs the database suites. `bash scripts/local-postgres.sh bash scripts/check.sh --postgres` runs the complete local check against that cluster. Where sockets are unavailable, `python3 scripts/check-sql.py` applies all migrations in an isolated single-user backend, type-checks runtime SQL, executes independent reconciliation, and verifies receipt/reservation constraints, append-only history and fault-injected rollback. That check does not establish SQLx bindings or concurrent client behavior.

Run the [complete software release gate](docs/release-qualification.md) with Docker, matching PostgreSQL tools and the browser installed:

```bash
bash scripts/local-postgres.sh bash scripts/qualify-release.sh
```

CI invokes this same gate. It includes actual archive restoration, live browser execution, monitoring rules, hardened production-container checks and sustained HTTP across two API replicas with forced API/PostgreSQL crashes. Reports bind the exact source, migration and immutable image identities; failed or missing stages cannot pass. Only the sanitized `publish/` artifact directory is suitable for uploading. [Native deployment preflight](docs/native-acceptance.md) and [signed facility acceptance](docs/facility-acceptance.md) require separately supplied deployment and physical observations.

## Deployment boundaries

The API serves static dashboard assets, `/health` liveness and `/ready` database readiness. Run migrations with a database identity allowed to create the WMS schema on startup. Terminate TLS at your ingress, use strong individual keys, back up Postgres, and qualify restore and physical reconciliation before live use. The container runs as UID 10001 and checks database readiness. Build with `docker build -t stateset-warehouse .`; the Compose `app` profile requires real credentials. `bash scripts/container-smoke.sh` builds and runs the actual image with an isolated PostgreSQL container, a read-only filesystem, no capabilities and no new privileges.

Persistence stores current products, locations, stock, receipt lines, orders and allocation ownership in normalized PostgreSQL rows. Movements and activity are append-only records; bounded operational views and keyset history pages keep growing history out of each command's write set. The former 8 MiB warehouse aggregate limit is removed. Startup applies tracked migrations and verifies legacy evidence before importing an older snapshot.

Writes serialize within each warehouse. The [workload harness and measured profiles](docs/performance.md) expose the cost of active work rather than claiming production capacity. The [restore rehearsal](docs/operations.md) has exercised an actual archive round-trip, complete table hashes, full reconciliation and recovered request journals. Real physical counts, actual scanner/device acceptance and deployed cross-service tenant mappings still require facility evidence. See [the evidence report](docs/evidence.md) for executed checks and limits.
