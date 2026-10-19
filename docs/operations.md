# Operating and restoring StateSet Warehouse

The automated rehearsal below qualifies an isolated synthetic database recovery. It does not establish a deployed facility's recovery point or physical inventory agreement. [Software evidence](evidence.md) records which checks actually ran. Use PostgreSQL client tools matching the server's major version.

## Select and create facilities

The top-bar warehouse button shows the active facility's code and city, including on mobile. Open it to select a facility, refresh the directory or load another page. Admins can choose **New warehouse** and supply its code, name, city and IANA timezone. Creation starts an empty catalog and inventory with receiving/staging bins and retains an immutable creation receipt. Each tenant role applies across that tenant's warehouses; selecting another facility keeps the same authenticated identity.

First login opens the server-declared bootstrap facility, even when it is outside the first directory page. The current browser session remembers subsequent selections separately for each tenant/operator. A retained physical or native request takes priority after reload: recovery opens its original warehouse and preserves its original URI, request ID, version and payload. A creation that never reached the server can display an existing workspace while its new warehouse URI remains in the pending journal. Use **Check original save** for that request. Switching and further writes stay held until its outcome is resolved. Preserve unreadable recovery data for review rather than deleting it to bypass the hold.

A switch closes old forms and searches, clears loaded history and proofs, and resets scanner/native selections before the new warehouse opens. A failed read or a response identifying another warehouse cannot complete the switch. Verify the visible facility before scanning or counting; each facility's catalog, stock, receipts, orders and versions are independent. Inter-facility transfers retain original dispatch lines, in-transit custody and separate actual destination arrivals; a loss journal or reversal does not receive stock.

## Review serial identities before saving

For a controlled SKU, confirm the original action, line, bin and lot shown in the serial review before scanning each physical unit. Two groups for the same SKU can belong to different order lines or transfer lines; preserve those original groups. Package quantities entered in packs still require every resulting each identity. Unpack, handoff and RMA cancellation use the original derived identities. Cancellation releases only units not yet received.

If a save becomes uncertain, use **Check original save** after reload. The retained journal keeps the original facility, request ID, physical/resource reviews, pack observations and serial groups. Resolve that exact request before reviewing another action. A concurrent count can change identities without changing the total quantity; a rejected stale review needs a fresh physical review. Destination transfer activity can also expire a source serial page while its warehouse version remains unchanged; refresh that serial review explicitly.

## Automated recovery and release qualification

Run the rehearsal against a PostgreSQL server on which the supplied role can create databases. It creates two uniquely named `wms_restore_source_test_*` and `wms_restore_target_test_*` databases, then drops only those databases. It does not copy or modify an existing warehouse database. The artifact directory must be empty; retained evidence is private to the current user.

```bash
export WMS_RESTORE_ADMIN_URL="$WMS_TEST_DATABASE_URL"
export WMS_PG_BIN=/usr/lib/postgresql/16/bin
export WMS_RESTORE_ARTIFACT_DIR="$(mktemp -d /tmp/wms-restore-evidence.XXXXXXXX)"
bash scripts/restore-rehearsal.sh
```

The harness seeds multiple tenant scopes, creates a second facility through its journaled command, and executes receiving, putaway, same-SKU order lines, picking, packing, carrier handoff, source-bin and staging reservations. A separate partly shipped order closes all remaining demand after physical recovery, retains its cancellation source and actual return allowance, and coexists with another order’s active packing and backorder. It retains an original command receipt, a completed native import, an uncertain transport, a rejected conditional OMS receipt followed by a separately reviewed uncertain OMS intent, their attempts, and pending outbox work. It then creates a real custom-format `pg_dump` archive, records its listing/checksum, restores it into the fresh target with `pg_restore --exit-on-error --single-transaction`, and opens the target through the application migration checks.

Qualification requires identical row counts and content hashes for **every WMS table**, identical warehouse versions and migration names/digests, and complete movement/receipt/allocation reconciliation for every restored warehouse. The probe retries the original facility creation, receiving and native import request IDs, requires their exact original results, and proves those retries changed no evidence. It checks actor-scoped receipt access, replays the terminal rejected OMS receipt unchanged, reclaims both exact uncertain native intents into retained attempts, and commits a new physical count followed by independent reconciliation. Native retries test journal recovery only; this rehearsal sends no external HTTP and does not certify OMS or TMS acceptance.

