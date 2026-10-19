# StateSet Warehouse

Warehouse execution and physical inventory authority for StateSet. **Sync is the OMS; Warehouse is the WMS; Freight is the TMS.**

[Desktop preview](docs/screenshots/desktop.png) · [Mobile preview](docs/screenshots/mobile.png) · [Architecture](docs/architecture.md) · [Verification evidence](docs/evidence.md) · [API contract](docs/api.md) · [Native OMS/TMS adapters](docs/native-integrations.md) · [Operations and restore](docs/operations.md) · [Release qualification](docs/release-qualification.md)

Rust 1.90+, Axum, Tokio, SQLx/Postgres, React, TypeScript and Vite, following the existing StateSet services and dashboards.

The [current local software release](docs/qualification/release-20261019-icommerce-tui-v1/summary.json) passes all ten stages on version `0.2.0`: **331 Rust, 74 Python, 13 terminal and 170 browser checks**. It includes pinned iCommerce crate interoperability, the warehouse TUI, populated archive restore and both full image/crash workloads. Production, deployed-client and physical facility acceptance remain separate.

Version 0.2.0 includes [iCommerce crate interoperability](docs/icommerce.md), a [warehouse terminal](cli/README.md), and [open-source release preparation](docs/open-source-release.md). The repository remains private; packages and images have not been published.

