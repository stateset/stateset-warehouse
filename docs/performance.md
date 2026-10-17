# Workload qualification

The [workload executable](../examples/workload.rs) measures current operational data and concurrent operators against real PostgreSQL. A historical-audit retention test alone cannot establish warehouse capacity: every command still loads the current catalog, bin-lot balances, open orders and allocation ownership. Writes serialize within each warehouse. This harness makes that cost, and the resulting version conflicts, visible before selecting a facility workload.

## Reproduce

Use an isolated database whose name contains `test`; the harness refuses other database names. Each run creates unique synthetic tenants and retains their evidence for review. It never clears an existing tenant. The shared Store connection pool has eight connections. Use a release build and the same database configuration and hardware for comparisons.

```bash
WMS_TEST_DATABASE_URL=postgres://warehouse:warehouse@127.0.0.1:5548/warehouse_test \
  WMS_WORKLOAD_REPORT=/tmp/wms-workload.json \
  cargo run --locked --release --example workload
```

The disposable PostgreSQL runner can supply the database instead:

```bash
bash scripts/local-postgres.sh \
  env WMS_WORKLOAD_REPORT=/tmp/wms-workload.json \
  cargo run --locked --release --example workload
```

| Variable | Default | Meaning |
|---|---:|---|
| `WMS_WORKLOAD_PRODUCTS` | 100 | Operational products; one additional product isolates the last-unit race |
| `WMS_WORKLOAD_BINS` | 20 | Storage bins; one additional bin isolates a competing destination |
| `WMS_WORKLOAD_LOTS` | 2 | Lot balances per operational product |
| `WMS_WORKLOAD_OPEN_ORDERS` | 100 | Open single-line orders; half are allocated and half pending |
| `WMS_WORKLOAD_CLIENTS` | 4 | Concurrent operator identities, each assigned a separate SKU |
| `WMS_WORKLOAD_ACTIONS` | 10 | Physical bin transfers per client |
| `WMS_WORKLOAD_REPORT` | stdout only | Optional JSON artifact path |
| `WMS_WORKLOAD_MAX_P95_MS` | unset | Fail if end-to-end action p95 exceeds this measured limit |
| `WMS_WORKLOAD_MIN_ACTIONS_PER_SECOND` | unset | Fail if committed physical-action throughput falls below this limit |

Each product has one opening stock bucket per lot. Moving creates a second bin bucket for a client's first lot. Zero balances remain evidence-bearing stock keys. Reported actual balance counts therefore include receiving, race and destination buckets. Configured product count must be at least client count, bins at least two, and action count positive. The executable validates bounds and threshold values before connecting.

For a larger operational profile, set products to `500`, bins to `100`, lots to `2`, open orders to `500` and actions to `25`. This seeds 1,000 stock buckets and 500 active orders through real commands rather than direct balance inserts. Seeding is measured separately and can take substantial time because it exercises the same correctness checks as normal execution. Do not compare a debug result with a release result.

## What is measured

All product creation, opening counts, receiving, putaway, order creation, allocation and test moves use `Store::execute`. Initial physical counts are disclosed synthetic fixtures. The only direct provisioning inserts create an empty second warehouse header and its standard receiving/staging locations, because public provisioning currently creates `wh-main` only. All operational changes in that second warehouse still use Store commands. SQL reads independently count the resulting records.

Concurrent clients alternate one-unit moves between two storage bins on distinct SKUs. Each action fetches current state, submits a versioned request and replays the exact committed request once, simulating a lost response. A definite stale-version rejection leads to another state read and a new request ID for the reviewed, still-valid lane move. A transient database error retains the original request ID, payload and expected version. It never becomes another physical action merely because a network retry occurred.

The JSON records:

- Actual seed sizes, active allocated orders and setup duration.
- Committed physical actions per second, excluding seed/setup and the separate race. The timed workload includes refreshes, stale rejections and successful frozen replays.
- End-to-end action p50/p95/p99/max, including refresh and rejected attempts up to commitment. The subsequent deliberate replay has its own latency distribution.
- State refresh, command attempt and frozen replay latency distributions. Command attempt samples include rejected stale requests, which are counted separately from physical actions.
- Stale-version rejections, transient 503 attempts, successful frozen replays and unexpected errors. An unexpected error fails the run rather than being hidden as successful throughput.
- Process high-water memory before and after clients, PostgreSQL version and available CPU parallelism. The memory number covers the Rust process, excludes PostgreSQL and is not proof of constant memory.

Optional thresholds have no built-in production defaults. Derive them from a facility's expected scan rates, peak concurrency, database placement and allowed operator wait. A failed configured threshold writes the report and exits nonzero. Run each candidate profile several times on an otherwise quiet host and retain every result, including slower runs.

## Correctness gates

The executable fails unless all of these hold:

1. Two requests competing for the final physical unit produce exactly one committed move. The loser receives a version conflict; after refreshing, it receives an insufficient-stock business rejection. Replaying the winner does not move another unit.
2. Every concurrent disjoint action eventually commits once. Frozen replays return the original result. Rejected attempts do not count as new work.
3. Command receipts, events, activity and warehouse version exactly equal seed commands plus committed actions. Physical movement count equals the independently calculated opening counts, receive/putaway, race and workload actions.
4. Seven received units have exactly seven units of linked receipt-line movement evidence after a repeated receive request.
5. Total physical custody and reservation ownership remain unchanged by moves. Open operational orders are retained in full. A full SQL ledger/receipt/allocation reconciliation passes after the workload.
6. Another tenant and another warehouse cannot read the primary order, movement history or command receipt, or use its product in a stock command. Second-warehouse commands leave the primary warehouse unchanged.
7. The dashboard's movement and activity windows stay at most 100 entries; two 25-item history pages have no overlapping cursors.

These checks establish software behavior for the recorded fixture. Current live catalog/open-work size still determines command memory and validation cost. Bounded historical pages do not imply bounded live-work memory or unlimited throughput.

## Executed local results

Recorded 2026-10-17 against isolated PostgreSQL 12.22 through a Unix socket, using an optimized release binary, eight available CPU threads and the standard eight-connection Store pool. All five recorded runs passed every correctness gate. They are short synthetic measurement windows on a shared development host, with the harness and PostgreSQL sharing resources; these are observations, not production SLOs.

| Profile | Products / opening bin-lots / open orders | Clients / physical moves | Moves/s | Action p95 / p99 | Stale rejections | Rust process peak |
|---|---|---|---:|---|---:|---:|
| [Small](performance/small.json) | 20 / 40 / 20 | 4 / 20 | 49.60 | 167 / 205 ms | 44 | 8.61 MiB |
| [Standard](performance/standard.json) | 100 / 200 / 100 | 4 / 40 | 21.38 | 481 / 1,148 ms | 103 | 13.00 MiB |
| [Larger, before lookup optimization](performance/large-before.json) | 500 / 1,000 / 500 | 4 / 100 | 3.41 | 2,735 / 3,355 ms | 262 | 39.45 MiB |
| [Larger, keyed lookups](performance/large-map-only.json) | 500 / 1,000 / 500 | 4 / 100 | 5.52 | 1,672 / 1,799 ms | 280 | 34.65 MiB |
| [Larger previous release, fresh database](performance/large-final.json) | 500 / 1,000 / 500 | 4 / 100 | 6.25 | 1,476 / 2,330 ms | 282 | 32.96 MiB |

The extra race SKU/bin and receiving/destination buckets are reported separately in each raw JSON. Every physical action was replayed once without another movement; there were zero transient 503s or unexpected errors. The larger profile ended with exactly 2,457 command receipts, events and activity rows, 1,104 physical movements, 100,000,008 units in custody, 250 reserved units and seven received units. Independent full-history reconciliation passed. Display history stayed at 100 movements/activity records and paginated history remained disjoint.