The serial recovery fixture retains all six nonempty identity tables and thirteen literal identities across on-hand, loose/packaged staging, shipped/partially returned, in-transit, lost and missing custody. It includes an unused controlled SKU with a zero head, original pack-plus-serial observations followed by a changed pack definition, unchanged full counts and a witnessed empty count. Restore must preserve exact per-unit lineage, original observation pages, global SKU reviews and every original actor-bound request/result. Migration 022 pins the serial place validator's helper lookup because `pg_restore` clears its session search path; the applied 021 source is unchanged.

Post-restore checks reject changed actors/payloads, duplicate cross-facility identities, changed tracking mode and obsolete pack observations without changing any WMS table. They reverse the exact lost identity without moving stock, receive the recovered/in-transit identities, and expire source-facility serial pages while leaving its warehouse version unchanged. They cancel only an unreceived RMA lease, authorize/receive that same unit again, unpack/repack/handoff exact staged owners, recover a missing unit through a complete count, and observe the restored zero head's first identity. All old and new original requests must then replay without table changes. Every warehouse undergoes independent historical reconciliation.

A separate isolated upgrade test builds the actual schema 21 predecessor, records nonempty serial history, then opens it through the application migration manager. It requires every existing data table and all original 21 migration ledger rows to remain unchanged, and verifies original request replay after a new current-schema movement. The PostgreSQL qualification role must be able to create these uniquely named test databases, as it already must for the archive rehearsal.

The expanded rehearsal requires every WMS table to contain recovery-point rows. It writes a nonempty aggregate snapshot under the actual initial schema, then lets the application migrate and import it. The retained snapshot and original shipped-order source must survive unchanged; old aggregate insert/update/delete attempts must remain fenced. New relational execution and exact replay must preserve that original snapshot. Open and resolved order exceptions are created through commands; an open exception blocks allocation without changing table hashes, and a separately reviewed post-restore resolution permits allocation.

The proposed-event outbox fixture uses an owned loopback HTTP receiver with a checksummed file journal. A real 503 retains a failed attempt; changed receiver configuration holds delivery even after an audited retry. A later original-recipient retry succeeds. Another worker is interrupted after its claim commits and the receiver records the event, preserving a started attempt at the recovery point. After restore, lease expiry retains that attempt as uncertain and changed-recipient delivery stays held. Wrong-role/tenant retries and an obsolete lease completion change no tables. An audited original-recipient retry with a rotated token preserves the event/payload and receiver fingerprint; the local receiver deduplicates the already recorded event without another effect. Original audit rows remain present and immutable. The seed and continued receiver journals are separate retained files.

Database timestamp comparisons normalize to UTC inside read-only transactions while source and target sessions deliberately remain Auckland and UTC. The report still lists empty recovery-point tables and now rejects any: all 49 current tables, including the six serial tables, must be nonempty. Migration 023 changes only source-validation computation; the archive must preserve all 23 migration digests. Synthetic archive and loopback checks remain separate from physical counts, deployed incident recovery, scanner hardware, native receiver acceptance and sustained serialized capacity.

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

Traverse all warehouse-directory pages using `next_after` before comparing the restored scopes. Include empty created facilities and their original creation receipts. Compare warehouse versions and full proof rows with the saved recovery point; verify representative completed receipts/orders, command receipts, native requests/attempts and pending outbox records. In the isolated copy, run a recorded receiving-to-handoff and uncertain-request recovery case. Retain the results before authorizing cutover. A restore to an older point requires review of post-backup physical movements and downstream acceptance before operators or delivery workers resume.

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

## Resource reviews, replenishment and customer returns

A physical review is scoped to the stock/work it touches when the request carries `resource_versions`. Unrelated work can advance the warehouse version without invalidating it. Relevant balance changes, changed-and-restored balances, witnessed counts even with no quantity delta, receipt/order updates and order exceptions advance monotonic database counters. Preserve the entire original envelope, including those counters. Never replace its map with current values to recover an uncertain physical action. The API contract defines the exact identity/map checks required before a `changed_resources` rejection may retire a journal; the legacy stale-version proof alone cannot retire a scoped request.

