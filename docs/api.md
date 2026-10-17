# API and integration contract

All `/v1` routes require `Authorization: Bearer <credential>`. The server resolves tenant, actor and role from configuration. Passing tenant/actor headers or command fields does not change identity. Responses containing warehouse data use `Cache-Control: no-store`. Request bodies are limited to 256 KiB.

## Routes

| Method | Route | Result |
|---|---|---|
| GET | `/health` | Process liveness; public |
| GET | `/ready` | 200 if a database query succeeds, otherwise 503; public |
| GET | `/v1/session` | Verified identity, explicit demo mode and configured receiver status |
| GET | `/v1/warehouses` | Accessible warehouse metadata and versions |
| GET | `/v1/warehouses/{id}` | Versioned warehouse state with reconciliation from that exact snapshot |
| GET | `/v1/warehouses/{id}/reconciliation` | Per-bin/lot ledger and reservation reconstruction, checked version and overall validity |
| POST | `/v1/warehouses/{id}/commands` | Atomic warehouse transition |
| GET | `/v1/warehouses/{id}/commands/{request_id}` | Original command receipt for this actor; `{found:false}` if not committed |
| GET | `/v1/warehouses/{id}/outbox` | Latest 200 tenant-scoped destination records |
| GET | `/v1/events/{event_id}` | Committed original event envelope for inspection/consumer recovery |
| POST | `/v1/outbox/{outbox_id}/retry` | Admin-only dead-letter requeue, with retained attempts and audit entry |

Unknown/cross-tenant entities return 404. Unauthorized requests return 401; forbidden roles 403; stale versions/reused keys 409; invalid transitions 422; database availability failures 503. Axum rejects malformed JSON/extraction errors with its standard non-JSON error body; clients must handle unreadable error bodies safely.

## Commands

Every command uses this envelope:

```json
{
  "request_id": "c62d2a33-b2ce-46a8-9f4e-b0d471b9cc7c",
  "expected_version": 0,
  "command": {
    "type": "create_product",
    "sku": "TEE-001",
    "name": "Cotton Tee",
    "category": "Apparel",
    "reorder_point": 20
  }
}
```

Request IDs contain 8–100 ASCII letters, digits, hyphens or underscores. Persist the **entire original envelope** before submission. Retry that exact envelope after an uncertain result; never generate a new key or update its expected version to retry an uncertain write. Successful receipts return `{replayed:false,result:{entity_id,version,type}}`; exact retries return the same result with `replayed:true`, even if the warehouse has since advanced. Another payload or actor using that key is rejected.

For a known version conflict, refresh and review the work, then submit a **new** request ID with the reviewed version. A receipt lookup returning `found:false` does not prove an earlier network request can never arrive: retry the original key. Do not repeat a physical action merely because its response was lost.

| Command type | Required command fields | Permission |
|---|---|---|
| `update_warehouse` | `code`, `name`, `city`, `timezone` (IANA) | admin |
| `create_product` | `sku`, `name`, `category`, `reorder_point` (0–1,000,000) | admin |
| `create_location` | `code`, `zone`, `kind` (`storage` or `receiving`) | admin |
| `create_receipt` | `reference`, `supplier`, `lines:[{id,sku,lot,quantity}]` | operator/admin |
| `receive` | `receipt_id`, `line_id`, `quantity`, `location` (receiving bin) | operator/admin |
| `move_stock` | `sku`, `lot`, `from`, `to` (storage bin), `quantity` | operator/admin |
| `count_stock` | `sku`, `lot`, `location`, `counted` (0–1,000,000), `reason` | admin |
| `create_order` | `reference`, `source_order_id` (nullable), `customer`, `destination`, `priority` (`normal`/`high`/`urgent`), `lines:[{id,sku,quantity}]` | operator/admin; OMS requires source ID |
| `allocate` | `order_id` | operator/admin |
| `pick` | `order_id`, `line_id`, `location`, `lot`, `quantity` | operator/admin |
| `pack` | `order_id` | operator/admin |
| `ship` | `order_id`, `carrier`, `handoff_reference` | operator/admin |
| `cancel_order` | `order_id`, `reason` | operator/admin; OMS only for source-linked work |
| `report_exception` | `order_id` (nullable), `category`, `detail` | operator/admin |
| `resolve_exception` | `exception_id`, `resolution` | operator/admin |

