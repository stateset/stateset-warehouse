# Warehouse execution boundaries

## Source evidence and ownership

`/home/dom/stateset-sync-server` is the OMS. It owns commercial orders and orchestration. `src/domain/warehouse_os.rs` and `src/services/warehouseos.rs` show its existing WarehouseOS adapter; this is a third-party contract, not an existing native StateSet WMS connector.

`/home/dom/stateset-warehouse` is the WMS. It owns the physical warehouse ledger, location balances, reservations, receipt execution, and pick/pack/handoff evidence. Its balances are authoritative for warehouses explicitly managed here. Do not also mutate those warehouses in iCommerce or WarehouseOS. An OMS copy of availability must be a projection, not an independent writer.

`/home/dom/stateset-freight` is the TMS. `src/integrations.rs` and `src/domain.rs` implement transportation, carrier procurement, stops, tracking and Sync writeback. This WMS records the physical handoff; a freight dispatch or delivery event does not authorize it to ship inventory a second time.

`/home/dom/stateset-edi/src/transactions/receipt944.rs`, `adjustment947.rs`, and the warehouse lifecycle in `src/lifecycle.rs` establish document responsibilities. EDI owns X12 parsing/transport. This repository emits warehouse events and does not implement another EDI parser.

`/home/dom/next-temporal-rs/crates/engine-workflows` supplies existing durable orchestration. The WMS does not need a Temporal worker to commit a local stock movement. Cross-service orchestration can consume its durable events.

`/home/dom/stateset-app/package.json` establishes React and TypeScript UI conventions; the newer EDI/Sync/engine dashboards establish React/Vite. The WMS uses Rust 1.90, Axum 0.8, Tokio, SQLx/Postgres and React/TypeScript/Vite.

iCommerce's `crates/stateset-core/src/models/warehouse.rs`, `stateset-db/src/postgres/warehouse.rs`, and migrations 080/083/092 establish bin/lot identity, exact quantities, order-line reservation identity and balance protection. Its `postgres_inventory_oversell.rs`, `postgres_move_inventory_concurrency.rs`, and shipment atomicity tests are concrete regression inputs. We reuse these concepts rather than depend on the entire embedded commerce engine, which would introduce a second order/inventory authority and its schema lifecycle.

## Inventory contract

Quantities are whole **each** units, bounded to 1,000,000 per operation and 1,000,000,000 per bin/lot. Fractional/case conversion is deliberately unsupported and rejected, not rounded. Stock identity is `(warehouse, location, SKU, lot)`. Lot is a required explicit string (`UNTRACKED` is allowed); expiry and serial control are not claimed.

Physical stock includes receiving, storage and staging. Receiving increases physical stock into a receiving bin; putaway transfers it into storage without changing the warehouse total. Allocation reserves only available storage stock, in stable location/lot order; this is not a claim of FEFO. Each allocation is keyed to an immutable order-line ID. Picking moves reserved units into the staging bin and preserves their reservation. Shipping removes those staged units only after packing, with a recorded carrier and handoff reference. TMS remains responsible for transportation after handoff.

Each transition checks quantities and legal state. Partial receipts and partial line picks persist; packing requires all ordered units picked, so partial shipments are not supported in this version. Cancellation releases reservations only before picking. A short pick can open a blocking exception. Recovery returns physically verified staged units to their original line/bin/lot, releases allocations only after every staged unit is returned, records a physical count, resolves the exception and allocates again. Returning packed units invalidates packing. Recovery remains available while an exception blocks forward execution; resolving an exception alone never changes quantities.

Every stock change creates a movement. Independent reconciliation reconstructs each physical balance from movement endpoints and reservations from exact line allocations. Opening stock requires a receipt or a reasoned physical count; it is never an invisible initial balance. Every receipt movement also identifies the exact receipt and line, so another receipt for the same SKU/lot cannot borrow its evidence. Counts reject stale versions and observed quantities below existing reservations.

## Persistence and concurrency

The warehouse head contains facility metadata and a version. Products, locations, stock, orders, order lines, allocation ownership, receipts, receipt lines and exceptions occupy indexed relational tables. Entity JSON preserves the API representation while normalized execution columns enforce quantities, identities and foreign keys. Movements and activity are append-only rows. The command path writes changed entities and new evidence; it does not rewrite accumulated warehouse history.