Committed request receipts and administrator audit records cannot be updated, deleted or truncated. Exact replay does not wait for the warehouse write lock. Do not remove resource-counter or command-history tables during maintenance. A restored backup can predate later physical/remote effects and browser journals; keep traffic stopped until that cutoff is independently reconciled, as required above. The restore rehearsal retains an actual customer return and replays its original scoped receiving request after restoration.

Use Inventory's pick-face replenishment planner to review a SKU, destination, minimum/target available stock and optional reserve zone. Plans neither reserve nor move units. Confirm actual bin/SKU/lot/quantity, save one movement, then replan. Already reserved units and other configured pick faces do not fund a plan; unfilled demand stays visible. These are per-review rules, not a scheduled replenishment policy.

For a customer return, open the original shipped fulfillment and review its remaining line/lot allowance. Authorize an RMA reference with observed reason/inspection evidence, then receive its exact line through Receiving or Scanner. Keep returned units in receiving until inspected and approved for putaway. Authorization alone is not physical custody. Completed and archived RMAs continue to consume the original shipment's allowance less their frozen cancelled remainders. Original shipment and OMS evidence are retained; refunds and automated quality disposition are not supplied by this workflow.

To stop an expected or partially received delivery, open its canonical receipt and choose **Cancel unreceived remainder**. Review the received quantities, explain why the rest will not arrive, and confirm. The cancelled receipt keeps every original expected quantity, received movement and its frozen remainder. Received goods still need inspection/putaway; cancellation is not a stock reversal or a refund. Receiving and scanner actions for that receipt stop. A fully received receipt has nothing to cancel. Any later delivery needs a new receipt and, for an RMA, a fresh exact source-line/lot authorization. Only the unreceived cancelled RMA quantity becomes available again. Recover a lost cancellation response through its original request journal; do not create another cancellation to resolve uncertainty. Archive qualification retains a partially received cancelled receipt and replays its original cancellation after restoration without altering any table hashes.

Original shipment quantities and source/carrier facts are protected after handoff. Return review states whether its allowance comes from the original recorded event, a retained legacy snapshot or a synthetic fixture. A baseline is not a newly verified physical handoff. Inspect the original handoff and physical records before using a legacy allowance; synthetic fixture quantities are for testing. Conditional OMS send requires a recorded event and cannot use a baseline.

If upgrade admission reports missing, ambiguous or differing original shipment evidence, retain the failed migration and reconcile the original event/snapshot, normalized lines/allocations and outbound movements before retrying. Do not fabricate an event or copy current counters into a baseline to pass the check. The ledger flags shipment disagreements separately and includes their provenance in the exported all-history proof. A forensic guard bypass does not release the first-send fence: an inconsistent source retains its native journal with zero remote attempts. The actual archive rehearsal restores immutable source records and their original provenance before replaying physical/native requests.

The cancellation migration refuses existing receipts whose terminal state or normalized return ownership/expected quantity disagrees with the recorded authorization. Preserve the failed upgrade evidence and reconcile the original receipt, shipment and authorization through the reviewed repair procedure before retrying; do not bypass the guard or delete authorizations to free allowance. After upgrade, original receipt-line identities/expectations cannot be rewritten, even before cancellation.

## Facility acceptance

For scanner qualification, configure a keyboard-wedge device that sends each printed work reference, bin, SKU and lot as its literal, case-sensitive value followed by Enter. The WMS does not claim GS1, EAN or SSCC parsing, case conversion or unit conversion. Print and scan the explicit `UNTRACKED` lot label when the work uses an untracked lot.

Receiving requires selecting the exact receipt line and receiving bin; picking requires selecting the exact allocation. Confirm bin, SKU and lot in that order, then enter a whole-each integer quantity and explicitly submit the action. A matching label does not itself record a movement. When work is stale, reload the canonical work and rescan its labels before submitting. When a response is lost, recover or retry the original journaled request; another physical scan is not the recovery procedure.

