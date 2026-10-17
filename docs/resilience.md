# Sustained HTTP and crash recovery qualification

`scripts/resilience-rehearsal.sh` exercises the production image through actual HTTP against two independent API processes sharing one disposable PostgreSQL primary. It uses real mode, one administrator, four distinct operator identities by default, and a second isolated tenant. The rehearsal never uses production connection strings or configured native/webhook receivers.

Run it against an immutable image ID from the production container build:

```bash
WMS_RESILIENCE_IMAGE=sha256:<64-character-image-id> \
  bash scripts/resilience-rehearsal.sh
```

Python 3, Docker, and an already built image are required. Set `WMS_RESILIENCE_ARTIFACT_DIR` to an empty directory to choose where raw evidence is retained. The script rejects mutable application image tags.

| Variable | Default | Accepted range / meaning |
|---|---:|---|
| `WMS_RESILIENCE_SECONDS` | 60 | 60–3600 seconds of sustained traffic, excluding seeding and crash phases |
| `WMS_RESILIENCE_CLIENTS` | 4 | 2–16 distinct authenticated operator identities |
| `WMS_RESILIENCE_PRODUCTS` | 100 | At least client count, at most 2000 operational products; one extra product isolates recovery evidence |
| `WMS_RESILIENCE_OPEN_ORDERS` | 100 | 0–5000 open single-line orders; alternating orders are allocated |
| `WMS_RESILIENCE_LOTS` | 2 | 1–10 opening lot balances per operational product |
| `WMS_RESILIENCE_BINS` | 20 | 2–1000 storage bins |
| `WMS_RESILIENCE_POSTGRES_IMAGE` | `postgres:16-alpine` | Actual immutable database image ID and version are recorded |

Each opening bin/lot contains 100,000 disclosed synthetic each units. Catalog creation, counts, orders and allocations all use actual authenticated HTTP commands, with setup duration reported separately. The default seeds 200 opening balances and 100 active orders. To qualify the larger operational profile used by the Store workload, set products to 500, orders to 500, bins to 100 and lots to 2: this creates 1,000 opening balances, 250 allocated orders and 250 pending orders before any timed traffic. The additional recovery SKU has its own two-line receipt, complete five-unit shipment and three-unit outstanding allocation. The harness verifies these active orders remain present and unchanged throughout the rehearsal.

The run creates a uniquely labeled private Docker network, one uniquely labeled named data volume, and three uniquely labeled containers. API host ports are random loopback ports. API containers use UID 10001, read-only root filesystems, all capabilities dropped, and no new privileges. The PostgreSQL named volume deliberately persists across its container crash/restart. Cleanup verifies ownership labels before removing only this run's resources; it does not prune Docker resources or touch external databases. Raw artifacts contain disposable generated API credentials and remain private; only the sanitized `resilience.json` report belongs in `docs/qualification/`.

The rehearsal checks these failure boundaries:

1. A local discard proxy sends a receiving command to the actual API, observes its successful commit, and deliberately closes the caller connection without sending the success response. The caller retries the exact original envelope through the other API. The original result is replayed, the seven-unit receipt line remains seven, and another line with the same SKU remains untouched.
2. A dedicated PostgreSQL session holds the warehouse row lock. PostgreSQL's blocking PID graph must show the actual HTTP command waiting behind that session before the API receives SIGKILL. The other API remains ready. The killed request has no committed receipt, its original envelope commits once through the survivor, and the restarted API replays it. The client refreshes Docker's random port mapping after restart.
3. Another actual HTTP command waits behind the same database row-lock barrier. PostgreSQL receives SIGKILL while that transaction is pending. Both APIs must return readiness 503 while process liveness remains 200. PostgreSQL starts from the same named volume and reports unclean-shutdown recovery. Every WMS table's sorted-row SHA256 and row count must match its pre-crash fingerprint before retrying the pending command. Both API pools recover without an API restart. The original command commits once and replays through the other replica.
4. For at least the configured sustained interval, each operator moves its own SKU between two bins. Each concurrent round reads an actual operational snapshot and intentionally starts with its shared warehouse version, forcing stale-version conflicts. Every stale retry refreshes the actual operational snapshot through the other replica and verifies its operational reconciliation, complete catalog and active-work coverage. A definite 409 permits a reviewed new request; every success is immediately retried with its original envelope through the other replica. The harness reports successful physical moves, replay counts, stale conflicts, action latency from the round's initial snapshot through commitment including conflict recovery, snapshot read latency, and individual HTTP write-attempt latency. Subsequent exact replay latency is excluded from action latency, while sustained throughput includes every replay. Replays are not counted as physical throughput. Setup uses lightweight metadata version reads and remains outside the timed phase.

