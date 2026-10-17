# Warehouse operations visibility

`GET /v1/operations/health` gives warehouse administrators and operators a tenant-local summary of delivery work, native recovery and open warehouse exceptions. `GET /v1/operations/metrics` exposes the same snapshot as Prometheus text for an administrator credential. Viewers and integration credentials cannot access either route; operators cannot scrape metrics. Both routes use the authenticated credential's tenant. Query parameters cannot select another tenant.

The Operations view is operational evidence, rather than an inventory reconciliation or a facility capacity certification. `/health` remains process liveness. `/ready` checks database connectivity. A successful operations read means that the database snapshot was available; it does not certify that every downstream system has accepted its work. A failed database read returns 503 rather than a fabricated empty queue.

## Snapshot contract

Health returns `schema_version: 1`, `snapshot_at`, `outbox`, `native` and `warehouses`. Counts and ages come from one read-only, repeatable-read PostgreSQL transaction with a single database timestamp. Queries have a three-second statement timeout and the whole operation has a five-second deadline, including acquiring a database connection. No inventory, delivery or native intent row is changed. Tenant-leading indexes support the aggregates without retrieving retained JSON payloads.

`outbox.groups` always contains the eight combinations of `destination` (`oms`, `tms`) and `status` (`pending`, `delivering`, `delivered`, `dead_letter`), including zero counts. Each group contains `count`, `due`, `active_leases`, `expired_leases`, `oldest_age_seconds` and `oldest_due_age_seconds`. The outer object includes tenant-wide totals, `oldest_pending_age_seconds`, and `receivers: [{destination, configured}]` for OMS and TMS.

Generic event receiver configuration is separate from native OMS/TMS adapters. Warehouse commands retain outbox events even while a receiver is intentionally unconfigured. The receiver flags report only whether this running instance has a generic destination configured for this credential's tenant; no URL or token is returned. They do not certify remote compatibility or reachability. Gate backlog/due/dead-letter alerts on the corresponding configured flag, and do not diagnose intentionally held events as a failed delivery worker. Configuration is loaded at startup and held constant alongside the database snapshot.

- `due` counts pending work whose `due_at` has arrived, plus deliveries whose lease has expired. Future retries and active leases are excluded.
- `oldest_age_seconds` measures time since the immutable event was committed. `oldest_pending_age_seconds` is the greatest such age among pending and delivering work, including future retries and active leases.
- `oldest_due_age_seconds` measures time since pending work became due, or since a delivery lease expired. This is the useful measure for a stalled worker alert. Ages are nonnegative whole seconds and zero when no matching row exists.
- Dead letters are retained for administrator review and requeue. A metrics read never retries a delivery.

`native.groups` always contains the eighteen combinations of `kind` (`oms_import`, `tms_transport`, `oms_fulfillment`) and `status` (`frozen`, `sending`, `uncertain`, `completed`, `local_conflict`, `rejected`). Each group contains `count`, `active_leases`, `expired_leases`, `oldest_age_seconds` since original intent creation, and `oldest_updated_age_seconds` since the last journal state change. The outer object includes `total`, `uncertain`, `rejected`, `local_conflict` and `requires_review`.

`requires_review` counts retained uncertain, rejected and local-conflict receipts. It is a review-history count, not an acknowledgement tracker or a count of currently unresolved incidents. Terminal rejected receipts remain present after a separately reviewed replacement succeeds. Do not page repeatedly merely because retained rejection history is nonzero. An uncertain receipt may already have applied remotely: recover its original request and frozen payload instead of creating another request. An expired sending lease is a candidate for recovery, not proof that the remote operation failed.

`warehouses` contains tenant-wide `total` and `open_exceptions`, plus at most 100 `items`, each with `warehouse_id`, `version` and `open_exceptions`. Names, customers, actors, SKUs, source payloads, errors, request identities and remote responses are absent. Page through warehouse rows with `?limit=50&after=<last-warehouse-id>`. The default limit is 50; the permitted range is 1–100. `next_after` is null at the end. Tenant totals and queue counts remain tenant-wide on every page. Each page is a new snapshot: refresh all pages when a coordinated cross-page view matters. Unknown parameters, duplicate parameters and invalid limits/cursors fail explicitly.

