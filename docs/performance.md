# Workload qualification

## Current 0.2.0 image profiles — 2026-10-19

The [full gate](qualification/release-20261019-icommerce-tui-v1/summary.json) uses the same 500-product / 100-bin / two-lot / 500-order / eight-operator / two-replica profile, with 300 requested seconds per mode. Untracked work records 9,676 moves/replays, 32.232 moves/s and 279.478ms action p95. Serial work records 988 timed moves/replays/source reviews, 3.269 moves/s and 2935.849ms action p95 across 16,000 labels. Both final complete proofs pass at unchanged SQL timeouts with zero errors/conflicts. Serial setup (611.263s) and two recovered crash commands are excluded from throughput. Eight fixed labels move in the timed window. Shared-host results establish no controlled improvement, broad-label capacity or facility SLO. See [limits](qualification/release-20261019-icommerce-tui-v1/qualification-limits.json).

The [workload executable](../examples/workload.rs) measures current operational data and concurrent operators against real PostgreSQL. A historical-audit retention test alone cannot establish warehouse capacity. Scoped physical commands load their SKUs and owning work plus independently derived foreign holds; full operational snapshot reads and global-version commands still load current warehouse data. Writes serialize within each warehouse. This harness makes those costs and version conflicts visible before selecting a facility workload.

## Current serial after-write image qualification — 2026-10-19

The [complete gate](qualification/release-20261019-serial-after-write-v1/summary.json) retains both 500-product / 100-bin / two-lot / 500-order / eight-operator / two-replica / 300-second image profiles. [Untracked traffic](qualification/release-20261019-serial-after-write-v1/image_resilience.json) records 8,768 moves/replays, 29.199 moves/s and 340.771ms action p95. [Serial traffic](qualification/release-20261019-serial-after-write-v1/image_serial_resilience.json) records 1,045 timed moves/replays/source reviews, 3.458 moves/s and 2404.496ms action p95 across 16,000 labels. Zero conflicts/errors and both final complete API proofs pass.

Two serial commands recover from API/PostgreSQL crashes after their uncommitted custody/audit mutations. They use an additional fixed label, move it out and back before timed traffic, and are excluded from throughput and source-read sample counts. The timed window still moves eight fixed labels; 15,992 others remain static during that window. Setup takes 603.810s and remains outside timing. See [limits](qualification/release-20261019-serial-after-write-v1/qualification-limits.json). Shared-host variation establishes no causal performance improvement, facility SLO or broad-label contention capacity.

## Previous serial source-set image qualification — 2026-10-19

The [accepted local software gate](qualification/release-20261019-serial-write-source-sets-v1/summary.json) passes both full image profiles at 500 products / 100 bins / two lots / 500 orders / eight independent clients / two replicas / 300 requested seconds. [Untracked traffic](performance/http-500-8-operators-serial-source-sets-untracked-v1.json) completes 11,291 moves/replays in 300.137s at 37.620 moves/s and 239.525ms action p95. [Serial traffic](performance/http-500-8-operators-serial-source-sets-v1.json), with 16,000 literal IDs, completes 1,374 moves/replays/source reviews in 301.737s at 4.554 moves/s and 1908.038ms action p95. Zero errors/conflicts and both final API proofs pass. Shared-host observations establish no controlled capacity comparison or facility SLO.

The [same-image diagnostic](performance/serial-write-source-set-diagnostic-20261019.json) measures original/candidate/restored-original median writes of 2985.811 / 1505.019 / 2982.528ms over 32 writes per phase. It retains all 96 exact replays and final proofs. Migration 029 preserves original guard outcomes through complete differential oracles and an [actual populated 28→29 upgrade](performance/serial-write-source-set-upgrade-20261019.json). The [previous serial-history release](qualification/release-20261019-serial-history-v1/summary.json) and its raw reports retain their own source and scope; do not treat uncontrolled runs as a causal performance comparison.

Serial setup takes 422.8s and is excluded from the sustained interval. Timed work moves eight fixed labels; 15,992 others supply a static source/quantity/history footprint. Current recovery commands use the untracked recovery SKU, including in serial mode. Committed serial data survive those crashes and subsequent serial traffic, while pending serial-write recovery remains open. Reports retain aggregate timing distributions. See [limits and provenance](qualification/release-20261019-serial-write-source-sets-v1/independent-audit.json).

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
| `WMS_WORKLOAD_REVIEW_MODE` | `resources` | `resources` requires zero spurious conflicts for disjoint SKU moves; `warehouse` retains the strict global-version comparison |
| `WMS_WORKLOAD_REPORT` | stdout only | Optional JSON artifact path |
| `WMS_WORKLOAD_MAX_P95_MS` | unset | Fail if end-to-end action p95 exceeds this measured limit |
| `WMS_WORKLOAD_MIN_ACTIONS_PER_SECOND` | unset | Fail if committed physical-action throughput falls below this limit |

Each product has one opening stock bucket per lot. Moving creates a second bin bucket for a client's first lot. Zero balances remain evidence-bearing stock keys. Reported actual balance counts therefore include receiving, race and destination buckets. Configured product count must be at least client count, bins at least two, and action count positive. The executable validates bounds and threshold values before connecting.

For a larger operational profile, set products to `500`, bins to `100`, lots to `2`, open orders to `500` and actions to `25`. This seeds 1,000 stock buckets and 500 active orders through real commands rather than direct balance inserts. Seeding is measured separately and can take substantial time because it exercises the same correctness checks as normal execution. Do not compare a debug result with a release result.

## What is measured

All product creation, opening counts, receiving, putaway, order creation, allocation and test moves use `Store::execute`. Initial physical counts are disclosed synthetic fixtures. The second warehouse is created through the same journaled `create_warehouse` command as the UI; its receiving/staging bins and creation event/receipt commit atomically. All operational changes in that warehouse also use Store commands. SQL reads independently count the resulting records.

Concurrent clients alternate one-unit moves between two storage bins on distinct SKUs. Each action fetches current state, submits a versioned request and replays the exact committed request once, simulating a lost response. Resource mode captures exact source/destination stock, product and location revisions from that snapshot; independent SKU lanes must commit with zero spurious conflicts. Warehouse mode retains the old global-version behavior. A definite stale rejection leads to another state read and a new request ID for the reviewed, still-valid lane move. A transient database error retains the original request ID, payload and expected version. It never becomes another physical action merely because a network retry occurred.

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

## 2026-10-17 release observations (before resource reviews)

The [final unified gate](qualification/release-20261017/summary.json) executed two distinct protocols on that recorded source. Both passed their correctness gates. The Store harness calls application persistence directly; the immutable-image rehearsal performs authenticated HTTP, JSON snapshot reads and cross-replica replays. Their latency and throughput figures describe different work and should not be used as a controlled comparison.

| Protocol | Products / opening balances / open orders | Physical moves / exact replays | Measured window | Moves/s | Action p95 |
|---|---|---|---|---:|---:|
| [Recorded Store, PostgreSQL 12.22](qualification/release-20261017/standard_workload.json) | 100 / 200 / 100 | 40 / 40 | 1.432 s | 27.935 | 288.569 ms |
| [Recorded HTTP, PostgreSQL 16.15](qualification/resilience.json) | 500 / 1,000 / 500 | 380 / 380 | 60.509 s | 6.280 | 737.820 ms |

Both use four clients and one warehouse. The HTTP run uses four distinct operator identities and two independent production-image API processes. Each round starts from an actual full operational snapshot; every definite stale-version retry refreshes that snapshot. Action latency includes the round's initial read through commitment, including conflicts and full snapshot refreshes. The subsequent original-request replay is excluded from action latency but included in elapsed throughput; replays never count as physical moves. The final HTTP phase observed 570 definite stale conflicts and zero unexpected errors. Snapshot-read p95 was 155.093 ms; individual write-attempt p95 was 94.866 ms.

HTTP fixture setup took 77.431 seconds and is excluded from the sustained window. Each opening balance contains 100,000 disclosed synthetic each units; this is a scaling fixture rather than a physical count. The independent final proof checks custody, reservation ownership, exact receipt lines, retained active orders, actor/request identity and explicit expected command/movement/event/outbox counts. The [rehearsal method](resilience.md) describes the separate forced API/PostgreSQL crash boundaries.