The development terminal hardens concurrent recovery with exclusive checkpoint positions and preserves existing 0.2.0 journals. See [upgrade and recovery instructions](cli/README.md#recovery). This follow-up has separate targeted checks; the dated release report above applies to its original frozen 0.2.0 source.

## Run

Requires Rust 1.90+, Node.js 22.12+, npm and Postgres 12+. Use an isolated development database.

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

- Native [MCP server](docs/mcp.md) at `/mcp` with 70 administrator tools, authenticated discovery, tenant/role enforcement, typed warehouse commands and original-request recovery through the same journal as REST.
- Create and select multiple facilities with independent stock, catalog, work and versions. Admin creation uses the same immutable command journal; pending warehouse/native saves hold switching, and reload restores their original warehouse. The paginated picker keeps the active facility visible on mobile.
- Catalog products and receiving/storage locations; configure facility name, code, city and IANA timezone from Locations.
- [Lot expiry and quarantine controls](docs/api.md#lot-expiry-quarantine-and-availability) with server-owned cutoff checks, optional required expiry labels, earliest-expiry allocation, independent reasoned holds and protected physical recovery. Inventory distinguishes physical/reserved custody from usable stock.
- [Cross-facility lot tracing](docs/api.md#cross-facility-lot-trace) through physical positions, reservations, receipts, packing/handoffs, transfers and original policy evidence, with paginated source reviews and explicitly partial exports.
- Expected receipts with immutable line IDs, explicit lot IDs, partial receiving, over-receipt rejection and reasoned cancellation of unreceived remainders.
- Scanner receiving and picking with bin → SKU → lot validation, exact receipt/allocation ownership, captured warehouse versions and explicit quantity confirmation. Uncertain saves retain the original request across reloads.
- Putaway and bin moves of unreserved stock. Receiving stock becomes allocatable only after it reaches storage.
- Physical inter-facility transfers with immutable carrier handoff, original per-line/SKU/lot ownership, goods in transit and separately confirmed partial destination arrivals. Both actions recover through the original durable command journal.
- [Confirmed transfer loss and capped reversal](docs/api.md#confirmed-transfer-loss-and-reversal), with destination administrator review, original incident evidence and paginated immutable history. Neither operation credits stock; actual located goods require a separate arrival.
- Pick-face replenishment plans with minimum/target availability, optional reserve zones, reservation protection, shared-source accounting and explicit physical confirmation through the normal request journal.
- Immutable original handoff evidence protects shipped quantities, lot ownership and carrier/source metadata; returns and conditional OMS writeback share that verified source.
- Customer return receipts tied to the exact original shipped order line and lot. Authorizations are capped across all retained RMAs; returned stock enters receiving before reviewed putaway, without changing the original shipment or issuing a refund.
- OMS-linked or manual fulfillment orders, with immutable line IDs and independent reservations even when several lines use the same SKU.
- Full-order or available-stock allocation, retained backorders, partial line picks, protected staging and immutable per-package packing/unpacking/carrier handoffs. Early customer returns use only actually shipped units.
- Close a partly shipped order by cancelling all unshipped demand after unpacking and physically returning its staged stock. Original ordered quantities, actual handoffs and return allowances remain retained; cancellation releases only that order’s unpicked reservations.
- Short-pick recovery: physically return an exact line's staged units, release allocations, recount, resolve the exception and allocate again. Returning packed units invalidates packing and requires another pack before shipping.
- Cancellation and reservation release before picking; blocking order exceptions and retained resolution evidence.
- Administrator physical counts with reasons, stale-version protection and rejection below reserved balances.
- Physical movement ledger, independent balance/reservation reconstruction, searchable views and CSV exports.
- Server-owned tenant/operator/role identity, stable request IDs, immutable command receipts and browser recovery after uncertain saves. Database-owned resource revisions let independent work proceed across warehouse version changes; relevant stock/work/exception changes still fence stale reviews, including balances changed and restored. Exact committed retries avoid the warehouse write lock. Definitive rejection can safely retire a request that never committed; ambiguous outcomes remain held.
- Durable OMS events and TMS readiness/handoff events, leased delivery, immutable receiver fingerprints, crash-counted retry budgets, retained delivery attempts, fenced late responses, dead letters and audited requeue.
- Source-derived Sync order import and Freight planned-load adapters, with strict tenant/source identities, exact whole-unit quantities, explicit transport facts and retained readiness evidence. Freight acknowledgments must retain an eligible canonical source as well as exact source lines.
- Opt-in conditional OMS fulfillment after physical handoff, with frozen source revisions, receiver capability checks, immutable accepted/rejected receipts and exact uncertain-request recovery. The [companion OMS receiver](integrations/stateset-sync-server/README.md) must be deployed and qualified first; the legacy endpoint stays held.
- Normalized operational rows, database-enforced movement/reservation/receipt proofs, and append-only movement/activity history with cursor pagination and archived record details.
- Tenant-wide Operations view for delivery queues, uncertain native requests and warehouse exceptions, with bounded consistent snapshots, explicit stale/offline status, current-evidence exports and administrator Prometheus metrics with tested alert policies.

Quantities are whole **each** units. Partial receipts, picks, shipments and physical customer returns are supported. Reviewed whole case, inner-pack and pallet conversions retain each quantities and the original pack definition through saves and recovery; see [pack units](docs/api.md#reviewed-pack-units). Serial command admission, literal-label collection/recovery and source-checked identity reviews are locally software qualified; the [serial contract](docs/api.md#serial-command-admission) and [evidence](docs/evidence.md) describe the measured scope and remaining cost/acceptance work. Fractional measured units and carrier purchasing are not implemented. Lot expiry requires an exact time with a UTC offset; date-only label interpretation needs an explicitly reviewed facility policy. Quarantine is warehouse-local and does not implement a tenant-wide recall. Partial-order cancellation closes the entire unshipped remainder; it does not purchase transport, issue refunds or cancel the upstream commercial order automatically. Multiple physical handoffs remain held in the current single-event native OMS connection; they never create an invented aggregate carrier or fulfillment event. Replenishment rules are reviewed per plan rather than a scheduled background policy. Return authorization consumes allowance across retained RMAs. Cancelling an unreceived remainder releases only that exact line/lot quantity; received returns remain consumed. Refunds are not implemented. Do not use the demo as operational inventory.

## Real workspaces

Set `WMS_DEMO=false`. For operator sign-in, configure [WorkOS AuthKit](docs/authentication.md). For service credentials or API-key mode, configure `WMS_API_KEYS_JSON` in `.env` or your secret manager:

```dotenv
WMS_API_KEYS_JSON={"replace-with-a-random-secret-of-at-least-32-characters":{"tenant":"your-org","actor":"operator:alice","role":"admin"}}
```

Real tenants start with an empty `MAIN` warehouse, a receiving bin and staging bin. Use the warehouse button in the top bar to select a facility; admins can choose **New warehouse** to create another empty facility. Configure its name, code, city and IANA timezone, then create products/storage locations and record actual receipts or opening physical counts. Each facility has independent inventory and work; selection is remembered per tenant/operator in the current browser session. Roles apply across that tenant's facilities. Use **Facility transfers** for physical dispatch and destination arrivals. Configure the same whole-each SKUs at both facilities before dispatch; in-transit goods cannot be allocated.

Roles are `viewer`, `operator`, `admin` and `oms`. Operators execute warehouse work; admins additionally manage facilities/catalog/locations and count adjustments. OMS credentials may create source-linked orders, cancel them before physical picking, or close their unshipped remainder after a partial handoff and completed physical recovery. Use individual keys for attributable actions. Browser credentials are session-scoped; sign-out removes the local credential. Server revocation is by removing/rotating the configured key and restarting instances. Optional [WorkOS AuthKit sign-in](docs/authentication.md) maps verified organization memberships to explicit WMS tenants and roles, with signed access tokens and browser refresh/sign-out. It requires actual WorkOS configuration and deployed qualification; it is not enabled by default. API keys remain available for service identities.

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

The live suite includes persistence, concurrency, HTTP warehouse lifecycle, native API recovery, conditional OMS receiver races and database guard tests. Explicit invocation fails without a database; default `cargo test` marks these tests ignored. The default check runs domain, authorization, native contract and offline browser tests. CI is configured to run both live database and live browser checks; the previous hosted run was blocked by organization billing/spending limits. Check the new commit’s run independently.

For disposable socket-only PostgreSQL verification, `bash scripts/local-postgres.sh` initializes an isolated temporary cluster and runs the database suites. `bash scripts/local-postgres.sh bash scripts/check.sh --postgres` runs the complete local check against that cluster. Where sockets are unavailable, `python3 scripts/check-sql.py` applies all migrations in an isolated single-user backend, type-checks runtime SQL, executes independent reconciliation, and verifies receipt/reservation constraints, append-only history and fault-injected rollback. That check does not establish SQLx bindings or concurrent client behavior.

Run the [complete software release gate](docs/release-qualification.md) with Docker, matching PostgreSQL tools and the browser installed:

```bash
bash scripts/local-postgres.sh bash scripts/qualify-release.sh
```

CI invokes this same gate. It includes actual archive restoration, live browser execution, monitoring rules, hardened production-container checks and sustained HTTP across two API replicas with forced API/PostgreSQL crashes. Reports bind the exact source, migration and immutable image identities; failed or missing stages cannot pass. Only the sanitized `publish/` artifact directory is suitable for uploading. [Native deployment preflight](docs/native-acceptance.md) and [signed facility acceptance](docs/facility-acceptance.md) require separately supplied deployment and physical observations.

The current facility-evidence profile requires a signed plan approved before work, independent operator/controller signatures and captured sources for 23 workflows. It binds the reviewed software manifest and immutable image, verifies original command/receipt and custody relationships, and recomputes peak/recovery targets. `scripts/facility-template.py` creates private unperformed templates; no local test supplies actual hardware or physical acceptance.

## Deployment boundaries

The API serves static dashboard assets, `/health` liveness and `/ready` database readiness. Run migrations with a database identity allowed to create the WMS schema on startup. Terminate TLS at your ingress, use strong individual keys, back up Postgres, and qualify restore and physical reconciliation before live use. The container runs as UID 10001 and checks database readiness. Build with `docker build -t stateset-warehouse .`; the Compose `app` profile requires real credentials. `bash scripts/container-smoke.sh` builds and runs the actual image with an isolated PostgreSQL container, a read-only filesystem, no capabilities and no new privileges.

Persistence stores current products, locations, stock, receipt lines, orders and allocation ownership in normalized PostgreSQL rows. Scoped physical commands load their SKUs and owning work, deriving other orders' source/staging holds directly from allocation rows; full snapshots and independent historical proofs still cover the warehouse. Operational reads serialize typed state directly and compute their physical/reservation proof once; structural validation uses catalog/location indexes. Movements and activity are append-only records; bounded operational views and keyset history pages keep growing history out of each command's write set. The former 8 MiB warehouse aggregate limit is removed. Startup applies tracked migrations and verifies legacy evidence before importing an older snapshot.

Writes serialize within each warehouse. The [workload harness and measured profiles](docs/performance.md) expose the cost of active work rather than claiming production capacity. The [restore rehearsal](docs/operations.md) has exercised an actual archive round-trip, complete table hashes, full reconciliation and recovered request journals. Real physical counts, actual scanner/device acceptance and deployed cross-service tenant mappings still require facility evidence. See [the evidence report](docs/evidence.md) for executed checks and limits.

## License

Available under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. See [third-party notices](THIRD_PARTY_NOTICES.md), [contributing](CONTRIBUTING.md) and [private security reporting](SECURITY.md).