The larger profile exposed repeated full-vector joins and change comparisons in persistence. The implementation now joins each canonical order/receipt child row once, indexes previous operational identities once per save and compares typed values directly. Warehouse version fencing, tenant scoping, database constraints and transactional event/receipt/outbox writes remain the same. The second recorded run observed 61.8% more committed actions per second and 38.9% lower action p95 at the same configured scale. These individual observations on a shared host do not guarantee a fixed performance improvement. All correctness totals remained identical and the 19 existing PostgreSQL persistence/acceptance/guard cases passed after the change.

A subsequent integrity fix strips mutable JSON child projections before deserialization and reconstructs state, detail and order/receipt history from the same canonical child rows. Detail/history read the header and children in one repeatable-read transaction. A new real database guard corrupts child JSON types, then proves canonical state/detail/history agree, tenant isolation holds, reads add no execution evidence and a subsequent physical command still reconciles.

The previous release run passed the same larger profile in a fresh disposable database with its six recorded migrations, before operations indexes were added. It recorded 100 committed moves and 100 exact replays in 16.00 seconds, with the same independent correctness totals. Command-attempt p95 was 116 ms; state-refresh p95 was 139 ms. The setup phase took 94.94 seconds and is excluded from committed-action throughput. The runner stopped and removed its own database afterward. No unexpected errors or transient 503s occurred.

[Provenance](performance/provenance.json) records source, release-binary and raw artifact fingerprints, exact final command and comparison limits. Earlier runs shared a qualification cluster containing synthetic test data; the final run used a fresh cluster. Cache/table population and shared-host scheduling differ. Only one short window is retained per profile, so these results do not establish a controlled causal estimate or a general latency guarantee. Repeat facility peak workloads before selecting operating limits.

This is a local synthetic Store workload. It does not include browser rendering, scanner hardware, HTTP network latency, deployed OMS/TMS behavior, physical counting, PostgreSQL failover or facility acceptance. A passing profile qualifies only its recorded scale, concurrency, machine and database settings. Production sign-off requires a measured facility workload and separately qualified restore and integration checks.

## Current release observations

The [final unified gate](qualification/release/summary.json) executed two distinct protocols on the current source. Both passed their correctness gates. The Store harness calls application persistence directly; the immutable-image rehearsal performs authenticated HTTP, JSON snapshot reads and cross-replica replays. Their latency and throughput figures describe different work and should not be used as a controlled comparison.

| Protocol | Products / opening balances / open orders | Physical moves / exact replays | Measured window | Moves/s | Action p95 |
|---|---|---|---|---:|---:|
| [Current Store, PostgreSQL 12.22](qualification/release/standard_workload.json) | 100 / 200 / 100 | 40 / 40 | 1.337 s | 29.920 | 267.722 ms |
| [Current HTTP, PostgreSQL 16.15](qualification/resilience.json) | 500 / 1,000 / 500 | 372 / 372 | 60.001 s | 6.200 | 710.238 ms |

Both use four clients and one warehouse. The HTTP run uses four distinct operator identities and two independent production-image API processes. Each round starts from an actual full operational snapshot; every definite stale-version retry refreshes that snapshot. Action latency includes the round's initial read through commitment, including conflicts and full snapshot refreshes. The subsequent original-request replay is excluded from action latency but included in elapsed throughput; replays never count as physical moves. The final HTTP phase observed 558 definite stale conflicts and zero unexpected errors. Snapshot-read p95 was 150.195 ms; individual write-attempt p95 was 87.734 ms.

HTTP fixture setup took 107.197 seconds and is excluded from the sustained window. Each opening balance contains 100,000 disclosed synthetic each units; this is a scaling fixture rather than a physical count. The independent final proof checks custody, reservation ownership, exact receipt lines, retained active orders, actor/request identity and explicit expected command/movement/event/outbox counts. The [rehearsal method](resilience.md) describes the separate forced API/PostgreSQL crash boundaries.

These are local observations on a shared development host, with database, clients and containers sharing resources. No facility latency/throughput requirement was supplied; no capacity SLO is certified. The 60-second synthetic HTTP window does not qualify physical scanners, an ingress/load balancer, deployed OMS/TMS services, managed failover or a facility's sustained peak. Retained reports preserve exact source/image/script identities and measurement scopes.