These are local observations on a shared development host, with database, clients and containers sharing resources. No facility latency/throughput requirement was supplied; no capacity SLO is certified. The 60-second synthetic HTTP window does not qualify physical scanners, an ingress/load balancer, deployed OMS/TMS services, managed failover or a facility's sustained peak. Retained reports preserve exact source/image/script identities and measurement scopes.


## 2026-10-18 resource-review release

The [resource-review release gate](qualification/release-20261018-resource-review/summary.json) passed all ten stages on 110 frozen source inputs and eleven migrations. Physical commands carry exact resource revisions; the benchmark's independent SKU lanes are required to produce zero spurious conflicts. Committed request replay reads immutable receipts without the warehouse writer lock. Both recorded protocols passed independent inventory, reservation, receipt and exact execution-count proofs.

| Protocol | Products / opening balances / open orders | Clients | Moves / exact replays | Window | Moves/s | Action p95 |
|---|---|---|---:|---|---:|---:|
| [Recorded Store, PostgreSQL 12.22](qualification/release-20261018-resource-review/standard_workload.json) | 100 / 200 / 100 | 4 | 40 / 40 | 0.744 s | 53.768 | 122.516 ms |
| [Recorded immutable-image HTTP, PostgreSQL 16.15](qualification/release-20261018-resource-review/image_resilience.json) | 100 / 200 / 100 | 4 | 2,300 / 2,300 | 60.083 s | 38.280 | 110.948 ms |

The HTTP run starts each round from one full operational snapshot shared by all four operators, then commits independent SKU moves and exact cross-replica replays. There were **zero stale conflicts and zero unexpected errors**. Snapshot p95 was 30.881 ms and individual write-attempt p95 83.275 ms. Action latency includes the initial snapshot through committed movement; exact replay is excluded from action latency but included in elapsed throughput. No replay counts as another physical movement.

These protocols, source versions, host conditions and fixture sizes differ from earlier reports, particularly the historical 500-product HTTP fixture. Do not treat their throughput ratios as a controlled performance estimate. The Store window is intentionally short and both database/client/container workloads share development-host resources. No production capacity target was supplied. The same image also passed forced API and PostgreSQL crash/recovery with all-table durability, immutable request recovery and independent final custody/reservation proofs. Actual device, facility, deployed integration and managed-failover acceptance remain separate.


### Larger five-minute HTTP profile

The [extended 500-product report](performance/http-500-8-operators.json) and [provenance](performance/http-500-8-operators-provenance.json) use the **same immutable image** as that resource-review release gate. With **500 products, 1,000 opening bin/lot balances, 500 open orders, eight distinct operators and two API replicas**, the sustained phase committed **3,792 physical moves and 3,792 exact cross-replica replays in 300.271 seconds**. There were **zero spurious conflicts and zero unexpected errors**.

Observed throughput was **12.629 moves/s**; action p50/p95/p99 were **396.841 / 638.592 / 693.566 ms**, with max 866.068 ms. Snapshot p95 was 102.095 ms and individual write-attempt p95 547.212 ms. Every operator committed/replayed exactly 474 actions. Fixture setup took 89.6 seconds and is excluded from the sustained window. The separate lost-success and forced API/PostgreSQL crash phases also passed.

Independent final counts matched the precomputed model: **6,157 commands/events/activity records**, **4,801 movements** and **6,159 outbox records**. Physical custody remained **100,000,013 each**, reservations **253 each**, and all 500 seeded open orders retained their states. Both replicas passed full ledger/receipt/allocation reconciliation; original actor/request/result ownership and contiguous warehouse versions matched.

This demonstrates correctness and measured behavior at the recorded scale. That image serializes writes and loads active warehouse data for each command; higher catalog/open-work size remains a cost for that implementation. The previous 500-product HTTP report used four operators, a shorter window, another source/image and different shared-host conditions, so the figures are observations rather than a controlled speedup claim. Eight synthetic operators and a five-minute software window do not establish a facility SLO or real device acceptance.


## 2026-10-18 selective-command release (shared-snapshot protocol)

The [shared-snapshot complete gate](qualification/release-20261018-shared-snapshot/summary.json) passed all ten stages on 111 frozen software inputs, using immutable image `sha256:fe24dece52ab8a48a5784e29da6729e6ee7b480cbbca91db3a157fab381ff275`. Scoped physical commands load their SKUs and owning work, derive foreign source/staging holds from canonical allocations and keep the normal reducer, warehouse lock and deferred database proofs. Public operational reads and full independent reconciliation still cover the warehouse. Strict warehouse-version requests retain the full loader.

| Protocol | Products / opening balances / open orders | Clients | Moves / exact replays | Window | Moves/s | Action p95 |
|---|---|---|---:|---|---:|---:|
| [Recorded Store, PostgreSQL 12.22](qualification/release-20261018-shared-snapshot/standard_workload.json) | 100 / 200 / 100 | 4 | 40 / 40 | 0.325 s | 123.155 | 42.910 ms |
| [Recorded immutable-image HTTP, PostgreSQL 16.15](qualification/release-20261018-shared-snapshot/image_resilience.json) | 100 / 200 / 100 | 4 | 3,980 / 3,980 | 60.040 s | 66.289 | 69.142 ms |

Both protocols reported zero stale conflicts, transient errors and unexpected errors. HTTP action p50/p95/p99 were 46.399 / 69.142 / 88.953 ms; write-attempt p95 was 36.804 ms and snapshot p95 32.831 ms. Each operator committed and replayed 995 moves. An independent count audit recomputed 4,465 commands/events/activity records, 4,189 movements and 4,467 outbox rows. Every original request and actor, warehouse version, exact receipt line, physical ledger and reservation owner reconciled; both forced crash/recovery phases passed. The real custom-archive restore also passed separately in the full gate.

The Store measurement is a short window and uses another database/protocol; it is not a comparison with the HTTP rate. Earlier HTTP observations above retain their source/image identity. Local shared-host scheduling, cache population and fixture differences prevent a controlled causal estimate or general latency guarantee. No facility capacity target was supplied, and physical devices, deployed integrations and managed failover remain separate acceptance work.

### Larger five-minute comparison

The [selective-command report](performance/http-500-8-operators-selective.json) and [provenance](performance/http-500-8-operators-selective-provenance.json) use the same immutable image as that shared-snapshot full gate. The fixture and protocol match the retained resource-review baseline: 500 products, 1,000 opening bin/lot balances, 500 open orders, eight distinct operators, two replicas and five continuous minutes. Each round shares one complete operational snapshot, then executes independent SKU-lane movements and exact cross-replica replays. This is not eight independent dashboard refresh streams or a browser-rendering measurement.

| Same configured HTTP protocol | Committed moves / exact replays | Window | Moves/s | Action p95 | Conflicts / errors |
|---|---|---|---:|---:|---|
| [Before selective loading](performance/http-500-8-operators.json) | 3,792 / 3,792 | 300.271 s | 12.629 | 638.592 ms | 0 / 0 |
| [Selective loading](performance/http-500-8-operators-selective.json) | 14,888 / 14,888 | 300.038 s | 49.620 | 196.870 ms | 0 / 0 |

The observed throughput ratio is **3.93×**, with **69.2% lower action p95** in these separate local windows. Action p50/p95/p99 were 130.449 / 196.870 / 232.847 ms; max was 351.200 ms. Individual write-attempt p95 was 67.341 ms and full-snapshot p95 130.370 ms. Each operator committed and replayed exactly 1,861 moves. Synthetic fixture setup took 106.584 seconds and is excluded from the five-minute measurement.

The independent final proof and a separate arithmetic audit matched **17,253 commands/events/activity records**, **15,897 physical movements**, **17,255 outbox records**, **100,000,013 each in custody** and **253 reserved each**. All 500 seeded open orders retained their states, all 26 WMS tables were fingerprinted, both replicas passed full historical reconciliation, and original actor/request/result ownership and contiguous versions matched. Lost-success, API SIGKILL and PostgreSQL crash recovery passed before the timed phase. No credential values appear in the retained report.

The executable source, image and measurement window changed; the qualifier scripts and configured protocol did not. Database population, caches and shared-host scheduling can differ, so this is an observed comparison rather than a controlled speedup guarantee. Full warehouse reads still scale with the active catalog and work set. Independent operator refresh traffic, facility peak load, physical scanners and deployed integration acceptance require their own measurements before choosing operating limits.


