# StateSet Warehouse

Warehouse execution and physical inventory authority for StateSet. **Sync is the OMS; Warehouse is the WMS; Freight is the TMS.**

[Desktop preview](docs/screenshots/desktop.png) · [Mobile preview](docs/screenshots/mobile.png) · [Architecture](docs/architecture.md) · [Verification evidence](docs/evidence.md) · [API contract](docs/api.md) · [Native OMS/TMS adapters](docs/native-integrations.md) · [Operations and restore](docs/operations.md)

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
- Putaway and transfers of unreserved stock. Receiving stock becomes allocatable only after it reaches storage.
- OMS-linked or manual fulfillment orders, with immutable line IDs and independent reservations even when several lines use the same SKU.
- Atomic full-order allocation, partial line picks, protected outbound staging, packing and physical carrier handoff evidence.
- Short-pick recovery: physically return an exact line's staged units, release allocations, recount, resolve the exception and allocate again. Returning packed units invalidates packing and requires another pack before shipping.
- Cancellation and reservation release before picking; blocking order exceptions and retained resolution evidence.
- Administrator physical counts with reasons, stale-version protection and rejection below reserved balances.
- Physical movement ledger, independent balance/reservation reconstruction, searchable views and CSV exports.
- Server-owned tenant/operator/role identity, stable request IDs, saved command receipts and browser recovery after uncertain saves.
- Durable OMS events and TMS readiness/handoff events, leased delivery, bounded retries, retained delivery attempts, dead letters and audited requeue.
- Source-derived Sync order import and Freight planned-load adapters, with strict tenant/source identities, exact whole-unit quantities, explicit transport facts and retained readiness evidence. Native OMS status writeback remains held because its current endpoint cannot condition the update on a source revision.
- Normalized operational rows, database-enforced movement/reservation/receipt proofs, and append-only movement/activity history with cursor pagination and archived record details.

Quantities are whole **each** units. Partial receipts/picks are supported; partial shipments, serial/expiry control, returns, replenishment planning and carrier purchasing are not implemented. Do not use the demo as operational inventory.

## Real workspaces

Set `WMS_DEMO=false` and configure `WMS_API_KEYS_JSON` in `.env` or your secret manager:

```dotenv
WMS_API_KEYS_JSON={"replace-with-a-random-secret-of-at-least-32-characters":{"tenant":"your-org","actor":"operator:alice","role":"admin"}}
```

Real tenants start with an empty `MAIN` warehouse, a receiving bin and staging bin. Configure the physical facility, create products/storage locations, and record actual receipts or opening physical counts. This version provisions one warehouse per tenant; it does not expose a multi-facility administration product. Separate warehouses are already part of every persistence and API key.

Roles are `viewer`, `operator`, `admin` and `oms`. Operators execute warehouse work; admins additionally manage facilities/catalog/locations and count adjustments. OMS credentials may create source-linked orders and cancel them before physical picking. Use individual keys for attributable actions. Browser credentials are session-scoped; sign-out removes the local credential. Server revocation is by removing/rotating the configured key and restarting instances. SSO integration is not claimed.

Integrations are opt-in server configuration. The [native adapters](docs/native-integrations.md) use Sync's tenant-bound order API and Freight's `/v1/sync/import` planned-load API. Configure their explicit tenant map with `WMS_NATIVE_INTEGRATIONS_JSON`. These contracts have source-derived fixtures and local HTTP tests; deployed credentials, tenant mappings and live external services still require qualification. Sync's unsafe fulfillment endpoint is deliberately held for review, with the handoff evidence retained.

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

The live suite includes persistence, concurrency, HTTP warehouse lifecycle, native API recovery and database guard tests. Explicit invocation fails without a database; default `cargo test` marks these tests ignored. The default check runs domain, authorization, native contract and offline browser tests. CI is configured to run both live database and live browser checks; its current GitHub run is blocked by organization billing/spending limits.

For disposable socket-only PostgreSQL verification, `scripts/local-postgres.sh` initializes an isolated temporary cluster and runs the warehouse persistence suites. Where sockets are unavailable, `python3 scripts/check-sql.py` applies all migrations in an isolated single-user backend, type-checks runtime SQL, executes independent reconciliation, and verifies receipt/reservation constraints, append-only history and fault-injected rollback. That check does not establish SQLx bindings or concurrent client behavior.

## Deployment boundaries

The API serves static dashboard assets, `/health` liveness and `/ready` database readiness. Run migrations with a database identity allowed to create the WMS schema on startup. Terminate TLS at your ingress, use strong individual keys, back up Postgres, and qualify restore and physical reconciliation before live use. The container runs as a nonroot user. Build with `docker build -t stateset-warehouse .`; the Compose `app` profile requires real credentials.

Persistence stores current products, locations, stock, receipt lines, orders and allocation ownership in normalized PostgreSQL rows. Movements and activity are append-only records; bounded operational views and keyset history pages keep growing history out of each command's write set. The former 8 MiB warehouse aggregate limit is removed. Startup applies tracked migrations and verifies legacy evidence before importing an older snapshot.

Writes still serialize within each warehouse. The retained-history test exceeds the former limit, but no production throughput or facility capacity is claimed. Real physical counts, operator/device acceptance, deployed cross-service tenant mappings and backup/restore qualification remain required. See [the evidence report](docs/evidence.md) for executed checks and remaining limits.
