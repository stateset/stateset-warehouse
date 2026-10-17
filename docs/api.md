# API and integration contract

All `/v1` routes require `Authorization: Bearer <credential>`. The server resolves tenant, actor and role from configuration. Passing tenant/actor headers or command fields does not change identity. Responses containing warehouse data use `Cache-Control: no-store`. Request bodies are limited to 256 KiB.

## Routes

| Method | Route | Result |
|---|---|---|
| GET | `/health` | Process liveness; public |
| GET | `/ready` | 200 if a database query succeeds, otherwise 503; public |
| GET | `/v1/session` | Verified identity, explicit demo mode and configured receiver status |
| GET | `/v1/warehouses` | Accessible warehouse metadata and versions |
| GET | `/v1/warehouses/{id}` | Consistent operational snapshot: active work, bounded completed work, recent history and an operational balance check |
| GET | `/v1/warehouses/{id}/reconciliation` | Full append-only movement, allocation and exact receipt-line proof, checked version and overall validity |
| GET | `/v1/warehouses/{id}/history/{kind}?limit=50&before={cursor}` | Keyset history for `orders`, `receipts`, `movements`, `activity` or `exceptions` |
| GET | `/v1/warehouses/{id}/orders/{order_id}` | Authoritative order detail, including archived work |
| GET | `/v1/warehouses/{id}/receipts/{receipt_id}` | Authoritative receipt detail, including completed receipts |
| POST | `/v1/warehouses/{id}/commands` | Atomic warehouse transition |
| GET | `/v1/warehouses/{id}/commands/{request_id}` | Original command receipt for this actor; `{found:false}` if not committed |
| GET | `/v1/warehouses/{id}/outbox` | Latest 200 tenant-scoped destination records |
| GET | `/v1/events/{event_id}` | Committed original event envelope for inspection/consumer recovery |
| POST | `/v1/outbox/{outbox_id}/retry` | Admin-only dead-letter requeue, with retained attempts and audit entry |

Unknown/cross-tenant entities return 404. Unauthorized requests return 401; forbidden roles 403; stale versions/reused keys 409; invalid transitions 422; database availability failures 503. Axum rejects malformed JSON/extraction errors with its standard non-JSON error body; clients must handle unreadable error bodies safely.

History pages return `{items:[{cursor,data}],next_cursor}` ordered by descending sequence. `limit` is 1–100, default 50. `before` is an exclusive positive cursor; use `next_cursor` unchanged for the next page. A null cursor means the end of that traversal. New records can appear above an existing cursor; refresh the first page to see them. History is retained independently of the bounded operational snapshot.

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
| `unpick` | `order_id`, `line_id`, `location`, `lot`, `quantity`, `reason` | operator/admin |
| `reset_allocation` | `order_id`, `reason` | operator/admin |
| `pack` | `order_id` | operator/admin |
| `ship` | `order_id`, `carrier`, `handoff_reference` | operator/admin |
| `cancel_order` | `order_id`, `reason` | operator/admin; OMS only for source-linked work |
| `report_exception` | `order_id` (nullable), `category`, `detail` | operator/admin |
| `resolve_exception` | `exception_id`, `resolution` | operator/admin |

Unknown fields are rejected. Nonempty text is at most 240 UTF-8 bytes, without surrounding whitespace/control characters. Receipt/order line arrays contain 1–100 lines with unique immutable IDs. All ordinary quantities are whole each units between 1 and 1,000,000. SKU/lot identity is case sensitive. Use an explicit `UNTRACKED` lot when no lot control applies. No fractional rounding or implicit case conversion occurs.

## Inventory and lifecycle semantics

`on_hand` is physical warehouse custody, including receiving and outbound staging. `reserved` is the subset held for outstanding order lines. A bin/lot must satisfy `0 <= reserved <= on_hand`. Only storage locations can supply new allocations. Picking transfers on-hand/reserved units from storage to `STAGE-01`; handoff removes the staged stock and consumes its reservation.

Orders transition `pending → allocated → picking → picked → packed → shipped`; a one-step complete pick can go directly from `allocated` to `picked`. `pending`/`allocated` may transition to `cancelled`, releasing only that order's reservations. Partial receipts transition `expected → partial → received`. Partial shipments are not supported: packing requires all lines picked. Blocking exceptions prevent allocation/pick/pack/ship; resolving one records evidence and does not manufacture stock.

Recovery is available during a blocking exception. `unpick` returns only the specified allocation's physically staged units to its original storage bin and lot, preserving that line's reservation. It accepts picking, picked and packed work; returning packed units invalidates packing. `reset_allocation` releases outstanding reservations only after every staged unit has been returned, then returns the order to `pending`. Recount or replenish the actual stock, resolve the exception and allocate again. Shipped work cannot be unpicked or reset.

## Native OMS and TMS operations

These routes use [the actual native contracts](native-integrations.md), configured per authenticated warehouse tenant with `WMS_NATIVE_INTEGRATIONS_JSON`. Generic warehouse event webhooks below are independent.