## 2026-10-18 independent-operator traffic (before facility administration)

The [associated independent-operator full gate](qualification/release-20261018-independent-operators/summary.json) passed all ten stages on 112 frozen software inputs. Its application image is unchanged from the selective-command release; the stronger default protocol gives every operator its own initial warehouse review and full post-save refresh. Clients progress independently without a shared snapshot or round barrier. The saved review is refreshed only after a confirmed save or a definitive stale rejection, matching resource-scoped dashboard work.

The [100-product HTTP report](qualification/release-20261018-independent-operators/image_resilience.json) records **4,444 moves and 4,444 exact cross-replica replays in 60.046 seconds**, with four operators and two replicas. Every successful movement had its own validated full post-save refresh; including four initial reviews, this produced **4,448 snapshot reads**. Throughput was **74.010 moves/s**, save p50/p95/p99 **46.718 / 63.191 / 73.242 ms**, write-attempt p95 **32.038 ms**, snapshot p95 **38.739 ms** and replay p95 **14.338 ms**. Operator commit/replay counts were 1,113 / 1,113 / 1,107 / 1,111; every client progressed, with zero conflicts or unexpected errors. Independent expected/observed counts matched 4,929 commands/events/activity records, 4,653 movements and 4,931 outbox rows; full receipt, physical and reservation proofs passed through both replicas.

Save latency begins with command submission from that client's cached review and ends after its validated full post-commit warehouse refresh. Initial review acquisition and subsequent deliberate replay have separate samples and remain included in elapsed throughput. Every returned refresh must contain the committed version and the exact operator SKU/bin move parity. The shared-snapshot profile above has another read count, scheduler and latency scope; these figures do not establish a performance improvement over that protocol.

Clients run continuously without human think time, through authenticated loopback HTTP with a new Python HTTP connection per request. The clients, application replicas and PostgreSQL primary share one development host. Browser rendering, independent device CPUs, physical scanners, deployed networking, native integrations and managed failover are excluded. Select facility operating limits only after testing declared peak concurrency/action rates with the actual deployed workload.

### Five-minute independent-operator profile

The [500-product report](performance/http-500-8-operators-independent.json) and [provenance](performance/http-500-8-operators-independent-provenance.json) use the same immutable application image and independent-operator protocol as the associated independent-operator full gate. The fixture has **500 products, 1,000 opening bin/lot balances, 500 open orders, eight distinct operators and two API replicas**. Its sustained phase ran **300.133 seconds**, completing **10,199 physical moves**, **10,199 exact cross-replica replays** and **10,207 full operational snapshot reads**, with **zero conflicts and zero unexpected errors**. Setup took 90.873 seconds and is excluded from the timed phase.

| Observation | Recorded result |
|---|---|
| Physical throughput | 33.982 moves/s; replay never counts as another move |
| HTTP save through validated full refresh | p50 197.482 ms; p95 274.137 ms; p99 321.109 ms; max 463.203 ms |
| Individual HTTP write attempt | p95 96.810 ms |
| Full snapshot read | p95 215.103 ms; 10,207 samples |
| Exact original replay | p95 74.637 ms; 10,199 samples |
| Per-operator completed moves/replays | 1,266–1,282 each; all eight operators progressed |
| Read accounting | Eight initial reviews plus exactly 10,199 validated post-save refreshes |

An independent arithmetic audit matched **12,564 commands/events/activity records**, **11,208 movements**, **12,566 outbox rows**, **100,000,013 each in physical custody** and **253 reserved each**. All 500 seeded open orders retained their states; source/destination move parity, original actor/request/result ownership and contiguous versions matched. Both replicas passed full ledger/receipt/allocation reconciliation, all 26 WMS tables were fingerprinted, and the lost-success and forced API/PostgreSQL crash phases passed before timed traffic. Credential values are absent from the published report.

This is a stronger read-volume measurement than the preceding shared-snapshot fixture, with another scheduler and latency scope. It is not a performance regression or speedup comparison: the application image is identical and the traffic model changed. Full warehouse reads remain a material part of the measured save cycle. The clients, API replicas and database share one host, so this result does not isolate one server bottleneck or certify capacity on independently deployed devices. The report establishes behavior for this continuous synthetic HTTP workload; no facility SLO or actual browser/device acceptance is implied.


## 2026-10-18 multi-facility release

The [dated full release gate](qualification/release-20261018-multi-facility/summary.json) qualify immutable image `sha256:07ba6293b0f01a00b72d4cd864a056bbbb00d54a59c1425c848b86f175a0c1b1` on 116 frozen software inputs. It adds journaled facility creation and safe warehouse selection. The qualifier also strengthens every snapshot check from catalog/work counts to the exact expected SKU set and all seeded order IDs/statuses, with same-count substitution regressions. The Store isolation facility now uses `create_warehouse` instead of direct header/bin inserts.

The [retained five-minute HTTP report](performance/http-500-8-operators-multi-facility.json) and [provenance](performance/http-500-8-operators-multi-facility-provenance.json) describe 500 products, 1,000 opening balances, 500 open orders, eight independent operators and two replicas. They use the same image as the full gate.

| Observation | Recorded result |
|---|---|
| Sustained interval | 300.238 s; setup 84.300 s excluded |
| Physical moves / exact replays | 8,526 / 8,526; zero conflicts and unexpected errors |
| Full snapshot reads | 8,534: eight initial reviews plus one validated refresh per move |
| Physical throughput | 28.397 moves/s, including refreshes and deliberate replays in elapsed time |
| HTTP save through validated refresh | p50 220.845 ms; p95 400.110 ms; p99 491.026 ms; max 1,288.937 ms |
| Snapshot / write-attempt / replay p95 | 315.488 / 128.146 / 94.104 ms |
| Per-operator completed moves/replays | 1,060–1,074 each; all eight operators progressed |

Independent expected/observed totals matched 10,891 commands/events/activity records, 9,535 movements, 10,893 outbox rows, 100,000,013 each in custody and 253 reserved each. All 500 seeded order states, stock parity, exact receipt-line evidence, contiguous versions and actor/request/result ownership matched. Both replicas passed full historical reconciliation, and lost-success/API SIGKILL/PostgreSQL crash recovery passed before the timed phase.

The earlier independent-operator window recorded 33.982 moves/s and 274.137 ms p95. This window is slower. Application source, qualifier verification and measurement conditions changed; [diagnostic context](performance/http-500-8-operators-multi-facility-host-observation.json) records compiler processes visible after this gate's builds finished. That observation does not isolate causality, exclude a regression or establish a performance improvement. Retain both results. Full warehouse reads remain a material part of the HTTP save cycle, and client, API and database processes share the host. No facility service target was supplied, and these figures do not certify browser rendering, physical devices, deployed integrations or managed failover.


## 2026-10-18 snapshot-read optimization

The [dated complete release gate](qualification/release-20261018-snapshot-reads/summary.json) qualify image `sha256:f6d5db89e96c1d7d66bd52912714641040ab5f25ad7d4a646d52612c4b4d9f53` on 116 frozen inputs. The state endpoint serializes typed state directly, computes the operational proof once and reuses product/location indexes during structural validation. Its proof accumulates actual balances, immutable physical evidence and exact reservation ownership in one ordered map with borrowed keys. Public JSON fields and invalid-state diagnostics are preserved; there is no snapshot cache. Full independent SQL reconciliation remains unchanged.

The [before report](performance/http-500-8-operators-snapshot-before.json) ([provenance](performance/http-500-8-operators-snapshot-before-provenance.json)) uses the previously qualified multi-facility image. The [after report](performance/http-500-8-operators-snapshot-after.json) ([provenance](performance/http-500-8-operators-snapshot-after-provenance.json)) uses this gate's new image. Both use the **same qualifier script hashes**, 500 operational products, 100 storage bins, two lots, 1,000 opening balances, 500 open orders, eight independent operators, two replicas and a 300-second requested interval. Every action includes its own validated full post-save refresh, followed by an exact cross-replica replay. A separate pre-interval snapshot fixture is excluded from the timed read counts.

