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

Each transition checks quantities and legal state. Partial receipts and partial line picks persist; packing requires all ordered units picked, so partial shipments are not supported in this version. Cancellation releases reservations only before picking. Short picks open blocking exceptions; resolving an exception requires a note and does not alter quantities. Operators must correct the stock or execution evidence before continuing. Counts specify observed physical quantity; a stale warehouse version or a count below reserved stock is rejected.

Every stock change creates a movement. Independent reconciliation reconstructs each balance from movement endpoints and independently reconstructs reserved quantities from line allocations. Opening stock is recorded as receipt movements; it is never an invisible initial balance.

## Persistence and concurrency

The initial implementation stores each warehouse as a versioned JSONB aggregate. A Postgres row lock serializes that warehouse's writes across API processes. Database checks independently reject negative/fractional balances, reservations exceeding physical stock, duplicate bin/lot identities, and mismatched aggregate versions/warehouse IDs. The state, immutable event, idempotency receipt and destination outbox records commit in one transaction. Stale versions fail; successful request replays return the original receipt even if the workspace has advanced. Reusing a request ID for another actor or payload fails. Read responses carry reconciliation from the same state snapshot, so the UI never attaches proof from a different warehouse version.

This intentionally favors a small, auditable transaction boundary over premature distributed stock writes. It has a real limit: reads and writes include the warehouse aggregate and its history. It is not qualified for large warehouses or sustained high write rates. The API bounds aggregate size; an indexed relational ledger/projection migration and measured workload qualification are required before those deployments. Histories are never silently truncated to fit.

All production data is tenant-scoped. API credentials map server-side to tenant, actor and role; client-supplied tenant/actor values are not trusted. Operators execute work; admins also manage catalog/locations and adjustments; OMS credentials submit/cancel fulfillment requests only. Demo mode is explicit, uses a separate tenant, synthetic records, and Postgres persistence. Real mode starts empty and requires strong configured keys.

## Cross-service delivery

Versioned event envelopes carry event ID, tenant, warehouse ID, order/source references, actor, time and committed version. Every command queues an OMS event; packed and shipped orders also queue a TMS event. A leased worker uses the same event ID as the receiver idempotency key, bounded HTTP timeouts, exponential backoff and dead letters. Unconfigured destinations retain pending events. Delivery means HTTP acceptance, not downstream business completion. Receivers MUST deduplicate event IDs and MUST NOT treat this as exactly-once networking.

The receiver URLs are **proposed integration contracts**, not verified existing native OMS/TMS routes. No sibling project is modified or silently connected. Actual end-to-end integration remains unverified until receiver adapters and their consumer tests exist. Events can be inspected/exported and dead letters explicitly retried. EDI 944/945/947 generation remains the EDI gateway's job.

## Evidence and assumptions

No production scans, physical counts, facility layout, workload measurements or operator observations were provided. Demo records and test fixtures are synthetic. Automated tests establish software invariants under the exercised conditions; they do not establish agreement with a physical warehouse. See `docs/evidence.md` for executed checks and operational acceptance still required.