| Method | Route | Result / permission |
|---|---|---|
| GET | `/v1/integrations/oms/orders?page=1` | Canonical OMS page for the configured source tenant; operator/admin |
| POST | `/v1/warehouses/{id}/imports/oms` | Freeze a reviewed source revision and execute its warehouse import; operator/admin/oms |
| GET | `/v1/warehouses/{id}/orders/{order_id}/readiness` | Packed readiness plus explicit OMS writeback hold; operator/admin |
| POST | `/v1/warehouses/{id}/orders/{order_id}/transport` | Freeze and execute a source-verified planned-load import; operator/admin |
| GET | `/v1/warehouses/{id}/native-requests/{request_id}` | Original actor's native receipt, source/frozen evidence and attempts; operator/admin/oms |
| GET | `/v1/warehouses/{id}/orders/{order_id}/oms-fulfillment` | Legacy held fulfillment proposal after committed handoff; operator/admin |
| POST | `/v1/warehouses/{id}/orders/{order_id}/oms-fulfillment` | Opt-in conditional OMS fulfillment from immutable handoff evidence; operator/admin |

An import body is `{request_id, expected_version, oms_order_id, source_updated_at, details:{customer,destination,priority}}`. The OMS UUID and exact source timestamp must identify the reviewed revision. A conditional OMS fulfillment body is `{request_id, expected_version}`. A transport body is `{request_id, expected_version, plan}`; the [native contract](native-integrations.md) specifies the complete plan with real facilities, dates, customer identity, units, mileage and prices. A warehouse readiness export does not supply those missing transport facts.

Persist each full original request before sending it. Fingerprints bind operation, warehouse, path order and complete input; receipt ownership separately binds the original actor. Native receipts return `{found,request_id,kind,status,order_id,attempts,replayed,response,last_error,in_progress,source_snapshot,evidence}`; receipt lookup also includes `attempt_history`. Reusing a key with another actor or input returns 409. A native receipt is visible only to its original actor. Native HTTP is disabled in demo mode; viewers cannot trigger external work or read native request evidence. Upstream HTTP/contract failures can return 502.

The native journal retains the original upstream response, source revision and translated command or exact transport payload before execution. States are `frozen`, `sending`, `uncertain`, `completed`, import-only `local_conflict`, or conditional-OMS-only `rejected`. A first transport send rechecks packed state, exceptions and source lines. A 30-second lease permits one sender, and no database lock spans external HTTP. Timeout or crash means acceptance may be uncertain: recover or retry the same original intent. Its frozen payload and remote idempotency key survive source changes, and another intent for the same source order is rejected.

OMS writeback defaults to held. Enabling `shipment_writeback: "conditional_v1"` requires the separately deployed [companion receiver](../integrations/stateset-sync-server/README.md). The WMS verifies its tenant/revision/terminal/idempotency/rejection capabilities, canonical source lines and immutable physical handoff before retaining a new intent. Initial GET-only preparation failure returns 424 and makes no OMS POST. After claim, remote errors remain `uncertain`; retry the frozen revision, recipient, body and key without replacing the intent. The legacy fulfillment endpoint is never used.

Only an acknowledgment matching the complete request identity and SHA256 digest can complete or reject a retained handoff. A durable `rejected` receipt is terminal for that request and permits a separately reviewed new intent for the same shipment event; an uncertain result does not. Rejected reasons are `source_revision_changed`, `source_state_not_eligible`, `source_lines_changed`, `order_already_fulfilled`. Exact completed/rejected request replays return the original receipt. The server does not automatically refresh a stale revision. Freight acceptance creates a planned load; it does not establish carrier pickup, dispatch or delivery.

The dashboard journals the original request before sending. Normal commands and scanner confirmations use synchronous submission guards; shared pending state blocks native operations and physical saves while a recovery journal is unresolved. A denied recovery retry retains it. Scanner validation captures the warehouse/version and exact receipt or allocation line before bin, SKU and lot confirmation; refresh or a warehouse change requires another scan. Scanning Enter advances validation and never commits stock without explicit quantity confirmation.

## Warehouse event receiver contract

This optional WMS producer contract requires compatible consumers; an existing Sync or Freight endpoint is not assumed to consume it. Native adapters above use their separate verified source contracts. The sibling services are unchanged by this repository. EDI continues to own X12 translation and transport.

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
  "schema_version": 2,
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
  "inventory_changes": [{"stock":{"sku":"TEE-001","lot":"LOT-1","location":"A-01","on_hand":12,"reserved":3},"version":17}],
  "command": "original typed command"
}
```

The descriptions for `order` and `command` above stand in for their actual JSON objects. `inventory_changes` contains absolute snapshots of only the bin/lot balances changed by this command; an empty array means no balance changed. Download committed event JSON from Integrations for an exact fixture. Existing version-1 events retain their original full `inventory` projection; consumers must branch on `schema_version` when replaying them.

Every committed command queues an OMS event. Packing and shipping also queue TMS readiness/handoff events. Delivery is at least once and may arrive out of order after retries. Consumers persist/deduplicate event IDs and version each stock identity `(tenant,warehouse,sku,lot,location)` independently. A newer event for another bin does not replace a missing change to this bin; do not drop every older event using one warehouse-wide version. Absence from `inventory_changes` means unchanged, never deleted. Zero balances are explicit snapshots. Missing events require reconciliation with authoritative warehouse data.

Per-order business effects also require retained event identity: a previously unseen shipment may still matter even when an event's warehouse version is older than another received event. A TMS readiness event never authorizes another warehouse stock decrement.

The HTTP timeout is 10 seconds, lease 30 seconds, with eight attempts and exponential backoff before dead letter. The same event ID survives retries/lease recovery/requeue. HTTP 2xx means the receiver accepted the envelope; it does not mean downstream order or transport execution completed. Delivery attempts and administrative requeues are retained independently of current queue status.