| Observation | Before | After |
|---|---:|---:|
| Actual sustained interval | 300.218 s | 300.202 s |
| Setup, excluded | 85.478 s | 71.972 s |
| Physical moves / exact replays | 9,260 / 9,260 | 8,968 / 8,968 |
| Full snapshot reads | 9,268 | 8,976 |
| Physical throughput | 30.844 moves/s | 29.873 moves/s |
| Save/refresh p50 / p95 / p99 | 215.645 / 297.857 / 339.886 ms | 209.953 / 339.653 / 426.485 ms |
| Snapshot p95 | 237.460 ms | 270.174 ms |
| Write-attempt p95 | 104.809 ms | 116.118 ms |
| Exact replay p95 | 88.063 ms | 114.471 ms |
| Conflicts / unexpected errors | 0 / 0 | 0 / 0 |

The optimized run's independent SQL proof and separate arithmetic audit matched 11,333 commands/events/activity, 9,977 physical movements, 11,335 outbox rows, 100,000,013 each in custody and 253 reserved each. Every actor progressed, all 500 seeded order IDs/states remained present, and post-save stock parity, exact receipt-line evidence, contiguous versions and original actor/request/result ownership matched. Both replicas passed independent full historical reconciliation, and all 26 WMS tables were fingerprinted. Lost-success recovery, deterministic API SIGKILL and PostgreSQL crash/durability recovery passed on both images.

A retained pre-optimization independent-map oracle checks generated balances and explicit corrupt states against the optimized proof. A separate exact response comparison also matched the old captured 501-product/502-order/1,254-proof-row fixture. The full release gate rechecked database constraints, archive recovery and all 73 browser workflows. These establish preserved software behavior for the fixture.

The after window was slower in end-to-end HTTP latency and throughput. An additional [isolated optimized Rust diagnostic](performance/snapshot-response-diagnostic.json) using the frozen original validator/proof/serializer and captured fixture measured median response construction at 20.468 ms before and 3.128 ms after (100 alternating pairs after ten warmups, identical dependency versions, clones outside timing). It excludes database, transport and client processing; the full captured response and validation result matched exactly. This diagnostic establishes reduced work in that response path, while the HTTP run establishes no end-to-end performance improvement. Controlled host measurements remain a follow-up.

These sequential windows control protocol and fixture, while the client, database and API processes still share a development host. Compiler processes became visible during part of the optimized timed window after this gate's builds had finished; [retained diagnostic context](performance/http-500-8-operators-snapshot-after-host-observation.json) records process names and lifetime CPU averages. That observation neither isolates causality nor rules out an application regression. Preserve both results, including all slower historical windows above. The isolated response comparison excludes database/network/client work, and HTTP observations do not certify browser rendering, physical scanners, deployed integrations, managed failover or an unspecified facility service target. `production_qualified` remains false.


## 2026-10-18 receipt-cancellation release

The [dated complete release gate](qualification/release-20261018-receipt-cancellation/summary.json) qualify image `sha256:a0b2641e89a4a8c24f557b3988de5507638d4199373c6782ec429014105e3e3b` on 120 frozen inputs and twelve migrations. It adds reasoned inbound/RMA remainder cancellation, immutable original receipt expectations and normalized frozen remainders, all-history return allowance/proof updates, operator controls and original cancellation recovery after archive restore. Cancellation changes no stock or physical movement. Database races, integrity negatives, upgrade admission and actual browser/API execution separately exercise that workflow.

The [retained HTTP report](performance/http-500-8-operators-receipt-cancellation.json) and [provenance](performance/http-500-8-operators-receipt-cancellation-provenance.json) use the same immutable image as all ten gate stages. The configured fixture remains 500 operational products, 100 storage bins, two lots, 1,000 opening balances, 500 open orders, eight independent operators and two replicas. The sustained actions are bin moves with a validated full post-save refresh and exact cross-replica replay, rather than repeated cancellations. No cancellation-specific sustained throughput is claimed.

| Observation | Recorded result |
|---|---:|
| Actual sustained interval / excluded setup | 300.149 / 91.908 s |
| Physical moves / exact original replays | 11,420 / 11,420 |
| Full snapshot reads | 11,428 |
| Physical throughput | 38.048 moves/s |
| HTTP save/refresh p50 / p95 / p99 | 168.495 / 237.384 / 272.983 ms |
| Snapshot / write-attempt / replay p95 | 188.882 / 86.339 / 84.998 ms |
| Conflicts / unexpected errors | 0 / 0 |

Independent SQL and separate count equations matched 13,785 commands/events/activity, 12,429 movements, 13,787 outbox records, 100,000,013 each in custody and 253 reserved each. All eight actors progressed, all 500 seeded order IDs/states stayed present, every post-save stock/version check matched, both replicas passed full historical reconciliation and all 28 WMS tables were fingerprinted. Lost-success recovery, deterministic API SIGKILL and PostgreSQL crash recovery passed before the timed phase. The separate archive fixture contains nonempty cancellation tables and proves their restoration and original-result replay without evidence changes.

Client, API and database processes share a development host. The qualifier hashes/traffic model are retained, while application/migration inputs differ from preceding windows. Preserve every prior result above; this single observation establishes neither a causal speedup nor a deployed capacity/facility service target. Browser rendering, physical scanners, external integration acceptance and managed database failover require their own evidence. `production_qualified` remains false.


## 2026-10-18 original-shipment evidence release

The [dated complete release gate](qualification/release-20261018-shipment-evidence/summary.json) qualify image `sha256:a684bb0dd7b00c961325c23b0b652938637f0fe4c3476fdb5b7ff80ee056cca0` on 123 frozen inputs and thirteen migrations. It adds immutable original handoff sources, guarded completed order facts, return limits derived from original allocations, source provenance and independent source reconciliation. Actual SQL races, fault rollback, multi-bin/lot ownership, old-schema upgrades, legacy import and archive restoration separately exercise these changes.

The [retained HTTP report](performance/http-500-8-operators-shipment-evidence.json) and [provenance](performance/http-500-8-operators-shipment-evidence-provenance.json) use the same immutable image as all ten gate stages. The fixture remains 500 operational products, 100 storage bins, two lots, 1,000 opening balances, 500 open orders, eight independent operators and two replicas. Sustained actions are bin moves with full post-save refresh and exact cross-replica replay. This does not measure repeated shipping, source recording or RMA authorization throughput.

| Observation | Recorded result |
|---|---:|
| Actual sustained interval / excluded setup | 300.135 / 71.676 s |
| Physical moves / exact original replays | 9,058 / 9,058 |
| Full snapshot reads | 9,066 |
| Physical throughput | 30.180 moves/s |
| HTTP save/refresh p50 / p95 / p99 | 190.893 / 421.761 / 524.912 ms |
| Snapshot / write-attempt / replay p95 | 331.093 / 126.454 / 113.383 ms |
| Conflicts / unexpected errors | 0 / 0 |

Independent SQL and separate count equations matched 11,423 commands/events/activity, 10,067 movements, 11,425 outbox records, 100,000,013 each in custody and 253 reserved each. All eight actors progressed, all 500 seeded order IDs/states stayed present, every post-save stock/version check matched, both replicas passed full historical reconciliation and all 29 WMS tables were fingerprinted. Lost-success recovery, deterministic API SIGKILL and PostgreSQL crash recovery passed before the timed phase. The archive fixture separately contains nonempty original shipment sources and proves exact event identity/provenance restoration with unchanged replay hashes.

Client, API and database share a development host. Concurrent Cargo/Rust compiler processes were observed late in the sustained window and a private process sample is retained; no causal attribution is inferred. Qualifier hashes and the traffic model are retained, while application/migration inputs changed. Every preceding window remains retained. This observation establishes neither a causal speedup nor a deployed capacity/facility service target. Physical devices, deployed services and managed failover require separate evidence; `production_qualified` remains false.


## 2026-10-18 facility-transfer release

The [current complete release gate](qualification/release-20261018-facility-transfers/summary.json) and [dated archive](qualification/release-20261018-facility-transfers/summary.json) qualify image `sha256:398aa9cb3f536bd91d225f643d7dec3e5472b8aa93c4ea779ab409be839a444e` on 127 frozen inputs and fourteen migrations. The new transfer workflow retains immutable physical dispatch/line evidence, separate partial destination arrivals and in-transit custody. Actual database/API races, stock-balanced forgery rejection, opposite dispatch locking, atomic outbox failure, browser work/recovery and a nonempty archive round-trip separately qualify those behaviors.