A row lock on the warehouse head serializes that warehouse's writes across API processes. The changed operational rows, movement/reservation evidence, event, actor-bound command receipt and destination outbox intents commit together. Deferred PostgreSQL constraints reject stock/ledger, reservation/allocation and received/exact-line evidence mismatches at commit. Immutable history cannot be updated or deleted. A final outbox failure rolls the entire command back. Stale versions fail; exact successful request retries return their original result even after other work advances the warehouse.

Operational reads use a consistent transaction snapshot. They include active work, bounded recently completed work and at most 100 recent movement/activity records, with the receipt evidence needed to validate those movements. Older records remain accessible through keyset history pages and authoritative order/receipt details. The full reconciliation endpoint recomputes balances from every immutable movement and exact allocation, independently of the operational ledger caches; it also compares received quantities with all exact receipt-line movements.

Tracked migrations import validated legacy snapshots and fence old aggregate writers while retaining the original snapshot. Ambiguous legacy receipt evidence blocks migration rather than inventing a line association. The former 8 MiB aggregate ceiling is removed; a retained-history test exceeds it without blocking subsequent commands. Writes still serialize per warehouse, active-work size still matters, and production workload/capacity qualification remains outstanding.

All production data is tenant-scoped. API credentials map server-side to tenant, actor and role; client-supplied tenant/actor values are not trusted. Operators execute work; admins also manage catalog/locations and adjustments; OMS credentials submit/cancel fulfillment requests only. Demo mode is explicit, uses a separate tenant, synthetic records, and Postgres persistence. Real mode starts empty and requires strong configured keys.

## Cross-service delivery

Schema-version 2 event envelopes carry event identity, tenant, warehouse, actor, time, command and committed version. `inventory_changes` contains absolute snapshots of only the bin/lot balances changed by that command, each tagged with its version. It is not a complete warehouse projection. Consumers deduplicate event IDs and version each bin/lot independently: an older event may contain a change to a different bin that a newer event did not include. Per-order business effects also require their own retained identity and reconciliation.

Every command queues an optional OMS event; packed and shipped orders also queue optional TMS events. A leased worker reuses the event ID as its idempotency key and retains attempt outcomes, expired-lease uncertainty, bounded retries and dead letters. Generic webhook destinations require compatible consumers. They do not reinterpret an existing Sync or Freight endpoint. HTTP acceptance does not establish downstream execution. EDI 944/945/947 generation remains the EDI gateway's responsibility.

Native adapters separately use Sync's tenant-scoped order API and Freight's `/v1/sync/import` API, grounded in their checked-in implementations. Imports validate a reviewed source revision, source status, exact quantities and stable line identities. A complete native source has one owner across the WMS tenant's warehouses, including commands using the native source prefix; warehouse-assigned splitting needs an explicit OMS assignment contract. Original response envelopes and translated commands are frozen in an actor-bound journal before warehouse execution. A retry can recover the normal command receipt after a crash between the warehouse commit and journal completion.

Transportation requires a packed order, no blocking exception, a verified source and an explicit operator-supplied transport plan. The request and source evidence are retained before remote HTTP. A lease permits one sender; retries reuse the same frozen plan and remote idempotency key, and preserve uncertain outcomes after a timeout or crash. No database lock spans the HTTP call. Freight acceptance means a planned load, not pickup, dispatch or delivery. Native request identities and completed receipts are protected from mutation by database triggers.

Sync's current fulfillment endpoint unconditionally updates commercial order status without an expected revision or terminal-state fence. The WMS therefore exports a concrete source-checked proposal and retained handoff evidence while holding the unsafe POST. Automatic writeback requires the upstream OMS to enforce those conditions atomically. See [the native contracts](native-integrations.md) for actual routes, wire formats, tenant mapping requirements and this unresolved upstream boundary.

## Evidence and assumptions

No production scans, physical counts, facility layout, workload measurements or operator observations were provided. Demo records and test fixtures are synthetic. Automated tests establish software invariants under the exercised conditions; they do not establish agreement with a physical warehouse. See `docs/evidence.md` for executed checks and operational acceptance still required.
