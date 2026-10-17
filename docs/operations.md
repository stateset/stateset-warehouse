# Operating and restoring StateSet Warehouse

The automated rehearsal below qualifies an isolated synthetic database recovery. It does not establish a deployed facility's recovery point or physical inventory agreement. [Software evidence](evidence.md) records which checks actually ran. Use PostgreSQL client tools matching the server's major version.

## Automated recovery and release qualification

Run the rehearsal against a PostgreSQL server on which the supplied role can create databases. It creates two uniquely named `wms_restore_source_test_*` and `wms_restore_target_test_*` databases, then drops only those databases. It does not copy or modify an existing warehouse database. The artifact directory must be empty; retained evidence is private to the current user.

```bash
export WMS_RESTORE_ADMIN_URL="$WMS_TEST_DATABASE_URL"
export WMS_PG_BIN=/usr/lib/postgresql/16/bin
export WMS_RESTORE_ARTIFACT_DIR="$(mktemp -d /tmp/wms-restore-evidence.XXXXXXXX)"
bash scripts/restore-rehearsal.sh
```

The harness seeds two tenant scopes and executes receiving, putaway, same-SKU order lines, picking, packing, carrier handoff, source-bin and staging reservations. It retains an original command receipt, a completed native import, an uncertain transport, a rejected conditional OMS receipt followed by a separately reviewed uncertain OMS intent, their attempts, and pending outbox work. It then creates a real custom-format `pg_dump` archive, records its listing/checksum, restores it into the fresh target with `pg_restore --exit-on-error --single-transaction`, and opens the target through the application migration checks.

Qualification requires identical row counts and content hashes for **every WMS table**, identical warehouse versions and migration names/digests, and complete movement/receipt/allocation reconciliation for every restored warehouse. The probe retries the original receiving and native import request IDs, requires their exact original results, and proves those retries changed no evidence. It checks actor-scoped receipt access, replays the terminal rejected OMS receipt unchanged, reclaims both exact uncertain native intents into retained attempts, and commits a new physical count followed by independent reconciliation. Native retries test journal recovery only; this rehearsal sends no external HTTP and does not certify OMS or TMS acceptance.

Success writes `release-qualification.json` with archive and executable/lockfile hashes, release revision, recovery duration and individual results. `recovery-point.json`, `verification.json`, `archive.list`, `warehouse.dump`, its checksum and stage logs support independent review. Failure exits nonzero and retains the stage evidence; a readable archive listing alone cannot pass qualification. `WMS_KEEP_RESTORE_DB=1` retains the synthetic databases for diagnosis. No success report should be inferred from a sandbox-blocked or skipped rehearsal.

The [unified release gate](release-qualification.md) runs the PostgreSQL/browser tests, actual archive round-trip, monitoring rule checks, production container and [sustained HTTP/crash rehearsal](resilience.md). GitHub Actions invokes this same command on Ubuntu 24.04 with matching PostgreSQL 16 tools. Local results remain independently reproducible when organization billing prevents a hosted job from starting. The production image runs as UID 10001 with a read-only filesystem, all capabilities dropped, and new privileges disabled. Its runtime check requires container health, authenticated reconciliation, an unauthenticated 401, built dashboard and graceful shutdown. The image's health check uses `/ready` on its standard internal port 8088; publish a different host port when needed.

Run that same container qualification locally with Docker access:

```bash
export WMS_CONTAINER_ARTIFACT_DIR="$(mktemp -d /tmp/wms-image-evidence.XXXXXXXX)"
bash scripts/container-smoke.sh
```

The script creates its own PostgreSQL 16 container and private Docker network, publishes the API only on a random loopback port, and removes only resources carrying its unique ownership label. PostgreSQL data lives on a disposable in-memory mount. It neither needs an existing database URL nor connects to a warehouse database. The check compares build-input hashes before/after compilation, runs the resulting immutable image ID, verifies that the running container has that same image ID, validates the non-root identity and kernel `NoNewPrivs`/effective capabilities, checks all applied migration digests and requires graceful SIGTERM exit. It retains `container-qualification.json`, image/tool identities, source hashes, readiness/reconciliation and stage logs. Local container qualification is independent of whether GitHub can start its CI job.