The [retained HTTP report](performance/http-500-8-operators-facility-transfers.json) and [provenance](performance/http-500-8-operators-facility-transfers-provenance.json) use the same immutable image for container runtime and HTTP resilience. The remaining gate stages execute builds from the bound frozen source inputs. The fixture remains 500 operational products, 100 storage bins, two lots, 1,000 opening balances, 500 open orders, eight independent operators and two replicas. Sustained actions are bin moves with full post-save refresh and exact cross-replica replay. This measures neither repeated inter-facility dispatch/arrival nor real carrier transit time.

| Observation | Recorded result |
|---|---:|
| Actual sustained interval / excluded setup | 300.408 / 161.842 s |
| Physical moves / exact original replays | 4,603 / 4,603 |
| Full snapshot reads | 4,611 |
| Physical throughput | 15.323 moves/s |
| HTTP save/refresh p50 / p95 / p99 | 412.264 / 724.431 / 1370.158 ms |
| Snapshot / write-attempt / replay p95 | 557.220 / 255.946 / 204.495 ms |
| Conflicts / unexpected errors | 0 / 0 |

Independent SQL and separate count equations matched 6,968 commands/events/activity, 5,612 movements, 6,970 outbox records, 100,000,013 each in custody and 253 reserved each. All eight actors progressed, all 500 seeded order IDs/states stayed present, every post-save stock/version check matched, both replicas passed full historical reconciliation and all 31 WMS tables were fingerprinted. Lost-success recovery, deterministic API SIGKILL and PostgreSQL crash recovery passed before the timed phase. The archive fixture separately contains the nonempty original transfer and partial arrival, with remaining in-transit quantity and unchanged hashes across both original replays.

Compared with the preceding original-shipment window, observed throughput changed from 30.180 to 15.323 moves/s and save/refresh p95 from 421.761 to 724.431 ms. This window was slower. The differing application inputs and uncontrolled host activity do not establish a cause. Both complete results remain retained.

Client, APIs and database share a development host. A single timed-window host sample recorded load averages 38.25 / 34.80 / 21.05 and is retained in the supporting provenance; it does not identify a cause or establish controlled capacity. Qualifier hashes and traffic model are retained; application/migration inputs changed. Preserve every prior window above: these observations establish neither a causal speedup nor deployed capacity or a facility SLO. Physical devices, deployed identity/native services and managed failover require separate evidence. `production_qualified` remains false.


## 2026-10-18 partial-fulfillment release

The [current complete release gate](qualification/release-20261018-split-fulfillment/summary.json) and [dated archive](qualification/release-20261018-split-fulfillment/summary.json) qualify image `sha256:cbf62efc5eaa53ec24fd51758707eb5d900e995a76d63cd9df5ce75e69dbfc55` on 131 frozen inputs and fifteen migrations. Available-stock allocation, backorders, immutable exact package claims, partial physical handoffs, early returns, blocked recovery and the single-package native protocol are separately exercised by actual database/API races, forgery/rollback/authorization tests, real browser work and nonempty archive restoration.

The [retained HTTP report](performance/http-500-8-operators-split-fulfillment.json) and [provenance](performance/http-500-8-operators-split-fulfillment-provenance.json) bind the same immutable image for container runtime and HTTP resilience; the remaining gate stages execute builds from the frozen source inputs. The fixture remains 500 operational products, 100 storage bins, two lots, 1,000 opening balances, 500 open orders, eight independent operators and two replicas. Sustained actions are bin moves with full post-save refresh and exact cross-replica replay. This measures neither repeated partial handoffs nor transfer-specific throughput.

| Observation | Recorded result |
|---|---:|
| Actual sustained interval / excluded setup | 300.234 / 83.000 s |
| Physical moves / exact original replays | 11,011 / 11,011 |
| Full snapshot reads | 11,019 |
| Physical throughput | 36.675 moves/s |
| HTTP save/refresh p50 / p95 / p99 | 174.013 / 252.719 / 298.635 ms |
| Snapshot / write-attempt / replay p95 | 198.244 / 92.056 / 89.369 ms |
| Conflicts / unexpected errors | 0 / 0 |

Independent SQL and separate equations matched 13,376 commands/events/activity, 12,020 movements, 13,378 outbox records, 100,000,013 each in custody and 253 reserved each. Every actor progressed, all 500 original seeded orders remained, every post-save stock/version check matched, both replicas passed full historical reconciliation and all 35 WMS tables were fingerprinted. Lost-success recovery, deterministic API SIGKILL and PostgreSQL crash recovery passed before the timed phase. The archive fixture separately retains a partial handoff, active packing, backorder and early return and proves unchanged hashes after original request recovery.

The [preceding failed full-gate attempt](qualification/attempt-20261018-split-fulfillment-timeout/failure-context.json) is retained alongside these observations. At its last progress message it had 2,699 moves at 110.2 seconds; two writes then timed out after 10 seconds waiting for the warehouse head. No complete final total or inventory proof is inferred for that failed interval. The root cause remains unconfirmed. A separate [same-image diagnostic replay](performance/http-500-8-operators-split-diagnostic.json) and [provenance](performance/http-500-8-operators-split-diagnostic-provenance.json) passed 8,758 moves/replays, zero conflicts/errors, 300.185 seconds and 347.495 ms save/refresh p95. It added read-only database/host sampling; the application and qualifier inputs were unchanged. The final full gate attempted an equivalent passive observer, but Docker access was denied from its restricted tool namespace: it captured host pressure and zero PostgreSQL blocker samples. Its exact source and raw host/error records are retained with hashes in provenance. Observer overhead and uncontrolled host conditions prevent a causal comparison. No timeout increase, skipped assertion or automatic retry hid the failed attempt.

The preceding facility-transfer window remains **15.323 moves/s** with **724.431 ms** save/refresh p95. All earlier windows above remain retained. Client, APIs and database share a development host; the supporting provenance retains a timed-window host-load and visible-process observation. Process counts are restricted to the tool execution PID namespace and do not establish host-wide compiler absence. Application/migration inputs changed while qualifier hashes and traffic model remain recorded. These single shared-host observations establish no causal speedup, controlled capacity or facility SLO. Physical devices, deployed identity/native services and managed failover require separate evidence; `production_qualified` remains false.


## 2026-10-18 native MCP release

The [current complete gate](qualification/release-20261018-wms-mcp/summary.json) and [permanent archive](qualification/release-20261018-wms-mcp/summary.json) qualify immutable image `sha256:fe63334b115d644f8e069b0674ec0db35d33ccad4b40f81e4120ed0ecdc7cf74` on 133 frozen software inputs and fifteen migrations. [MCP protocol, identity, permission and original-journal recovery checks](mcp.md) are separate correctness tests. The container check independently discovers all 51 administrator tools and matches the authenticated MCP principal to REST. This sustained profile still uses REST bin moves and original replays; it measures neither sustained MCP traffic nor repeated partial shipments or transfers.

The [HTTP report](performance/http-500-8-operators-wms-mcp.json) and [provenance](performance/http-500-8-operators-wms-mcp-provenance.json) retain the unchanged 500-product, 1,000-opening-balance, 500-open-order fixture with eight independent operators and two API replicas. Setup is excluded from the timed interval.

| Observation | Recorded result |
|---|---:|
| Sustained interval / excluded setup | 300.153 / 73.997 s |
| Physical moves / exact cross-replica replays | 10,934 / 10,934 |
| Full snapshot reads | 10,942 |
| Throughput | 36.428 moves/s |
| Save/refresh p50 / p95 / p99 | 175.364 / 254.305 / 295.984 ms |
| Snapshot / write-attempt / replay p95 | 199.191 / 92.762 / 90.597 ms |
| Conflicts / unexpected errors | 0 / 0 |

Independent SQL and separate equations match 13,299 commands/events/activity, 11,943 movements, 13,301 outbox records, 100,000,013 each in custody and 253 reserved each. All actors progressed, all 500 seeded orders remained, both replicas passed full historical reconciliation and all 35 WMS tables were fingerprinted. Forced API/PostgreSQL recovery passed before timed traffic; archive restoration separately retained nonempty package/transfer/return ownership and original requests.

