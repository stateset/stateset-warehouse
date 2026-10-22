# Deploying to production

A step-by-step checklist for running StateSet Warehouse with real inventory. Each step links to the detailed reference. The project's own qualification is local and synthetic. **Before relying on a deployment, run your own [facility acceptance](facility-acceptance.md).**

## 1. Prerequisites

- **PostgreSQL 16** is recommended; it is the version CI and the release images test against. Versions 12 and later are supported for development. Use a dedicated database, with point-in-time backups enabled.
- **A TLS-terminating ingress** (load balancer or reverse proxy). The API speaks plain HTTP on its bind address; browsers need HTTPS for any remote access.
- **A secret manager** for API keys, WorkOS settings and integration credentials.
- **The container image**, built from a release tag with `docker build -t stateset-warehouse .`. Record the image digest with the release.

## 2. Run the container safely

The image runs as UID 10001 and passes the release gate's hardening check with these flags:

```bash
docker run --read-only --cap-drop=ALL --security-opt=no-new-privileges:true \
  --tmpfs /tmp:rw,nosuid,nodev,size=16m \
  -e DATABASE_URL=... -e WMS_BIND=0.0.0.0:8088 -e WMS_DEMO=false \
  -e WMS_API_KEYS_JSON=... \
  -p 8088:8088 stateset-warehouse@sha256:<digest>
```

The image's built-in health check polls `/ready`. Two or more replicas can share one database; writes to one warehouse are serialized by a row lock, so replicas don't need coordination.

## 3. Configure

| Variable | Required | Purpose |
|---|---|---|
| `DATABASE_URL` | yes | PostgreSQL connection. The role must be able to create the WMS schema; migrations run at startup. |
| `WMS_BIND` | yes | Listen address, e.g. `0.0.0.0:8088` inside a container. |
| `WMS_DEMO` | yes: `false` | `true` serves a synthetic demo tenant and refuses real integrations. Production must be `false`. |
| `WMS_API_KEYS_JSON` | for service or API-key access | Map of secret key (at least 32 characters) to `{tenant, actor, role}`. Roles: `viewer`, `operator`, `admin`, `oms`. Use one key per person or service so every action is attributable. |
| `WMS_WORKOS_JSON` | for browser sign-in | WorkOS AuthKit organizations mapped to tenants and roles. See [authentication](authentication.md). |
| `WMS_MCP_JSON` | for remote MCP clients | Allowed hosts and origins for `/mcp`. Loopback only by default. See [MCP](mcp.md). |
| `WMS_NATIVE_INTEGRATIONS_JSON` | for Sync OMS / Freight TMS | Per-tenant OMS and TMS connections. OMS writeback stays held until you opt in. See [native integrations](native-integrations.md). |
| `WMS_OMS_WEBHOOK_URL`, `WMS_OMS_WEBHOOK_TOKEN`, `WMS_TMS_WEBHOOK_URL`, `WMS_TMS_WEBHOOK_TOKEN`, `WMS_WEBHOOK_TENANT` | optional | Generic event receivers that implement the [event contract](api.md#warehouse-event-receiver-contract). `WMS_WEBHOOK_TENANT` is required when a receiver is set. |

Startup validates all of this **before** opening the database, and fails fast on bad configuration or a port conflict.

## 4. First start and migrations

- On startup the API applies any new migrations in one transaction, then checks each applied migration's digest. A changed migration stops startup.
- **Rolling back the executable does not roll back the database.** Take a verified backup before every upgrade; see [operations](operations.md#deploy-and-establish-the-recovery-point).
- A real tenant starts with an empty `MAIN` warehouse and its receiving and staging bins. Load products, locations, and opening stock through receipts or counted inventory, never by direct SQL.

## 5. Health and traffic

- `/health` is liveness: the process is up.
- `/ready` returns 200 only when the database is usable. Admit traffic on `/ready`, not on an open TCP port.
- On SIGTERM or Ctrl-C the API stops accepting new connections, lets in-flight requests finish, and gives the delivery worker up to 15 seconds to stop.

## 6. Monitoring

- Scrape `GET /v1/operations/metrics` (Prometheus text) with an administrator credential. Use [deploy/monitoring](../deploy/monitoring) for the tested alert rules. See [observability](observability.md).
- Alert on dead-lettered deliveries, uncertain native requests and open exceptions.
- Run `GET /v1/warehouses/{id}/reconciliation` on a schedule. `ok: false` is an incident: stop affected work and investigate before continuing. Its serial portion takes about 4 seconds at 16,000 labels; the 10-second database statement limit applies throughout.

## 7. Backups and restore drills

Back up continuously, and **rehearse a restore into a fresh database before go-live**. A restore is complete only when reconciliation passes and every in-flight request has been resolved. Follow [back up and restore](operations.md#back-up-and-restore-into-a-fresh-database).

## 8. Go-live acceptance

1. Run the [facility acceptance](facility-acceptance.md) plan with real scanners, real stock and signed observations.
2. Qualify each integration in staging first: [deployed OMS and Freight acceptance](native-acceptance.md).
3. Keep the demo off, use individual credentials, and confirm alerts reach a person.

## 9. Upgrades

1. Read the [changelog](../CHANGELOG.md) for behavior changes.
2. Pause operators and integrations, let in-flight requests finish, and take a verified backup.
3. Deploy the new image digest. Watch `/ready` and the startup log for migration results.
4. Run reconciliation, then resume work.