Before live use, record physical counts, receiving-to-handoff and short-pick cases using the actual facility, operators and devices; verify cross-service tenant mapping and replay/deduplication; and complete the restore rehearsal above. Whole each units, explicit lots, complete-order or explicitly reviewed package shipments are the supported execution contract. Physical agreement, production throughput and recovery-point suitability require facility-owned evidence.

Use the [GET-only native deployment preflight](native-acceptance.md) and [signed facility evidence validator](facility-acceptance.md) to retain reproducible checks bound to the reviewed tenant, facility, warehouse, release and independently trusted key. Neither tool silently fills missing observations or declares production qualified. The facility owner must establish the physical facts and authorize the operating limits.


## Dispatch and receive between facilities

Configure each whole-each SKU at source and destination. At the source, open **Facility transfers → New facility transfer**, select the destination and exact storage SKU/lot/bin lines, enter each quantities, carrier and handoff reference, then review and record the actual handoff. Reserved and staged order stock cannot leave through a transfer. Dispatch reduces source availability and leaves the goods in transit; it creates no destination available balance.

At the destination, open the same transfer, verify its original line/lot and actual arrived each quantity, select a receiving bin and record arrival evidence. Receive partial quantities when observed; the remainder stays in transit. Put away verified receiving stock through Inventory before allocating it. Completed transfers remain accessible through **Include completed transfers** and cursor pages. Refresh the full transfer list if work changed while paging. The original journal recovers lost dispatch/arrival responses and retains the original warehouse across reloads; never create another handoff or arrival merely to resolve uncertainty.

A transfer-evidence alert holds further receiving. Export full reconciliation and compare original immutable source/arrival events and exact movements, without editing the original ownership or adding a replacement movement. Shortage, damage or loss must remain visible as an exception and unresolved in-transit quantity until an approved disposition workflow accounts for the actual goods. Confirmed losses use the separately reviewed administrator journal below. Transfer rerouting and automatic quality disposition remain unsupported. Facility acceptance must separately witness both physical sides and the company-wide custody balance.


## Pick and hand off an order in separate packages

Open the canonical order in **Fulfillment** and use **Shipment packages and backorders**. **Allocate available stock** reserves actual available storage stock and leaves unavailable demand open. Pick the exact line/bin/lot units physically moved to staging. Refresh the shipment review, enter a unique package reference and exact picked quantities, then **Confirm packed shipment**. Packing protects these staged units from another package or unpick; it does not record departure.

Choose **Hand off** for that package, enter the actual carrier and handoff reference, and confirm that the carrier physically received its exact contents. The order remains partly shipped until all original units depart. Receive/put away later stock, allocate the remaining demand, pick and pack another package as observed. Multiple handoffs retain separate carriers and evidence; the order summary directs you to the packages instead of inventing one carrier.

For a packing error, **Unpack** with evidence first; goods remain staged. Use the existing physical-return/unpick procedure to return exact units to their original source, then **Release unpicked reservations** with a reason if storage holds should be released. These recovery actions remain available under a blocking exception; forward packing/handoff remains held. Resolving an exception alone does not change stock or record departure.

**Review return allowance** is available after the first actual handoff. Authorize only the original shipped line/lot; staged or backordered units cannot be returned. Existing RMA inspection, receipt cancellation and putaway rules continue to apply. A lost packing/handoff response uses **Check original save** and its retained request, including after reload; never hand the goods over twice to resolve uncertainty.

The current native OMS connection cannot represent multiple/partial handoffs and holds those orders before sending a fulfillment request. Review the original package evidence and use a separately qualified receiver contract that preserves each shipment before enabling such writeback. One complete package can use its actual event with the existing conditional receiver. Cancelling a partly picked/shipped commercial remainder is not supported; keep the unresolved demand visible. Facility acceptance must witness partial stock availability, two handoffs, physical recovery and an early return on actual devices.

## Recover a database timeout

A `503` telling the client to retry the same request ID leaves the save unresolved. Retain the original warehouse URI, actor, request ID, reviewed versions and full command. Use **Check original save**, including after reload, to recover its committed receipt or retry that exact envelope. A timeout alone does not prove rejection and does not authorize another physical move or carrier handoff. The normal stale-review proof is required before replacing a definitively uncommitted review.