## Deploy and establish the recovery point

Record the application commit/image digest, PostgreSQL version and applied `wms_schema_migrations` names/digests with the release. Configure real tenant/actor service credentials or [WorkOS membership mappings](authentication.md), `WMS_DEMO=false`, database access and TLS at the ingress. `/health` is process liveness; `/ready` must return 200 before traffic is admitted and returns 503 after a bounded two-second database check. Native credentials and generic event webhooks are separate configuration; verify the intended OMS and TMS tenant mapping before enabling either.

Startup validates authentication, native integration and event receiver configuration before opening the database. It then reserves the configured listener, so a port conflict fails before migration or tenant provisioning. The listener does not serve HTTP until database initialization succeeds; use HTTP `/ready`, not a TCP connection alone, to admit traffic. The listening log records the actual bound address, including the selected port when `WMS_BIND` uses port zero.

Startup then applies tracked migrations in a transaction and validates imported legacy evidence. Relational migration fences writes from old aggregate binaries. **Rolling back the executable does not roll back the database.** A rollback across that boundary requires a verified pre-cutover backup and a plan to reconcile every physical or remote action after it. Keep all old writers stopped during cutover.

For a planned cutover, pause warehouse operators, OMS submissions and native transport requests. Stop all WMS instances gracefully and let active requests finish before taking the final backup. For ordinary online backups, `pg_dump` provides a database snapshot, but a restored snapshot still needs reconciliation with physical movements and remote acceptances that happened afterward.

## Back up and restore into a fresh database

Use protected storage and existing credential handling; do not put real secrets in the runbook or shell history. Set `WMS_BACKUP_PATH` to a new archive path, `WMS_RESTORE_DATABASE` to a new isolated database name, and the restore URLs to that same database server/name. The restore target must be empty and separate from the running database.

```bash
pg_dump --format=custom --file "$WMS_BACKUP_PATH" --dbname "$DATABASE_URL"
pg_restore --list "$WMS_BACKUP_PATH" > "$WMS_BACKUP_PATH.list"
sha256sum "$WMS_BACKUP_PATH" > "$WMS_BACKUP_PATH.sha256"

createdb --maintenance-db="$WMS_RESTORE_ADMIN_URL" "$WMS_RESTORE_DATABASE"
pg_restore --exit-on-error --single-transaction --no-owner --no-privileges \
  --dbname "$WMS_RESTORE_DATABASE_URL" "$WMS_BACKUP_PATH"
```

The restore role owns the restored objects when `--no-owner` is used; provision its intended application privileges explicitly. Retain the archive, checksum, release identity, expected warehouse versions and pre-backup reconciliation results. A successful archive listing establishes readability, not a successful restore.

Start the exact recorded application against the restored database, on an isolated loopback address. Set `WMS_BINARY` to its absolute path and configure temporary API keys for the original tenant/actor identities. Use a clean rehearsal working directory without `.env` settings in it or its parents; the API's dotenv loader can otherwise reintroduce unset receiver configuration. Keep native services and webhook delivery disabled during the rehearsal:

```bash
env -u WMS_OMS_WEBHOOK_URL -u WMS_OMS_WEBHOOK_TOKEN \
  -u WMS_TMS_WEBHOOK_URL -u WMS_TMS_WEBHOOK_TOKEN \
  WMS_NATIVE_INTEGRATIONS_JSON='{}' \
  DATABASE_URL="$WMS_RESTORE_DATABASE_URL" WMS_BIND=127.0.0.1:18088 \
  WMS_DEMO=false WMS_API_KEYS_JSON="$WMS_RESTORE_API_KEYS_JSON" \
  "$WMS_BINARY"
```

From another shell, verify readiness and every restored tenant's warehouse using its own credential. Set `WMS_RESTORE_API_TOKEN` and `WMS_WAREHOUSE_ID` to the relevant restored identity and warehouse:

```bash
curl --fail --silent --show-error http://127.0.0.1:18088/ready
curl --fail --silent --show-error \
  -H "Authorization: Bearer $WMS_RESTORE_API_TOKEN" \
  http://127.0.0.1:18088/v1/warehouses
curl --fail --silent --show-error \
  -H "Authorization: Bearer $WMS_RESTORE_API_TOKEN" \
  "http://127.0.0.1:18088/v1/warehouses/$WMS_WAREHOUSE_ID/reconciliation" \
  | jq -e '.ok == true'
```

Compare warehouse versions and full proof rows with the saved recovery point; verify representative completed receipts/orders, command receipts, native requests/attempts and pending outbox records. In the isolated copy, run a recorded receiving-to-handoff and uncertain-request recovery case. Retain the results before authorizing cutover. A restore to an older point requires review of post-backup physical movements and downstream acceptance before operators or delivery workers resume.

## Recover an uncertain request

For an ordinary warehouse save, inspect `GET /v1/warehouses/{warehouse}/commands/{request_id}` using the original actor. Retry the entire original command envelope when necessary. Preserve its request ID, payload and expected version; do not repeat a physical scan because the response was lost.

If the server returns the matching `command_outcome` stale-version proof described in the [API contract](api.md), the original save was not committed and its old expected version fences delayed copies. The dashboard retires that journal, refreshes the work and asks for physical review before a new save. It keeps the journal for generic conflicts, denied access, malformed/mismatched proof or unknown outcomes. A receipt lookup with `found:false` is insufficient to discard an uncertain request.

For native work, inspect `GET /v1/warehouses/{warehouse}/native-requests/{request_id}` using the original actor. The receipt retains the frozen source/plan, response and attempt history. If `in_progress` is true, let its delivery lease finish or expire. Recover or retry the exact original import/transport body through its original route. An uncertain transport may already have created a planned Freight load; another key or revised plan cannot safely replace it. Review the retained request and remote receipt rather than editing the journal or clearing its lease manually.

If the original browser journal is unavailable, an authorized database incident responder can inspect the tenant-scoped native journal to identify the original actor, warehouse, request and frozen input. Preserve that evidence and restore access for the original actor before using the normal recovery route. Tenant-wide Operations counts deliberately omit these identities and cannot substitute for the original request. Do not update journal rows to assign recovery to another actor.

The retained TMS intent pins its normalized endpoint and Freight tenant. Restore that authority before retrying if configuration changed. Before a new intent and every POST, the adapter checks Freight's actual `GET /v1/access` response for the retained `tenant_id` and `operations.write`. A credential may rotate within that same tenant; repointing a pending request to another endpoint or tenant is held. The Freight tenant's own OMS connection must still be qualified against the intended upstream source tenant.

`local_conflict` means the original local import command could not commit. Review the source, warehouse version and existing ownership before starting a new reviewed request. A completed receipt is immutable.

OMS fulfillment remains held by default. Enable `shipment_writeback: "conditional_v1"` for a tenant only after deploying and qualifying the companion receiver's tenant-bound conditional-revision, terminal-state, durable-idempotency and durable-rejection contract. The legacy unconditional fulfillment endpoint is never used. Submit a physical shipment through `POST /v1/warehouses/{warehouse}/orders/{order}/oms-fulfillment` with a new `request_id` and reviewed `expected_version`; the WMS requires matching committed shipment evidence and freezes the source revision, full handoff and recipient before any POST. A 424 during initial preparation means that preparation sent no OMS POST. Once a request is retained or attempted, recover its original body/key and journal receipt.

An `uncertain` OMS outcome retains exclusive ownership of the physical event and must not be replaced by another request. A verified, durable `rejected` receipt proves the particular predicate did not apply; it and its finished attempt remain immutable. Review the current OMS order and physical handoff before explicitly initiating another request. The new intent receives another remote request UUID while retaining the same shipment event. The WMS never rebases an old intent automatically or alters its source snapshot. A completed or rejected original request always replays its original receipt.

## Resolve delivery and inventory incidents