All earlier windows remain retained, including the [failed split-release attempt](qualification/attempt-20261018-split-fulfillment-timeout/failure-context.json) and [separate diagnostic replay](performance/http-500-8-operators-split-diagnostic.json). That failure's lock-holder/root cause remains unconfirmed; no final inventory total is inferred for its interrupted interval. This new adapter does not establish a fix. The shared development host is uncontrolled, and this complete run adds no PostgreSQL blocker sampler. No causal speedup, managed-failover claim, deployed MCP acceptance, controlled capacity or facility SLO follows from these observations. `production_qualified` remains false.


## 2026-10-18 partial-order cancellation release

The [current complete gate](qualification/release-20261018-order-remainder/summary.json) and [dated archive](qualification/release-20261018-order-remainder/summary.json) qualify image `sha256:b316ba7e357d5522559eb281e59cb3b871c7a27ee78baba3ec979f16a1e91419` on 135 frozen software inputs and sixteen migrations. Cancellation of all unshipped demand is separately verified through domain, PostgreSQL ownership/race/fault/upgrade tests, MCP, browser recovery and a real browser/API/PostgreSQL workflow. The archive retains nonempty original cancellation ownership beside other active packed/backordered work. This timed profile still performs REST bin moves; it does not measure repeated cancellation, package handling, transfer-specific or MCP capacity.

The [retained HTTP report](performance/http-500-8-operators-order-remainder.json) and [provenance](performance/http-500-8-operators-order-remainder-provenance.json) use 500 operational products, 1,000 opening balances, 500 open orders, eight independent operators and two replicas. Setup is excluded from the five-minute interval.

| Observation | Recorded result |
|---|---:|
| Sustained interval / excluded setup | 300.257 / 97.938 s |
| Physical moves / exact replays | 10,694 / 10,694 |
| Full snapshot reads | 10,702 |
| Throughput | 35.616 moves/s |
| Save/refresh p50 / p95 / p99 | 181.546 / 259.414 / 306.831 ms |
| Snapshot / write-attempt / replay p95 | 201.418 / 96.952 / 89.124 ms |
| Conflicts / unexpected errors | 0 / 0 |

Independent SQL and separate equations match 13,059 commands/events/activity, 11,703 movements, 13,061 outbox records, 100,000,013 each in custody and 253 reserved each. Every actor progressed, all original seeded orders remained, both replicas passed full historical reconciliation and all 37 WMS tables were fingerprinted. Forced API/PostgreSQL recovery passed before timed traffic; actual archive recovery separately proved immutable cancellation ownership, actual return allowances and original request recovery without changed evidence.

All earlier windows remain available, including the [failed split-release interval](qualification/attempt-20261018-split-fulfillment-timeout/failure-context.json) and [diagnostic replay](performance/http-500-8-operators-split-diagnostic.json). Their root-cause limits remain unchanged. Client, APIs and PostgreSQL share an uncontrolled development host; this run adds no PostgreSQL blocker sampler. No causal speedup, deployed MCP/native acceptance, managed failover, controlled capacity or facility SLO follows from these measurements. `production_qualified` stays false.


## 2026-10-18 lot expiry and quarantine release

The [current complete gate](qualification/release-20261018-lot-controls/summary.json) and [dated archive](qualification/release-20261018-lot-controls/summary.json) qualify image `sha256:0c072d68b369cf5815790eb88c256be3c5426ae6c82ffe7b4d071be98c77d4fd` on 142 frozen software inputs and seventeen migrations. Lot controls have separate domain, PostgreSQL race/fault/forensic/upgrade, MCP and browser coverage. Actual restoration retains nonempty required policies, corrected expiry, independent holds, original eligibility decisions, staged packing, an earlier handoff and partially arrived goods in transit. Original retries change no table hashes across actual Auckland/UTC application sessions.

The [retained HTTP report](performance/http-500-8-operators-lot-controls.json) and [provenance](performance/http-500-8-operators-lot-controls-provenance.json) use 500 operational products, 1,000 opening balances, 500 open orders, eight independent operators and two replicas. Setup is excluded from the timed interval. Timed work is still REST bin moves and full refreshes; it is not a lot-policy, expiry-heavy fulfillment, transfer-specific or MCP throughput test.

| Observation | Recorded result |
|---|---:|
| Sustained interval / excluded setup | 300.130 / 156.364 s |
| Physical moves / exact replays | 8,658 / 8,658 |
| Full snapshot reads | 8,666 |
| Throughput | 28.847 moves/s |
| Save/refresh p50 / p95 / p99 | 218.532 / 398.993 / 500.952 ms |
| Snapshot / write-attempt / replay p95 | 309.582 / 120.175 / 98.775 ms |
| Conflicts / unexpected errors | 0 / 0 |

Independent SQL and separate equations match 11,023 commands/events/activity, 9,667 movements, 11,025 outbox records, 100,000,013 each in custody and 253 reserved each. Every actor progressed, all original seeded orders remained, both replicas passed full reconciliation and all 40 WMS tables were fingerprinted. Forced API/PostgreSQL recovery passed before timed traffic using the same immutable image as the container check.

All prior windows remain retained, including the failed split interval and separate diagnostic. Its original lock-holder/root cause remains unconfirmed. Clients, APIs and PostgreSQL share an uncontrolled host. No PostgreSQL blocker sampling, causal speedup, deployed capacity, managed failover or facility SLO is inferred. `production_qualified` stays false.


## 2026-10-18 cross-facility lot-trace release

The [current complete gate](qualification/release-20261018-lot-trace/summary.json) and [dated archive](qualification/release-20261018-lot-trace/summary.json) qualify image `sha256:a4927db4e168db7d54fba3778dcd1c34ba9d5fdfe53dc36e726c6a0f15959a00` on 150 frozen software inputs and eighteen migrations. Separate tests cover canonical trace reads, snapshot consistency, version-fenced pagination, original event/authorization forgery, tenant scope, MCP cursors and actual desktop/mobile operation. Archive restoration retains all nine nonempty trace sections and original review across actual Auckland/UTC application sessions; all 20 original quality requests leave every restored table hash unchanged.

The [HTTP report](performance/http-500-8-operators-lot-trace.json) and [provenance](performance/http-500-8-operators-lot-trace-provenance.json) use 500 products, 1,000 opening balances, 500 open orders, eight independent operators and two replicas. Timed traffic remains REST bin moves, post-save snapshots and exact original retries. This does not measure sustained trace reads, lot controls, expiry-heavy fulfillment, transfer-specific or MCP throughput.

| Observation | Recorded result |
|---|---:|
| Sustained interval / excluded setup | 300.173 / 205.163 s |
| Moves / exact replays / full reads | 9,178 / 9,178 / 9,186 |
| Throughput | 30.576 moves/s |
| Save/refresh p50 / p95 / p99 | 214.950 / 305.544 / 364.808 ms |
| Snapshot / write-attempt / replay p95 | 238.375 / 106.261 / 96.486 ms |
| Conflicts / unexpected errors | 0 / 0 |

Independent SQL and separate equations match 11,543 commands/events/activity, 10,187 movements, 11,545 outbox records, 100,000,013 each in custody and 253 reserved. Every actor progressed, all original seeded orders remained, both replicas passed full reconciliation and all 40 WMS tables were fingerprinted. Forced API/PostgreSQL recovery passed before the timed interval using the same immutable image as the container check.

The shared host is uncontrolled. Prior results, failed split interval and separate diagnostic remain retained; the original lock holder/root cause remains unconfirmed. No deployed capacity, controlled speedup, managed failover or facility SLO is inferred. `production_qualified` stays false.


## 2026-10-18 transfer-loss release

The [current complete gate](qualification/release-20261018-transfer-loss/summary.json) and [dated bundle](qualification/release-20261018-transfer-loss/summary.json) qualify image `sha256:cba329b82b07f4f29dbc439b7ad702b0019cd133866c05e288e38e99d598a571` on 154 frozen inputs, nineteen migrations and 41 tables. Loss settlement/reversal has separate original-source, role/scope, concurrent arrival, independent SQL cap, rollback/replay, prior-schema upgrade, MCP and browser checks. Archive restoration retains a nonzero loss and capped reversal with ten original loss-workflow requests, all twenty quality originals and nine nonempty trace sections across actual Auckland/UTC application sessions.