The final proof reads actual PostgreSQL rows independently of the application's reconciliation endpoint. It reconstructs every bin/lot's physical quantity from append-only movements, every reservation from allocation ownership, and received quantities from the exact receipt/line movement identity. It checks receive 18 minus ship 5 equals 13 recovery units in custody, three recovery units plus the seeded allocations reserved, all opening physical custody conserved, each operator bin's expected move parity, and every seeded order's exact pending/allocated status. It then checks one command receipt, event, activity entry and contiguous version for each successful request, original actor/result/command identity, exact OMS/TMS outbox ownership, zero unconfigured delivery attempts, tenant isolation and actor-bound replay rejection. Both replicas must also return successful full-ledger reconciliation.

Expected execution counts are derived from the configured fixture before comparing them to actual PostgreSQL rows. Setup commands equal `bins + 2 + products × (lots + 1) + open_orders + ceil(open_orders / 2)`: bins, the recovery product/receipt, operational product/count commands, orders and allocations. Recovery contributes exactly 13 committed commands. Each timed physical move contributes one further command; exact replays and definite rejections contribute none. Physical movement rows must equal `products × lots + 9 + committed_moves`: opening counts, two receipt lines, one putaway, two picks, two shipment allocation movements, two recovered crash moves and timed moves. Command/event/activity totals must match setup plus recovery plus timed work, with exactly two additional TMS outbox rows. The report includes both expected and observed counts and fails on any mismatch.

The report binds these results to the running immutable application image, that image's build-source fingerprint, both qualifier script hashes, database image/version, and checked migration hashes. A change to either qualifier script during execution fails the run. Local restart-to-readiness durations are observations for that run, not recovery SLOs. This is evidence for local process survival, transaction atomicity, request recovery and PostgreSQL crash durability. It does not qualify database failover, host loss, a production load balancer, deployed network latency, physical warehouse scanners, native receiver deployment, or facility throughput. No production throughput SLO is inferred from a synthetic fixture.

## Executed final release profile

The [checked final report](qualification/resilience.json), recorded on 2026-10-17, passed on production image `sha256:9ede733ef170f3b2eaaee7f97504cb3d2874d4d029f1d18e868c64e6d57b9c70` against isolated PostgreSQL 16.15. Its image/source fingerprint, both unchanged qualifier hashes and all eight migration digests were independently checked against the final frozen release. The checked artifact comes from the full release gate with every stage passing.

| Observation | Final recorded result |
|---|---|
| Active fixture | 500 operational products, 1,000 opening bin-lots, 100 storage bins, 500 open orders; 250 allocated and 250 pending |
| Recovery fixture | One additional product; same-SKU receipt lines of 7 and 11 units, a complete 5-unit shipment, and 3 units reserved |
| HTTP setup | 2,352 committed setup commands in 60.718 seconds; excluded from traffic timing |
| Sustained traffic | Four distinct operators through two API replicas for 60.349 seconds |
| Committed physical moves | 480, plus 480 exact retries through the other replica |
| Definite stale conflicts | 720; zero unexpected errors |
| Committed throughput | 7.954 physical moves/second, including snapshot refreshes, stale retries and exact replays in the timed interval |
| Action latency | p50 324.937 ms; p95 530.309 ms; p99 586.507 ms; measured from initial snapshot through commitment |
| Operational snapshot latency | 840 reads; p95 114.888 ms; every snapshot reconciled and retained the full active catalog/work set |
| API restart to readiness | Observed locally at 0.396 seconds; the other replica remained ready throughout the API failure |
| Database restart to both APIs ready | Observed locally at 1.047 seconds; both API processes recovered without restarting |

The lost-success proxy, deterministic blocked-write API SIGKILL and deterministic PostgreSQL SIGKILL phases all passed. While PostgreSQL was stopped, both replicas returned readiness 503 and process liveness 200. All 24 WMS tables had identical pre-crash and post-recovery sorted-row fingerprints before the original interrupted request was retried. The owned persistent volume survived the crash; afterward, all rehearsal containers, networks and volumes were removed and the label-filtered Docker lists were independently verified empty.

Independent SQL reconstruction ended with exactly 100,000,013 each units in physical custody and 253 reserved. The expected/observed execution models agreed on 2,845 command receipts, events and activity rows, 1,489 movements and 2,847 outbox rows. The 13 recovery commands and 480 timed physical commands account for every version after setup. All 500 seeded orders retained their original pending/allocated states, exact receipt lines retained 7 and 11 received units, both replicas passed full append-only-ledger reconciliation at version 2,845, and tenant/actor isolation passed. No external delivery was configured or attempted.

This is one synthetic local measurement window with forced same-version contention and synchronized rounds. It measures the recorded HTTP snapshot/command/replay path; it does not measure browser rendering, label handling or physical scanner timing. It is separate from the Store workload, which uses another database setup and refresh pattern, so their throughput or latency values do not establish a performance improvement. Production acceptance still requires deployed network and failover qualification, deployed native receivers, and an actual facility trial.