Unknown fields are rejected. Nonempty text is at most 240 UTF-8 bytes, without surrounding whitespace/control characters. Receipt/order line arrays contain 1–100 lines with unique immutable IDs. All ordinary quantities are whole each units between 1 and 1,000,000. SKU/lot identity is case sensitive. Use an explicit `UNTRACKED` lot when no lot control applies. No fractional rounding or implicit case conversion occurs.

## Inventory and lifecycle semantics

`on_hand` is physical warehouse custody, including receiving and outbound staging. `reserved` is the subset held for outstanding order lines. A bin/lot must satisfy `0 <= reserved <= on_hand`. Only storage locations can supply new allocations. Picking transfers on-hand/reserved units from storage to `STAGE-01`; handoff removes the staged stock and consumes its reservation.

Orders transition `pending → allocated → picking → picked → packed → shipped`; a one-step complete pick can go directly from `allocated` to `picked`. `pending`/`allocated` may transition to `cancelled`, releasing only that order's reservations. Partial receipts transition `expected → partial → received`. Partial shipments are not supported: packing requires all lines picked. Blocking exceptions prevent allocation/pick/pack/ship; resolving one records evidence and does not manufacture stock.

## OMS / TMS event receiver contract (proposed)

This is a newly implemented WMS producer contract, **not a verified existing receiver endpoint** in Sync or Freight. Native consumer adapters are still needed in those projects; they have not been modified here. EDI continues to own X12 translation and transport.

Configure receivers in real mode only:

```dotenv
WMS_WEBHOOK_TENANT=your-org
WMS_OMS_WEBHOOK_URL=https://your-oms.example/integrations/wms/events
WMS_OMS_WEBHOOK_TOKEN=an-independent-receiver-secret
WMS_TMS_WEBHOOK_URL=https://your-tms.example/integrations/wms/events
WMS_TMS_WEBHOOK_TOKEN=another-independent-receiver-secret
```

URLs require HTTPS, forbid embedded credentials, and do not follow redirects. URL and token must be configured together; tokens require at least 16 characters. The tenant must exist in the credential mapping. This initial configuration supports one delivery tenant per process. Other tenants' events remain pending, never routed using that tenant's receiver credentials.

The producer sends `POST <configured URL>`, JSON body, bearer token, and `Idempotency-Key: <event ID>`. The envelope includes:

```json
{
  "schema_version": 1,
  "id": "stable-event-uuid",
  "tenant": "your-org",
  "warehouse_id": "wh-main",
  "version": 17,
  "type": "fulfillment.packed",
  "actor": "operator:alice",
  "occurred_at": "2026-10-17T12:00:00Z",
  "request_id": "original-command-uuid",
  "result": {"entity_id":"fulfillment-uuid","version":17,"type":"fulfillment.packed"},
  "order": "full order snapshot, or null for non-order commands",
  "inventory": "full warehouse bin/lot balance projection at this version",
  "command": "original typed command"
}
```

The string descriptions above stand in for the actual JSON objects/arrays. Download real event JSON from Integrations for an exact fixture.

Every committed command queues an OMS event. Packing and shipping also queue TMS readiness/handoff events. Delivery is at least once and may arrive out of order after retries. Consumers must persist/deduplicate event IDs, apply warehouse projection snapshots only when their version is newer, and retain per-order event identity separately. An older warehouse projection must not overwrite a newer one, but a previously unseen shipment still needs its business effect considered. A TMS readiness event does not authorize another warehouse stock decrement.

The HTTP timeout is 10 seconds, lease 30 seconds, with eight attempts and exponential backoff before dead letter. The same event ID survives retries/lease recovery/requeue. HTTP 2xx means the receiver accepted the envelope; it does not mean downstream order or transport execution completed. Delivery attempts and administrative requeues are retained independently of current queue status.
