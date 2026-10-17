# Implementation evidence

Recorded 2026-10-17. This report separates implemented behavior, executed checks and unverified operational claims.

## Executed checks

| Check | Observed result | What it establishes |
|---|---|---|
| `cargo fmt --all -- --check` | Passed | Rust formatting |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed | All Rust targets compile without Clippy warnings |
| `cargo test --locked` | 15 passed; 11 explicitly ignored Postgres tests | Domain behavior and in-process HTTP authorization; NOT live persistence |
| Two `proptest` cases within the domain suite | 128 generated cases each, passed | Random move/count sequences conserve movement totals; partial picks preserve physical custody and reservations |
| `npm --prefix dashboard run build` | Passed | Strict TypeScript check and production Vite build |
| Playwright `offline-contract` project | 5 passed | Built UI drives the actual Rust domain through intercepted requests; desktop/mobile layout, receive/putaway, fulfillment/handoff, uncertain-save recovery after reload, and stale-count rejection |
| `python3 scripts/check-sql.py` | Schema and all 22 runtime statements parsed/type-checked by Postgres 12.22; invalid reserved balance rejected | Actual SQL grammar, parameter inference and a database balance backstop in an isolated single-user backend |
| `docker compose config --quiet` | Passed | Compose configuration resolves; NOT an image/deployment test |
| `bash -n scripts/dev.sh scripts/check.sh` | Passed | Shell syntax |

The passing browser suite generated [desktop](screenshots/desktop.png) and [mobile](screenshots/mobile.png) screenshots. The offline harness serves the built assets through Playwright interception and calls `examples/domain_driver.rs` for actual domain transitions. It does not substitute for the separate live browser/Postgres test.

## Cases with concrete expected results

- Receive ten units, allocate three, partially pick one: source becomes 9 on-hand/2 reserved; staging becomes 1 on-hand/1 reserved; warehouse physical total stays ten. After picking, packing and shipping all three, the physical total is seven and no reservation remains for that order.
- Two same-SKU lines of five and one units: picking the one-unit line leaves the five-unit line's reservation untouched. This addresses the failure documented in iCommerce migration 080.
- An allocation whose second line lacks stock fails without retaining its first line's reservation.
- Counts cannot reduce on-hand below reserved units; transfers cannot consume reserved units; resolving an exception does not create missing inventory.
- Browser fulfillment of synthetic `SO-1041` reduces warehouse physical stock from 1,577 to 1,553 only when its 24-unit handoff is recorded.
- An accepted save whose response is deliberately dropped is recovered after browser reload without advancing the warehouse version again.
- A count form opened at an older version preserves entered evidence but cannot overwrite a concurrent operator's updated balance.

## Required checks not executed in this environment

The host sandbox rejected local socket creation (`Operation not permitted`), preventing Postgres from listening on localhost. Its approval policy also rejected the request to run that database outside the sandbox. No live Postgres test is reported as passing.

Eleven compiled integration tests in `tests/postgres.rs` cover simultaneous last-stock allocations, identical request races, raw database corruption rejection, duplicate receiving, reconnect/replay recovery, changed payload/actor conflicts, stale counts/competing transfers, tenant isolation, transaction failure on the final outbox insert, expired delivery leases, retry identity and dead letters. Run them with:

```bash
WMS_TEST_DATABASE_URL=postgres://warehouse:warehouse@127.0.0.1:5548/warehouse_test \
  cargo test --locked --test postgres -- --ignored
```

Invoking these tests without an isolated database fails. The normal unit command explicitly marks them ignored, never silently passes them. `.github/workflows/check.yml` provisions Postgres and requires this suite plus the separate `live-postgres` browser project. The workflow has been written but has not run remotely in this session.

The Docker image was not built here. No deployment, external receiver, existing native OMS/TMS consumer, SSO provider, live EDI exchange or production workload was verified.

## Physical and operational acceptance still required

All demo records are synthetic. No production scanner events, physical counts, packing records, facility layout or operator observations were provided. Software tests cannot establish agreement with physical stock.

Before a real facility uses this version, reconcile a known physical count, execute a receiving-to-handoff case with the actual operators/devices, replay representative exceptions with known outcomes, verify OMS/TMS consumer deduplication/version handling, and exercise database backup/restore. Record outcomes against facility-owned acceptance cases.

This version handles whole each units, explicit lots, complete-order allocation/shipping, partial receipts and partial picks. It does not qualify fractional/case conversions, expiry/serial policies, returns, partial shipments, or high-volume operation. Persistence has an enforced 8 MiB aggregate limit and serializes writes within each warehouse. These limits are documented rather than hidden behind synthetic performance claims.