An administrator can correlate the response's request ID with API and PostgreSQL logs. Capture the active transaction and blocking relationship while the wait exists, using the established database monitoring access:

```sql
SELECT pid, state, wait_event_type, wait_event,
       pg_blocking_pids(pid) AS blockers, xact_start, query_start, query
FROM pg_stat_activity
WHERE datname = current_database()
  AND pid <> pg_backend_pid()
  AND (state <> 'idle' OR xact_start IS NOT NULL)
ORDER BY xact_start NULLS LAST, pid;
```

For isolated qualification investigations, start an explicitly recorded read-only sampler before the timed phase; the helper removes its owned services when it exits. Retain raw wait graphs and host-pressure samples privately and publish sanitized reports with the observer's scope and hashes. A timeout waiting for the warehouse head does not identify its holder or cause by itself. Keep failed windows and their missing-proof limits visible even when another run passes. The [retained timeout attempt](qualification/attempt-20261018-split-fulfillment-timeout/failure-context.json) and [same-image diagnostic replay](performance/http-500-8-operators-split-diagnostic-provenance.json) demonstrate this distinction; they establish no facility availability or capacity SLO.


## Review expiry and quarantine

Open **Inventory → Lot expiry and quarantine**. Review the exact SKU/lot, physical custody, reservations, usable storage, required-label policy, recorded expiry and independent active hold IDs. Enable required expiry only after reviewing the product and existing stock. An operator can record the first exact label and place an urgent hold; an administrator corrects expiry, changes the requirement or releases one exact hold. Enter the observed reason, review the captured facility/version and explicitly confirm the evidence checkbox. Recover a lost response with the original journal and body before starting another action.

A date-only printed label does not define an instant. Apply the facility's reviewed label/cutoff/timezone policy and enter the exact offset-bearing time; the application does not guess midnight or use the device clock. At the exact server cutoff the lot stops being usable. Refresh failed or stale observations before forward work. A hold remains active until its own reviewed release; releasing one cannot clear another or override an expired/missing required label. Metadata changes do not alter physical counts or release reservations.

If already allocated/staged goods become unavailable, stop picking/packing/handoff and retain the actual stock. Unpack protected packages, physically return the exact unshipped allocation units, then release/reset the appropriate unpicked reservations or close eligible remaining demand. Unavailable stock can still be counted, received and moved to reflect actual custody. Preserve earlier actual handoffs and their return allowances. A policy change is not disposal, a refund, a new receipt or evidence of destruction. Automated quality disposition is not supplied.

Receive actual inter-facility arrivals against their original lines. The destination conservatively retains known earlier expiry and independent restrictions from the original dispatch and current facilities. An arrival can be physically received while unavailable. Source quarantine is warehouse-local: inspect earlier destination arrivals and apply a separate destination hold when required; this feature does not implement a recall across all warehouses. Retain conflicts and resolve labels through attributed administrator corrections rather than changing physical quantities.

Ledger quality evidence reports original policy changes and admission decisions separately from current availability. A legitimate held/expired lot can retain a healthy original custody history. A failed/missing original proof requires incident review and holds new fulfillment; do not alter history, delete a hold or fabricate a baseline to turn it green. Upgrade admission rejects unattributed preexisting required-label policy. Retain the failed upgrade and original records for reconciliation. During restore rehearsal compare exact policies, active/released holds, original decisions, staged packages, actual handoffs and transit across every facility; replay originals without changes before separately observed recovery work.


## Trace a lot across facilities

Open **Inventory → Trace a lot across facilities** and enter the exact SKU and lot. Review physical custody, reserved ownership, receiving/staged positions and usable storage independently. Inspect all nine sections as needed: facilities, positions, allocations, receipts, shipments, transfers, movements, policies and decisions. A transfer destination may appear before arrival; dispatched minus actual arrived minus net recorded loss remains in transit. Active packing is not a customer handoff. Legacy/synthetic shipment baselines remain explicitly labelled and do not invent an observed handoff. Shared SKU/lot labels do not establish manufacturer batch genealogy.