Start with the Operations view or authenticated `GET /v1/operations/health`. Review configured-receiver flags, oldest due work, expired leases, uncertain native requests and open warehouse exceptions. A retained rejected receipt is historical evidence; an uncertain receipt requires recovery of its original request. The panel refreshes while visible and online, marks failed/offline/older-than-one-minute observations stale, and disables stale snapshot exports. Its counts never retry work. The [metrics and alert policies](observability.md) describe administrator-only scraping and example policies with actual rule tests.

Inspect `GET /v1/warehouses/{warehouse}/outbox` and the original `GET /v1/events/{event_id}` before changing receiver configuration. Correct the compatible consumer or its credentials, then use administrator `POST /v1/outbox/{outbox_id}/retry` for a dead letter. Requeue preserves the original event identity and prior attempt evidence. Eight claimed attempts, including crashed or uncertain sends, exhaust automatic delivery. An expired eighth lease becomes a dead letter without a ninth send; the retired attempt remains uncertain, and a late response cannot replace it. Review receiver evidence before authorizing another cycle. Consumers must deduplicate it and apply schema-version 2 stock changes per bin/lot version. Requeue does not authorize another warehouse movement.

Delivery also retains the first attempted URL's fingerprint. If a URL change holds an event, restore its original URL before audited retry; credentials can rotate at that endpoint. Requeue cannot redirect an attempted event. Migration 008 conservatively marks historical attempted deliveries with no recorded receiver as `unverified`; pending/expired items then become dead letters with no HTTP send. Reconcile those event IDs against independently retained receiver receipts and deployment history before designing a reviewed recovery migration. The application provides no endpoint override for that missing evidence. Completed items are not resent, and new or never-attempted work can still proceed. Upgrade every delivery worker: after migration 008, a legacy worker cannot commit its next delivery claim because it lacks the matching transaction-local receiver assertion. A send already in flight at migration time may still finish; its remote result remains part of the historical reconciliation.

When full reconciliation reports a difference, pause affected warehouse submissions, retain the versioned proof and original movement/receipt/order evidence, and verify the actual physical stock. Return staged stock through exact unpick/reset recovery before a justified count adjustment. Do not rewrite movement history or make stock and ledger caches agree through SQL: full historical reconciliation is deliberately independent of those caches. Administrative corruption or a restore gap needs a reviewed repair/recovery procedure before execution resumes.

Rotate API keys by preserving the tenant/actor identity, issuing a new secret, updating all instances and removing the old secret. Restart instances to replace startup-loaded credentials. Preserving actor identity lets the same operator recover an earlier request. Rotate upstream OMS API and Freight bearer credentials with their service owners, update `WMS_NATIVE_INTEGRATIONS_JSON` and restart affected instances. Preserve the pinned Freight tenant and endpoint for retained intents; its credential requires `operations.write`. Native configuration contains credentials, while retained request evidence must not.

## Facility acceptance

For scanner qualification, configure a keyboard-wedge device that sends each printed work reference, bin, SKU and lot as its literal, case-sensitive value followed by Enter. The WMS does not claim GS1, EAN or SSCC parsing, case conversion or unit conversion. Print and scan the explicit `UNTRACKED` lot label when the work uses an untracked lot.

Receiving requires selecting the exact receipt line and receiving bin; picking requires selecting the exact allocation. Confirm bin, SKU and lot in that order, then enter a whole-each integer quantity and explicitly submit the action. A matching label does not itself record a movement. When work is stale, reload the canonical work and rescan its labels before submitting. When a response is lost, recover or retry the original journaled request; another physical scan is not the recovery procedure.

Before live use, record physical counts, receiving-to-handoff and short-pick cases using the actual facility, operators and devices; verify cross-service tenant mapping and replay/deduplication; and complete the restore rehearsal above. Whole each units, explicit lots and complete-order shipments are the supported execution contract. Physical agreement, production throughput and recovery-point suitability require facility-owned evidence.

Use the [GET-only native deployment preflight](native-acceptance.md) and [signed facility evidence validator](facility-acceptance.md) to retain reproducible checks bound to the reviewed tenant, facility, warehouse, release and independently trusted key. Neither tool silently fills missing observations or declares production qualified. The facility owner must establish the physical facts and authorize the operating limits.