The [HTTP report](performance/http-500-8-operators-transfer-loss.json) and [provenance](performance/http-500-8-operators-transfer-loss-provenance.json) use 500 products, 1,000 opening balances, 500 open orders, eight independent operators and two replicas. Timed actions remain REST bin moves, full refreshes and exact cross-replica replays; they do not measure loss-journal, trace-read, expiry-heavy fulfillment, transfer-specific or MCP capacity.

| Observation | Recorded result |
|---|---:|
| Sustained interval / excluded setup | 300.124 / 146.053 s |
| Moves / exact replays / full reads | 9,948 / 9,948 / 9,956 |
| Throughput | 33.146 moves/s |
| Save/refresh p50 / p95 / p99 | 196.536 / 276.717 / 331.956 ms |
| Snapshot / write-attempt / replay p95 | 215.690 / 96.446 / 85.042 ms |
| Conflicts / unexpected errors | 0 / 0 |

Independent equations and SQL match 12,313 commands/events/activity, 10,957 movements, 12,315 outbox records, 100,000,013 each in custody and 253 reserved. Every operator progressed, all seeded orders remained, both replicas reconciled and all 41 tables were fingerprinted. Forced API/PostgreSQL recovery preceded timed traffic on the same immutable image as the container check. Prior slower/faster windows and the failed split interval remain intact. The original timeout's lock holder/root cause is unconfirmed; this shared-host result establishes no causal speedup, facility SLO or deployed capacity.


## 2026-10-18 reviewed-pack release

The [complete gate](qualification/release/summary.json) and [dated bundle](qualification/release-20261018-pack-units/summary.json) qualify image `sha256:7c74f8c03d743e2a8dd921fc55275dd9927501de55114ebc81695ef2e2034eee` on 161 inputs, twenty migrations and 43 tables. Pack conversions have separate original-source, authority, definition/observation race, SQL cap, rollback/replay, actual19 upgrade, MCP, browser and cross-timezone archive coverage. This window measures REST each-unit bin moves, full refreshes and exact retries; it does not establish pack-conversion or read-page capacity.

| Observation | Recorded result |
|---|---|
| Profile | 500 products, 1,000 opening balances, 500 open orders, eight independent operators, two replicas |
| Timed window | 300.157 seconds |
| Confirmed moves / exact replays | 10,591 / 10,591 |
| Complete snapshot reads | 10,599 |
| Moves per second | 35.285 |
| Save/refresh p95 | 254.244 ms |
| Independent tables | 43 |

See the [raw sanitized report](performance/http-500-8-operators-pack-units.json) and [source/image provenance](performance/http-500-8-operators-pack-units-provenance.json). All preceding windows, failures and lock diagnostics remain retained. The shared development host is uncontrolled; comparisons do not establish causality, production capacity or a managed failover SLO. No physical or deployed-client acceptance is implied.

## 2026-10-18 facility-evidence software release

The [retained report](performance/http-500-8-operators-facility-evidence-v2.json) and [provenance](performance/http-500-8-operators-facility-evidence-v2-provenance.json) bind the current 167-input software qualification and immutable image `sha256:5740c4e4fd3a89c47fba21f6770f4c17a18f2a4afa8d6c38e904fbf3efec74aa`. The 300-second profile uses 500 SKUs, 100 bins, two lots, 500 open orders, eight independent operators and two API replicas. It records 7,700 moves, 7,700 exact replays and 7,708 full reads over 300.388 seconds; 25.633 moves/s and 400.666 ms save/refresh p95, with 0 conflicts and 0 unexpected errors. Independent SQL/equations match 10,065 commands/events/activity, 8,709 movements, 10,067 outbox rows and all 43 table fingerprints.

Traffic remains REST each-unit bin moves and original-request recovery. It does not measure the new offline evidence validation/streaming path or pack, trace, transfer, package and MCP capacity. The stronger facility validator recomputes independently supplied sample rate/p95 and actor participation against a plan signed before work, but no physical facility samples were collected here. The uncontrolled shared host supports neither a causal performance comparison nor deployed capacity/SLO or managed failover claims. All previous success/failure windows remain retained; the earlier lock timeout's original root cause stays unconfirmed.


## Serial source diagnostic in development — 2026-10-19

The [raw diagnostic](performance/serial-source-diagnostic-20261019.json) and [provenance](performance/serial-source-diagnostic-provenance-20261019.json) retain original/current source identities, complete samples and query plans. Writes use real Store transactions and exact original replay; reads use the authenticated in-process API router. This excludes TCP, browser/device and sustained-concurrency costs. Run only on an owned isolated test database:

```bash
export WMS_SERIAL_PROFILE_OUTPUT="$(mktemp -d /tmp/wms-serial-profile.XXXXXXXX)/report.json"
export WMS_SERIAL_PROFILE_UNITS=16,64,128,256
export WMS_SERIAL_PROFILE_SAMPLES=3
export WMS_SERIAL_PROFILE_MOVES=10
WMS_KEEP_TEST_DB=1 bash scripts/local-postgres.sh cargo run --locked --example serial_profile
```

The executable refuses an existing report and writes partial stage/error evidence on failure. Each fixture registers one controlled SKU, two storage bins and the complete literal set, measures current/history/observation pages and full reconciliation, moves one original unit ten times, then repeats reads with retained history. Every original command must replay unchanged; selected source flags and full reconciliation must pass. The evidence comparison checks command/event/outbox/identity counts and global review heads; read handlers themselves use read-only transactions. This is a warmed, small-data diagnostic.

| Identities | Current page median, before / current (ms) | Full reconciliation median, before / current (ms) |
| --- | ---: | ---: |
| 16 | 87.854 / 48.292 | 272.552 / 177.879 |
| 64 | 335.097 / 265.872 | 1081.505 / 834.095 |
| 128 | 708.126 / 333.439 | 3028.172 / 2338.078 |
| 256 | unmeasured / 1006.073 | unmeasured / 8028.257 |

These independent runs occurred at different times on an uncontrolled shared host, alongside other synthetic checks. They do not establish a causal end-to-end speedup. A separate alternating same-fixture SQL experiment retains three samples per variant/query/size after warmup. At 128 identities its median original/current birth-event validation was 645.393 / 184.556 ms, and complete serial-source reconciliation was 3932.951 / 2869.024 ms; every source proof passed. Migration 023 removes repeated target/group queries and shares event-local observation binding without caching validity across snapshots. Source, actor/result, revision, ownership and complete historical-closure checks remain required.

A real 100-original-line dispatch regression retains the original ten-second statement timeout and proves both source equations after committing. It failed with timeouts in three earlier source capsules, which remain retained, before the shared validation change passed. This is separate from the diagnostic's ten single-unit moves. Remaining full-event validation per selected unit, historical bucket/closure and label traversal costs need further work; even 256 identities take seconds for a complete audit. No serialized sustained workload, production capacity, device/facility/native acceptance or complete new release gate is claimed.


## Share complete event proofs within serial reads — 2026-10-19

The [current diagnostic](performance/serial-shared-source-diagnostic-20261019.json) and [provenance](performance/serial-shared-source-provenance-20261019.json) retain the previous/current raw application profiles and 24 paired SQL samples. The executable and 16/64/128/256 fixtures above are unchanged. Every original command replays unchanged; authenticated source pages and full reconciliation must pass. The paired run alternates the exact previous complete serial query and the new query over the same retained data, keeps three rounds after warmup, requires identical complete proof JSON and rolls back its temporary results.

| Identities | Previous / shared complete serial-source query median (ms) |
| --- | ---: |
| 16 | 363.900 / 69.929 |
| 64 | 1125.394 / 127.857 |
| 128 | 2614.248 / 247.888 |
| 256 | 7030.970 / 606.378 |

The previous formula repeatedly checked the same step/observation sources through each current unit, each historical step and the full original event. The query now proves each original event once and separately retains all projection/latest-revision, missing scoped-source, mode/head, batch and physical checks. Selected current/history/observation pages also reuse their already-required event proofs; current custody references and the history current-source predicate remain independent. All stored SQL functions and migrations stay unchanged. A retained previous-query oracle and committed/rolled-back forensic fixtures verify complete results, selected flags, damage in a different unit of the same event, and fresh repair without a changed global review.

