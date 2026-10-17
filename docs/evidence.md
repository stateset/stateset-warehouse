# Implementation evidence

Recorded 2026-10-17. The executed suites cover 70 Rust cases (44 without a database and 26 against real PostgreSQL) and 13 browser cases (12 offline contracts and one live service/database lifecycle). Results below distinguish those software checks from deployed integration and physical warehouse acceptance.

## Executed checks

| Check | Observed result | What it establishes |
|---|---|---|
| `bash scripts/check.sh --postgres` | Passed with exit 0 | Consolidated formatting, Clippy, 44 default Rust cases, 26 real database cases, dashboard production build and 12 offline browser contracts |
| `cargo test --locked --test domain --test http_auth --test native_contracts` | 21 domain, 3 authorization and 15 native contract tests passed | Warehouse invariants and recovery, tenant identity, source-derived native payloads, recipient authority pins and actual local HTTP contracts |
| Property tests within the domain suite | Passed | Generated move/count and partial-pick sequences conserve physical custody and reservation ownership |
| `cargo test --locked --test postgres --test persistence_acceptance -- --ignored --test-threads=1` | 15 passed against real PostgreSQL on loopback | Concurrent allocation and request replay, transactional rollback, reconnection, tenant isolation, delivery recovery, HTTP warehouse execution and retained-history behavior |
| `cargo test --locked --test persistence_guards -- --ignored --test-threads=1` | 4 passed against real PostgreSQL | Completed business identity conflicts, legacy writer fencing, database corruption/history guards and full historical proof independent of operational caches |
| Native API suite | 5 default and 7 real PostgreSQL cases passed | Authorization/input guards, durable import/transport recovery, exact receipt ownership, immutable native evidence, tenant-wide source ownership and the native HTTP/PostgreSQL path |
| Playwright `offline-contract` | 8 warehouse and 4 native operator UI cases passed | Warehouse flows, archived proof, reviewed source/transport plans, independent form versions and original request recovery after reload or denied retries |
| Playwright `live-postgres` | Full browser/HTTP/SQLx/PostgreSQL lifecycle passed in 20.8 seconds | Receiving, same-SKU short-pick recovery, physical shortage/replenishment, handoff, reload and full historical reconciliation against the actual service |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed | Current Rust targets compile without Clippy warnings |
| `npm --prefix dashboard run build` | Passed | Strict TypeScript checks and a production dashboard build |
| `python3 scripts/check-sql.py` | 4 migrations and 82 runtime statements passed with actual PostgreSQL 12.22 | Ordered migrations and runtime SQL types, actual independent reconciliation, exact receipt evidence, reservation proofs, immutable history and fault-injected rollback |

The live database suites executed against an isolated local PostgreSQL 12.22 instance. These results are separate from the offline browser harness and socket-free SQL checks.

The offline harness serves built assets through Playwright interception and calls `examples/domain_driver.rs`. Its [desktop](screenshots/desktop.png) and [mobile](screenshots/mobile.png) screenshots are review artifacts. The separate `live-postgres` browser test uses the actual service and database. Native API database cases are included in `scripts/check.sh --postgres`; their authorization/input guards run in the default check. The consolidated local check script completed successfully, and the socket-free SQL proof also passed independently.

## Concrete acceptance results

- The concurrent last-stock test allows one allocation to reserve ten available units; competing attempts cannot oversell. An identical-request race records one command, event and delivery intent.
- Repeated SKU lines retain independent reservations and staged quantities. Returning one line's picked units cannot release another line's staged stock.
- A partial pick can be blocked by an exception, physically returned, released, recounted, resolved and allocated again. Returning packed units invalidates packing. Shipping still requires a complete repack and physical handoff evidence.
- The HTTP lifecycle receives 12 units across two same-SKU receipt lines, deduplicates a repeated receipt request, recovers a short pick, records a four-unit physical shortage, then ships eight. Final physical and reserved stock are both zero and independent reconciliation passes.
- Thirty thousand retained audit records exceed the former 8 MiB aggregate ceiling. Commands remain operable, the operational history view stays bounded, all original evidence remains stored and cursor pages expose older evidence without overlap.
- Completing 25 receipts keeps the oldest receipt accessible and its exact line evidence reconciled beyond the completed-work dashboard window.
- A failed final outbox write rolls back stock, movement ledger, event and saved command receipt. Reconnecting and retrying a committed request returns its original result without another physical movement.
- The independent SQL fixture receives ten units, reserves seven across two same-SKU order lines and picks two. Storage is eight on-hand/five reserved; staging is two on-hand/two reserved. Direct physical, reservation and receipt-evidence corruption is rejected, as are movement updates and audit deletion.
- Native adapter contracts reject fractional units, mismatched source identities, unsafe statuses, redirects and oversized responses. Freight import retries retain a stable prepared request. The OMS adapter makes no unsafe fulfillment POST.
- Competing warehouses cannot import the same complete native OMS source, including an attempt through an ordinary native-prefix command. A retained transport intent pins the Freight endpoint/tenant; every POST checks actual `/v1/access` tenant identity and `operations.write`. Changing authority cannot redirect the pending request, while a secret can rotate within its original tenant.

## Persistence and integration boundaries

Current stock, products, locations, receipt lines, orders and allocation ownership use normalized PostgreSQL rows. Database triggers independently enforce physical movement, reservation and exact receipt-line balances. Movement/activity histories are append-only and paginated. Operational reads include active work, bounded completed work and bounded recent history. The former 8 MiB warehouse JSON aggregate limit is removed; writes still serialize within a warehouse.

The [native integration report](native-integrations.md) identifies the actual Sync and Freight routes, authentication, response envelopes and unresolved contract gaps. Sync remains the commercial order authority; WMS owns physical inventory; Freight creates and executes transportation plans. Source-derived contract tests do not establish deployed credentials or the two applications' tenant mappings.

Native OMS fulfillment writeback is held because the upstream endpoint unconditionally changes order status without a source revision or terminal-state fence. The WMS retains the handoff evidence and concrete proposal. Native Freight import creates a planned load; it does not establish carrier acceptance, pickup or delivery. Generic warehouse event webhooks remain a separate producer contract requiring compatible consumers.

## Reproduce and outstanding validation

```bash
bash scripts/check.sh
WMS_TEST_DATABASE_URL=postgres://warehouse:warehouse@127.0.0.1:5548/warehouse_test \
  bash scripts/check.sh --postgres
python3 scripts/check-sql.py
```

Use a separate database whose name contains `test`. Explicit database test invocation fails when the database is missing; default unit execution marks those tests ignored. The socket-free SQL check runs the tracked [relational proof](../scripts/relational-proof.sql) and [fault proof](../scripts/fault-proof.sql) and requires explicit assertion markers; it does not prove SQLx bindings or concurrent client behavior.

CI is configured for PostgreSQL, the full check script and live browser/API execution. The GitHub workflow remains blocked before execution by organization billing/spending limits. The Docker image, production deployment, external service credentials, EDI exchange and SSO integration are unverified. The [operations runbook](operations.md) supplies the required restore rehearsal; those backup/restore commands have not yet been qualified here.

All fixtures are synthetic. No production scanner events, physical counts, facility layout or operator observations were supplied. Before live operation, record facility-owned receiving-to-handoff and exception cases, reconcile actual physical stock, verify the deployed OMS/TMS tenant mapping and consumer recovery, and qualify backup/restore. A reconnect test does not establish recovery from a lost database.

Supported scope remains whole each units, explicit lots, full-order allocation/shipping, partial receipts and partial picks. Fractional/case conversions, expiry/serial control, returns, partial shipments and production throughput are not qualified by this evidence.
