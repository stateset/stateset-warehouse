# Operating and restoring StateSet Warehouse

This runbook describes procedures; the backup and restore commands below have not been executed as a deployment qualification. [Software evidence](evidence.md) records the checks actually run. Use PostgreSQL client tools matching the deployed server's major version.

## Deploy and establish the recovery point

Record the application commit/image digest, PostgreSQL version and applied `wms_schema_migrations` names/digests with the release. Configure real tenant/actor credentials, `WMS_DEMO=false`, database access and TLS at the ingress. `/health` is process liveness; `/ready` must return 200 before traffic is admitted. Native credentials and generic event webhooks are separate configuration; verify the intended OMS and TMS tenant mapping before enabling either.

Startup applies tracked migrations in a transaction and validates imported legacy evidence. Relational migration fences writes from old aggregate binaries. **Rolling back the executable does not roll back the database.** A rollback across that boundary requires a verified pre-cutover backup and a plan to reconcile every physical or remote action after it. Keep all old writers stopped during cutover.

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

For native work, inspect `GET /v1/warehouses/{warehouse}/native-requests/{request_id}` using the original actor. The receipt retains the frozen source/plan, response and attempt history. If `in_progress` is true, let its delivery lease finish or expire. Recover or retry the exact original import/transport body through its original route. An uncertain transport may already have created a planned Freight load; another key or revised plan cannot safely replace it. Review the retained request and remote receipt rather than editing the journal or clearing its lease manually.

The retained TMS intent pins its normalized endpoint and Freight tenant. Restore that authority before retrying if configuration changed. Before a new intent and every POST, the adapter checks Freight's actual `GET /v1/access` response for the retained `tenant_id` and `operations.write`. A credential may rotate within that same tenant; repointing a pending request to another endpoint or tenant is held. The Freight tenant's own OMS connection must still be qualified against the intended upstream source tenant.

`local_conflict` means the original local import command could not commit. Review the source, warehouse version and existing ownership before starting a new reviewed request. A completed receipt is immutable. OMS fulfillment writeback remains held until the upstream endpoint implements an atomic revision and terminal-state rule; changing a client header does not supply that missing guarantee.

## Resolve delivery and inventory incidents

Inspect `GET /v1/warehouses/{warehouse}/outbox` and the original `GET /v1/events/{event_id}` before changing receiver configuration. Correct the compatible consumer or its credentials, then use administrator `POST /v1/outbox/{outbox_id}/retry` for a dead letter. Requeue preserves the original event identity and prior attempt evidence. Consumers must deduplicate it and apply schema-version 2 stock changes per bin/lot version. Requeue does not authorize another warehouse movement.

When full reconciliation reports a difference, pause affected warehouse submissions, retain the versioned proof and original movement/receipt/order evidence, and verify the actual physical stock. Return staged stock through exact unpick/reset recovery before a justified count adjustment. Do not rewrite movement history or make stock and ledger caches agree through SQL: full historical reconciliation is deliberately independent of those caches. Administrative corruption or a restore gap needs a reviewed repair/recovery procedure before execution resumes.

Rotate API keys by preserving the tenant/actor identity, issuing a new secret, updating all instances and removing the old secret. Restart instances to replace startup-loaded credentials. Preserving actor identity lets the same operator recover an earlier request. Rotate upstream OMS API and Freight bearer credentials with their service owners, update `WMS_NATIVE_INTEGRATIONS_JSON` and restart affected instances. Preserve the pinned Freight tenant and endpoint for retained intents; its credential requires `operations.write`. Native configuration contains credentials, while retained request evidence must not.

## Facility acceptance

Before live use, record physical counts, receiving-to-handoff and short-pick cases using the actual facility, operators and devices; verify cross-service tenant mapping and replay/deduplication; and complete the restore rehearsal above. Whole each units, explicit lots and complete-order shipments are the supported execution contract. Physical agreement, production throughput and recovery-point suitability require facility-owned evidence.