## Prometheus scrape

Metrics have a fixed set of 112 samples per credential, including zero groups. Only `destination`, `status` and `kind` appear as application labels. Tenant, warehouse, actor, SKU, event and request identities never become labels. Each scrape belongs to one credential's tenant; configure a separate scrape job for each intended tenant. Keep the credential in a protected file and restrict its distribution to the monitoring service. Treat the resulting counts and warehouse identifiers as private operational information even though payloads and secrets are redacted.

```yaml
scrape_configs:
  - job_name: stateset-wms-example
    scheme: https
    metrics_path: /v1/operations/metrics
    scrape_interval: 30s
    scrape_timeout: 7s
    authorization:
      type: Bearer
      credentials_file: /run/secrets/wms-monitoring-token
    static_configs:
      - targets: [warehouse.example.internal]
```

The families are `wms_outbox_receiver_configured`, `wms_outbox_items`, `wms_outbox_due_items`, `wms_outbox_active_leases`, `wms_outbox_expired_leases`, `wms_outbox_oldest_pending_age_seconds`, `wms_outbox_oldest_due_age_seconds`, `wms_native_requests`, `wms_native_oldest_age_seconds`, `wms_native_oldest_updated_age_seconds`, `wms_native_active_leases`, `wms_native_expired_leases`, `wms_warehouse_count` and `wms_open_exceptions`. All are gauges, including the retained receipt counts; none is an application throughput counter. Do not use `rate()` to claim physical actions per second from these gauges.

The endpoint accepts no query parameters and returns `Cache-Control: no-store`. Access it over the same private TLS ingress and authentication controls as the API. Do not publish an unauthenticated reverse-proxy metrics alias.

## Review policies and alerts

[Example alert rules](../deploy/monitoring/alerts.yml) provide actionable policies for missing scrapes, overdue event deliveries, dead letters, old uncertain native requests, expired native leases and newly recorded rejected/local-conflict outcomes. Their timing values are editable review-policy examples, not a promised facility SLO. Qualify them against actual retry schedules, staffing, downstream availability and agreed response ownership before enabling notifications. The rules never retry work or change business state.

The rejection/local-conflict alert uses a positive fifteen-minute gauge change, so retained history alone does not page forever. It cannot detect outcomes that predate the first scrape. Keep durable application evidence and a review process; monitoring is not an acknowledgement database. Likewise, an absent metrics target must be detected through Prometheus `up`, rather than interpreting absent queue series as zeros.

For a delayed outbox, verify the configured receiver and inspect the original outbox/event evidence; requeue dead letters only after the receiver is ready to deduplicate. For uncertain native work, inspect the original actor's retained native receipt and its attempts, then recover the exact original request. For open inventory exceptions, review the affected warehouse and run its complete reconciliation before physical adjustments. [The operating runbook](operations.md) describes these procedures.

`deploy/monitoring/alerts.test.yml` contains synthetic Prometheus rule cases for all six alerts, including suppression for intentionally unconfigured receivers and retained historical rejections. Run `bash deploy/monitoring/check.sh` with Docker access; the script pins the official Prometheus test image by digest, provides a disposable memory-backed temporary directory and disables container networking. With a matching local `promtool`, the equivalent checks are `promtool check rules deploy/monitoring/alerts.yml` and `promtool test rules deploy/monitoring/alerts.test.yml`.

Actual application coverage is in `tests/operations.rs`: role/authentication rejection, strict query parsing, real PostgreSQL queue/lease ages, fixed metric cardinality, paginated warehouse totals, tenant isolation, redaction and unchanged retained evidence. A multi-session test holds a database lock at the second aggregate, observes PostgreSQL's actual lock wait, commits new delivery/version/exception rows from another transaction, and verifies that the in-flight snapshot consistently retains the earlier view. The next read must show the new committed view.