The separate current application run observed current-page medians of 28.416 / 74.709 / 162.598 / 458.676 ms and full Store reconciliation of 51.274 / 93.142 / 168.918 / 500.892 ms at those four sizes. These router/Store values include different work from the paired serial-only SQL query. The standalone-guard EXPLAIN diagnostic deliberately still calls the unchanged standalone predicate, rather than the new page query. Independent profiles occurred at different times; shared-host load and concurrent checks were uncontrolled. These samples do not establish a causal end-to-end speedup, capacity or facility SLO.

Initial full-set write/commit cost, repeated source checks during write admission, larger original events and historical buckets/closures, label traversal and serialized sustained workloads remain open. The 256-unit fixture's faster audit does not qualify large reviewed counts, physical/device/native acceptance or the complete release gate.


## Reuse original event payloads in serial write guards — 2026-10-19

The [raw diagnostic](performance/serial-write-payload-diagnostic-20261019.json) and [provenance](performance/serial-write-payload-provenance-20261019.json) retain two rollback-only paired SQL candidates, instrumented before/after application profiles, function counters and the separate current uninstrumented profile. PostgreSQL repeatedly decoded large original events inside each source guard. Migration 024 loads one uncompressed original payload per guard call while retaining every original condition and all23earlier migrations. The materialized-CTE candidate was measured and retained but not selected; a local value avoids a large payload tuplestore.

| Identities | Original / local unit-source median (ms) | Original / local step-source median (ms) |
| --- | ---: | ---: |
| 256 | 1255.666 / 467.853 | 992.955 / 473.604 |
| 512 | 4063.182 / 1372.854 | 3541.350 / 1414.146 |

These queries alternate definitions over the same retained rows, retain three samples after warmup and roll back every DDL change. All healthy predicates pass. Independent original dependency-graph tests also compare every per-row verdict after malformed and altered source facts. Neither query is a complete command or sustained workload.

Instrumented independent 256/512 full-count commands observed original 6078.084 / 16613.426 ms and current 1669.619 / 6880.219 ms. Their actual count COMMIT statements took 5960.723 / 16426.189 ms and 1569.942 / 6758.849 ms respectively. The earlier baseline program, whose telemetry collector subsequently failed, observed 3433.205 / 9727.917 ms, demonstrating substantial host variance. Function counters cover reads, plans and writes; overlapping inclusive times must not be added. No causal end-to-end speedup follows from these independent runs.

The separate current uninstrumented Store/router profile passes16/64/128/256/512identities, ten single-unit moves and fourteen unchanged original replays per fixture. Full-count command observations were126.686/394.602/1038.885/2317.101/7136.558ms. Complete sets were never truncated or split. The original ten-second statement limit remains unchanged; the test's old/new dependency comparison uses primary-key chunks under one snapshot so the oracle itself fits that limit. Larger original events, historical/closure work, realistic concurrency, reviewed large-count sessions and the full release gate remain unqualified.


## Sustained literal serial traffic: retained failed attempt — 2026-10-19

The [raw attempt](performance/serial-traffic-attempt-20261019.json), [per-cycle timings](performance/serial-traffic-attempt-cycles-20261019.jsonl) and [source provenance](performance/serial-traffic-attempt-provenance-20261019.json) retain an unthrottled five-minute native HTTP workload. Four distinct operators use two current debug API processes against one native PostgreSQL12primary. Eight controlledSKUs start with one lot and16literal each, across eight bins and eight open orders. Each operator repeatedly moves one fixed original label between two bins; the other124labels stay in place. There is no client think time or round barrier.

Traffic completed4665moves, exact cross-replica replays and post-save literal source reviews in300.220seconds, with zero unexpected traffic errors or stale-resource conflicts. Observed throughput was15.539moves/s; save/full-refresh/serial-review p95 was337.969ms. Initial review and subsequent replay are excluded from action latency. Throughput includes complete cycles and timing-journal writes. The raw timing journal retains the history-growth trend rather than only the aggregate.

The overall attempt **failed** at final complete API reconciliation: PostgreSQL exceeded the unchanged10second statement limit and returned503. A read-only60second diagnostic timeout collected an actual12756.068ms serial-subsection plan on the exact retained history. Its event-source CTE took12315.711ms for4703events. Function counters show12309.822ms inclusive in original event validation, with2874.677ms self-time in input matching and1669.529ms self-time in step-source binding. Inclusive counters overlap. This is a retained-history audit bottleneck, not a successful capacity qualification.

A separate immutable-row rebuild verifies every literal unit/lot/custody, dense predecessor chain, normalized original step, complete observed group and globalSKUbatch/head against the original journal:128units,4793steps and4673observations/batches. It does not substitute for the failed API proof. Preserve this exact history and fix complete validation without raising the product timeout, shrinking the workload, dropping original conditions or persisting verdict caches.

After building the native API, reproduce on an owned synthetic PostgreSQL server:

```bash
cargo build --locked --bin stateset-warehouse
WMS_KEEP_TEST_DB=1 bash scripts/local-postgres.sh bash scripts/serial-traffic-native.sh
```

The runner retains its unique database, original accepted-request and raw timing journals, reports and logs. A corrected10second smoke passed199moves; the first smoke's readiness race and source remain retained. The Python suite passes55cases, including four new protocol/evidence checks. The container resilience runner also accepts opt-in `WMS_RESILIENCE_SERIAL_UNITS`; that image-based serial path has not been run. This debug/shared-host native attempt does not establish image, crash/failover, deployed/device/native-receiver or facility acceptance. The full ten-stage gate and broader original-event/count/label/contention profiles remain open.


## Complete serial event history and batch lookups — 2026-10-19

The [raw report](performance/serial-event-source-diagnostic-20261019.json), [native cycle samples](performance/serial-event-source-cycles-20261019.jsonl) and [provenance](performance/serial-event-source-provenance-20261019.json) retain the successful repair of the failed4,665move corpus. Its4,703original events and every business-table fingerprint survive the index upgrade. Both complete API proofs pass in1641.305/1491.627ms; all4,703original requests replay unchanged. The product SQL statement limit stays10seconds.

The query resolves identical warehouse/command/input classes once per read snapshot and binds each ordinary event's own complete observations, source/effect callbacks, scoped step/batch counts, declared heads and literal membership in sets. The full original validator handles count and derived historical-closure cases and malformed envelopes. An event-leading batch index supplies the access path that SKU-leading keys could not bound. Total-map left joins preserve useful cardinality estimates; hashable null-safe keys distinguish absent commands from JSON null. No stored validity cache or inventory rewrite is introduced.

| Same retained4703event data, three alternating samples after warmup | Median serial query(ms) |
| --- | ---: |
| Intermediate shared resolution, no event-leading index |7821.863|
| Shared resolution and new index |4597.768|
| Set bindings and new index |1914.186|

Every original/current complete JSON proof matches. Final frozen-source tests compare every event flag and complete proof after26source-damage vectors and repair, across facilities and distinct input classes;200unique metadata commands exercise class cardinality. The intermediate five-argument read component remains compatible in migration025. A more complex nine-argument prototype was measured, retained and not adopted. Generic runner labels that misdescribe read-only/index-only experiments have separate annotations; the actual SQL and original metadata are preserved.

A fresh run with the identical unthrottled300second/four-operator/two-process/eight-SKU/128identity/eight-bin/eight-order profile completes **5434moves**, **5434exact cross-replica retries** and **5434literal source reviews** in **300.222seconds**, with zero unexpected errors/conflicts. Measured throughput is **18.100moves/s**, and save/full-refresh/literal-review p95 is **252.626ms**. Both final full source/physical API proofs pass. The timing and accepted-request journals permit independent recomputation of counts, percentiles, operator coverage and overlap.

The four clients move four fixed labels;124identities stay in place. There is no rate limit, think time or client-round barrier. Action latency excludes the initial snapshot and subsequent exact replay; throughput includes complete cycles and timing-journal writes. All own regression/browser/restore/benchmark jobs were terminal before the final interval. These are debug native/shared-host loopback measurements, not a production image, facility SLO, crash/failover, deployed/device/native-receiver or broad random-label/count capacity qualification.

316Rust cases,48repeatedPG16serial cases,tenlivebrowser workflows and actualPG12/PG16archives with49populated tables/26migration digests pass. The55Python cases are rerun;149offlinebrowser results retain earlier provenance for unchanged inputs. Earlier failed launchers, old-schema test setup, concurrent-regression API timeout and near-limit intermediate API results are retained. The exact full release gate/new image and broader original-event/count/label/facility profiles remain open.