Keep the original review while switching sections and use **Load more trace records** for original pages. A source change discards retained pages and requires explicit **Refresh trace review**. Changed-and-restored work also invalidates the old review. Each page reports its own server observation time; expiry can make earlier availability stale without new work. Paging/export is disabled when the most recent successful observation is older than one minute; refresh before continuing. Earlier loaded records retain their own observation times. Confirm the newly observed policy before a separately reviewed hold/release; a trace fingerprint cannot authorize a command.

Retain source and reconstruction warnings as incidents. Balanced quantities cannot clear a failed original-source proof. Signed divergent ledger quantities remain evidence, never usable stock or a healthy reconciliation. A green page covers the shown records and reported physical/reservation reconstruction, not every unloaded historical record or physical reality. Inspect exact original handoffs and return authorizations instead of inferring shipped units from a current status or carrier label.

The JSON export contains only the loaded selected section, its loaded/total counts, original review and per-page observation/source flags. `section_complete` is true only after all records in that section are loaded; `global_trace_complete` stays false. Partial exports must remain labelled partial and cannot be represented as a complete cross-facility recall report. Read/export actions create no stock, requests, holds or physical observations. A later source quarantine does not automatically hold earlier destination arrivals: inspect every affected facility and apply independently authorized holds when appropriate.


## Review a confirmed transfer loss

Keep an unconfirmed shortage in transit while investigating. A destination administrator reviews the carrier/controller evidence, exact original line/SKU/lot, physical arrivals, remaining transit and incident reference. Open **Facility transfers → Loss settlements and reversals → Review loss journal**, then **Review loss for line …**. Enter the confirmed each quantity, carrier incident reference and observed reason; confirm the evidence checkbox. This records an attributed loss claim in warehouse ownership, not an observed receipt, disposal movement, refund or carrier settlement. Check that source/destination stock and reservations stay unchanged.

When unresolved transit reaches zero through a mixture of arrivals and losses, use **Include completed transfers** to inspect it. The status says transit was settled with recorded loss rather than claiming a complete physical arrival. Keep original loss and reversal history; do not edit or replace the dispatch, inflate arrivals or count stock to hide the missing quantity.

To correct an error or account for located goods, review the exact original loss and select its reversal action. Reverse only the quantity supported by new evidence, retaining the original incident and reason. Reversing reopens that line's unresolved transit and credits no warehouse stock. Record a separate actual destination arrival only after physically observing the goods, then inspect and put away through ordinary custody workflows. Earlier expiry/quarantine is retained. There is no per-unit serial identity or invented link from a reversal to a particular arrival.

Loss journal pages retain the original destination review. A source-side read can become stale even while the source's own version is unchanged. Discard changed pages and explicitly refresh the transfer and journal before new work. Source failures hold settlement/reversal/receiving for incident review. The shared pending-save banner retains the original transfer/line or loss ID, quantity, incident and reason. Recover the original request after uncertainty before starting a separately reviewed action. JSON reconciliation and lot-trace exports retain loss totals and journal counts; they do not include every unloaded original loss record.


## Reviewed case and pallet entry

Inventory → Pack units lets an administrator review an exact SKU and pack code, the current original definition, a proposed whole each count and evidence before confirming. Read actual packaging labels and independently verify the factor. Use the same code only when it identifies the intended packaging; changes create new definition IDs. Disable obsolete codes explicitly. Stock totals and native integrations remain in each units.

At a quantity field, explicitly choose Each or a reviewed whole pack. Changing units clears the entry and requires the observed count again; the form displays the exact resulting each quantity. A changed or disabled definition requires a fresh review and rescanning where applicable. Use each input for loose-unit quantities; do not approximate fractional packs or measured goods. Operators cannot change pack definitions.

If a response is lost, retain the entire original journal and use Check original save. Its original count, factor and definition remain visible even after a later size change. Never substitute the latest factor or repeat the physical action to recover a save. An unreadable conversion leaves the original browser data retained and writes paused. Page original definitions and observations with their returned review/cursor; stale or failed source checks require a new review. Reconciliation failures need original-source investigation rather than a stock count intended to hide a conversion discrepancy.
