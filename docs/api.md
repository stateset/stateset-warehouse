# API and integration contract

All `/v1` routes require `Authorization: Bearer <credential>`. The server resolves tenant, actor and role from API-key configuration or verified WorkOS AuthKit membership with explicit server mappings; see [authentication](authentication.md). Passing tenant/actor headers or command fields does not change identity. Every API response, including authentication errors and unknown routes, uses `Cache-Control: no-store`. Request bodies are limited to 256 KiB.

Responses carry a server-generated UUID in `X-Request-Id`; a caller cannot override it. Request tracing records this ID, method and route template without credentials or query strings. Responses also set `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer` and a same-origin Content Security Policy that disallows framing, objects and external scripts. Connections are same-origin plus the single configured WorkOS Authentication API origin when enabled. TLS remains the ingress's responsibility.

## Routes

The same authenticated service exposes a [native MCP endpoint](mcp.md) at `/mcp`. Its tools dispatch through these canonical handlers and retain the original tenant, actor, request ID, reviewed version and response. MCP clients must send their bearer credential on every request.

| Method | Route | Result |
|---|---|---|
| GET | `/auth/config` | Public nonsecret login provider configuration; no-store, no tenant/role maps |
| GET | `/health` | Process liveness; public |
| GET | `/ready` | 200 if a database query succeeds within two seconds, otherwise 503; public, no-store |
| GET | `/v1/session` | Verified identity, explicit demo mode and configured receiver status |
| GET | `/v1/operations/health?limit=50&after={warehouse_id}` | Consistent tenant-wide queue/native/exception summary and warehouse page; operator/admin |
| GET | `/v1/operations/metrics` | Fixed-cardinality Prometheus snapshot; admin only, no query parameters |
| GET | `/v1/lot-trace?sku={exact_sku}&lot={exact_lot}` | Tenant-wide exact-label trace with section pagination and version-fenced source review; read roles |
| GET | `/v1/serials?sku={exact_sku}` | Bounded current literal custody; global SKU review and selected source flags |
| GET | `/v1/serial-history?sku={exact_sku}&serial={literal}` | Original per-unit before/after lineage; global review |
| GET | `/v1/serial-observations?sku={exact_sku}` | Original label occurrences and explicit empty observation groups |
| POST | `/v1/warehouses/{id}/serial-observation-plan` | Read-only original command target resolution for literal-label collection; retains original write fences |
| GET | `/v1/warehouses?limit=100&after={warehouse_id}` | Tenant warehouse metadata/version page and `next_after`; limits 1–100 |
| GET | `/v1/warehouses/{warehouse}/transfers/{transfer_id}/losses` | Original loss/reversal journal page, with destination review and source flags; authenticated read |
| GET | `/v1/warehouses/{id}/unit-packs?sku={exact_sku}` | Current definitions, including disabled codes; `history=true` pages immutable changes |
| GET | `/v1/warehouses/{id}/unit-observations?sku={exact_sku}` | Original pack counts, exact each conversions, definition IDs and source flags |
| GET | `/v1/warehouses/{id}` | Consistent operational snapshot: active work, bounded completed work, recent history and an operational balance check |
| GET | `/v1/warehouses/{id}/reconciliation` | Full append-only movement, allocation and exact receipt-line proof, checked version and overall validity |
| GET | `/v1/warehouses/{id}/history/{kind}?limit=50&before={cursor}` | Keyset history for `orders`, `receipts`, `movements`, `activity` or `exceptions` |
| GET | `/v1/warehouses/{id}/orders/{order_id}` | Authoritative order detail, including archived work |
| GET | `/v1/warehouses/{id}/receipts/{receipt_id}` | Authoritative receipt detail, including completed receipts |
| GET | `/v1/warehouses/{id}/orders/{order_id}/return-eligibility` | Consistent shipped/authorized/remaining quantities for each exact order line and lot, plus reviewed warehouse version |
| POST | `/v1/warehouses/{id}/replenishment/plan` | Read-only bounded pick-face plan; operator/admin |
| GET / PUT | `/v1/warehouses/{id}/replenishment/rules` | Saved replenishment rules; read roles / admin (audited) |
| GET | `/v1/warehouses/{id}/replenishment/due` | Replenishment due under saved rules; operator/admin |
| POST | `/v1/warehouses/{id}/putaway/plan` | Read-only putaway suggestions for receiving stock; operator/admin |
| POST | `/v1/warehouses/{id}/picking/batch-plan` | Read-only multi-order pick walk with totes; operator/admin |
| POST | `/v1/warehouses/{id}/counts/plan` | Read-only ABC cycle counts due; operator/admin |
| POST | `/v1/warehouses/{id}/labels` | ZPL labels for bins, products, packages, serials and carrier labels; read roles |
| GET / POST | `/v1/warehouses/{id}/packages/{package}/carrier-labels` | List / record a package's carrier label; read roles / operator |
| POST | `/v1/warehouses/{id}/packages/{package}/carrier-labels/{label}/void` | Void the active carrier label with a reason; operator |
| POST | `/v1/warehouses/{id}/commands` | Atomic warehouse transition |
| GET | `/v1/warehouses/{id}/commands/{request_id}` | Original command receipt for this actor; `{found:false}` if not committed |
| GET | `/v1/warehouses/{id}/outbox` | Latest 200 tenant-scoped destination records |
| GET | `/v1/events/{event_id}` | Committed original event envelope for inspection/consumer recovery |
| POST | `/v1/outbox/{outbox_id}/retry` | Admin-only dead-letter requeue, with retained attempts and audit entry |

Unknown/cross-tenant entities return 404. Unauthorized requests return 401; forbidden roles 403; stale versions/reused keys 409; invalid transitions 422; database availability failures 503. Axum rejects malformed JSON/extraction errors with its standard non-JSON error body; clients must handle unreadable error bodies safely.

Unknown `/v1`, `/v1/` and nested API paths return authenticated JSON 404 responses. They cannot fall through to the dashboard HTML. The [operations contract](observability.md) specifies queue ages, configured-receiver semantics, tenant-local metrics and snapshot deadlines. Operations reads do not alter or retry warehouse work.

History pages return `{items:[{cursor,data}],next_cursor}` ordered by descending sequence. `limit` is 1–100, default 50. `before` is an exclusive positive cursor; use `next_cursor` unchanged for the next page. A null cursor means the end of that traversal. New records can appear above an existing cursor; refresh the first page to see them. History is retained independently of the bounded operational snapshot.

`GET /v1/session` also supplies `default_warehouse_id`: the bootstrapped `wh-main` facility in real mode or `wh-pdx` in demo mode. It is independent of directory sort order and may be outside the first page. Browser selection gives retained physical/native work priority, then a remembered facility, then this server default.

The warehouse directory returns `{warehouses:[{warehouse,version}],next_after}` in ascending immutable warehouse-ID order. `limit` defaults to 100, with a range of 1–100. Pass the returned `next_after` unchanged and URL-encoded as `after`; null ends the traversal. Directory reads never create execution evidence. Every page is tenant-scoped; unknown/duplicate parameters, invalid limits and control-character cursors return 422. Pages are separate reads, so refresh the first page to see facilities inserted before a previously used cursor. Warehouse IDs identify the scope; code, name, city and timezone are editable labels.

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

### Create a facility

An admin creates an empty facility with a normal command at its new immutable warehouse URI. New IDs contain 1–80 ASCII letters, digits, hyphens or underscores and start with a letter or digit. The dashboard generates the ID once before submission. For example, POST `/v1/warehouses/wh-east/commands`:

```json
{
  "request_id": "create-east-original-2026",
  "expected_version": 0,
  "command": {
    "type": "create_warehouse",
    "code": "EAST",
    "name": "East fulfillment",
    "city": "Toronto",
    "timezone": "America/Toronto"
  }
}
```

Creation requires version zero and omits `resource_versions`. The new header, `RECV-01` receiving bin, `STAGE-01` staging bin, version-1 `warehouse.created` activity/event, actor/payload-bound receipt and OMS outbox intent commit together. Products, stock, receipts and orders start empty. Invalid metadata or a final delivery-intent failure leaves no partial facility. Racing exact envelopes produce one creation and original-result replays; another request cannot recreate an existing facility, including a preprovisioned version-zero warehouse.

Preserve **both the warehouse URI and complete envelope** across uncertain responses. Receipt lookup can return `found:false` before a new facility exists; resubmitting the exact original URI/envelope remains safe. A committed creation replays its original version-1 result after later facility updates. Operators, viewers and integration roles cannot create facilities; authenticated roles retain their existing tenant-wide read/work permissions. Ordinary stock movements remain within one warehouse scope; use the separate physical transfer workflow below for inter-facility custody.

### Review and recovery

For a known version conflict, refresh and review the work, then submit a **new** request ID with the reviewed version. A receipt lookup returning `found:false` does not prove an earlier network request can never arrive: retry the original key. Do not repeat a physical action merely because its response was lost.

A strictly stale original command can return HTTP 409 with this additional, definitive outcome:

```json
{
  "error": "Warehouse changed; review current work",
  "command_outcome": {
    "status": "not_committed",
    "reason": "stale_version",
    "tenant": "your-org",
    "actor": "operator:alice",
    "warehouse_id": "wh-main",
    "request_id": "c62d2a33-b2ce-46a8-9f4e-b0d471b9cc7c",
    "expected_version": 17,
    "current_version": 18
  }
}
```

The server emits this outcome only after taking the warehouse write lock, finding no saved receipt for that request, and proving the expected version is strictly older. An already committed matching request replays its original result first. Future versions, actor/payload conflicts, authorization errors and ordinary failures do not carry this proof. A delayed copy of the original stale envelope cannot commit while warehouse versions advance normally.

Recovery clients may retire their pending journal only when the 409 outcome matches its tenant, actor, warehouse, request ID and expected version, with a safe integer current version strictly greater than expected. Then refresh and review current work and physical stock before creating another request; no physical action is repeated automatically. A generic 409 or `found:false` alone remains ambiguous. The dashboard closes stale detail and version-binds canonical order/receipt reads so an older detail response cannot authorize a form at a newer warehouse version.


Physical commands can additionally include `resource_versions`, an object copied from the resources reviewed in the operational snapshot. Keys are compact JSON arrays such as `["stock","TEE-001","LOT-1","A-01"]`, `["product","TEE-001"]`, `["location","A-01"]`, `["order","order-id"]`, `["receipt","receipt-id"]` and `["inventory","TEE-001"]`. A missing resource begins at revision zero. Counters are database-owned and monotonic, including metadata creation and balances changed and later restored. They are independent of warehouse versions.

| Scoped command | Required resources |
|---|---|
| `move_stock` | Source/destination stock, SKU product and both locations |
| `count_stock` | Counted stock, SKU product and location |
| `receive` | Exact receipt, destination stock, SKU product and receiving location |
| `allocate` | Order and one SKU-wide inventory counter for each ordered SKU; new bin/lot buckets also advance it |
| `pack` | Order; opening/resolving a blocking exception advances the same counter |
| `pick`, `unpick` | Order, exact allocation's source/staging stock, SKU product and locations |
| `ship`, `cancel_order`, `reset_allocation` | Order, all allocation source/staging stocks, products and locations |

The server requires exactly the command's resource set, at most 2,000 nonnegative revisions. It still serializes the physical commit under the warehouse lock and checks database inventory proofs. Unrelated warehouse changes may proceed without replacing the reviewed request or rescanning unaffected work. Catalog/receipt/order creation, return authorization and exception administration retain the warehouse-wide fence. Clients omitting the map retain the original strict version contract. Large resource sets can use that strict contract. The dashboard falls back when the map exceeds 2,000 keys or 128 KiB of UTF-8 JSON, leaving room within the 256 KiB command body limit for the request envelope and command. This preserves strict stale-review rejection for large multi-lot work.

A scoped conflict can return `command_outcome` with `reason: "changed_resources"`, the same eight identity/version fields above, and `expected_resource_versions`/`current_resource_versions`. Recovering clients verify the full original identity and expected map, exactly matching key sets, safe integer counters, every current revision at least expected and at least one strictly greater. The global current version must be at least expected. A legacy `stale_version` proof cannot retire a scoped journal: that original may still safely commit across unrelated global changes. Matching committed requests replay first; future resource versions and ambiguous outcomes remain held. Counters and receipts must remain intact during normal operation; restore requires the separate post-backup physical/remote review in the operating runbook.

Committed command receipts and administrator audit records are append-only. Exact actor/payload replay reads an immutable receipt without waiting for the physical writer lock. When no receipt exists, the write path checks again under that lock before acting, preserving duplicate-request serialization.

| Command type | Required command fields | Permission |
|---|---|---|
| `create_warehouse` | New warehouse URI, `code`, `name`, `city`, `timezone` (IANA); envelope version 0, no resource map | admin |
| `update_warehouse` | `code`, `name`, `city`, `timezone` (IANA) | admin |
| `create_product` | `sku`, `name`, `category`, `reorder_point` (0–1,000,000) | admin |
| `create_location` | `code`, `zone`, `kind` (`storage` or `receiving`) | admin |
| `create_receipt` | `reference`, `supplier`, `lines:[{id,sku,lot,quantity}]` | operator/admin |
| `create_return` | `reference`, `order_id`, `reason`, `lines:[{id,order_line_id,lot,quantity}]` | operator/admin |
| `cancel_receipt` | `receipt_id`, `reason` | operator/admin |
| `receive` | `receipt_id`, `line_id`, `quantity`, `location` (receiving bin) | operator/admin |
| `move_stock` | `sku`, `lot`, `from`, `to` (storage bin), `quantity` | operator/admin |
| `count_stock` | `sku`, `lot`, `location`, `counted` (0–1,000,000), `reason` | admin |
| `create_order` | `reference`, `source_order_id` (nullable), `customer`, `destination`, `priority` (`normal`/`high`/`urgent`), `lines:[{id,sku,quantity}]` | operator/admin; OMS requires source ID |
| `allocate` | `order_id` | operator/admin |
| `allocate_available` | `order_id` | operator/admin |
| `release_unpicked` | `order_id`, `reason` | operator/admin |
| `pack_shipment` | `order_id`, `reference`, `lines:[{line_id,location,lot,quantity}]` | operator/admin |
| `unpack_shipment` | `shipment_id`, `reason` | operator/admin |
| `handoff_shipment` | `shipment_id`, `carrier`, `handoff_reference` | operator/admin |
| `pick` | `order_id`, `line_id`, `location`, `lot`, `quantity` | operator/admin |
| `unpick` | `order_id`, `line_id`, `location`, `lot`, `quantity`, `reason` | operator/admin |
| `reset_allocation` | `order_id`, `reason` | operator/admin |
| `pack` | `order_id` | operator/admin |
| `ship` | `order_id`, `carrier`, `handoff_reference` | operator/admin |
| `cancel_order` | `order_id`, `reason` | operator/admin; OMS only for source-linked work |
| `cancel_remainder` | `order_id`, `reason` | operator/admin; OMS only for source-linked partly shipped work; complete warehouse-version review |
| `report_exception` | `order_id` (nullable), `category`, `detail` | operator/admin |
| `resolve_exception` | `exception_id`, `resolution` | operator/admin |

Unknown fields are rejected. Nonempty text is at most 240 UTF-8 bytes, without surrounding whitespace/control characters. Receipt/order line arrays contain 1–100 lines with unique immutable IDs. All ordinary quantities are whole each units between 1 and 1,000,000. SKU/lot identity is case sensitive. Use an explicit `UNTRACKED` lot when no lot control applies. No fractional rounding or implicit case conversion occurs.

## Inventory and lifecycle semantics

Customer returns require a physically shipped original order. Each new receipt line is bound to an exact original order line and lot; SKU is derived from that source. All retained authorizations, including completed and archived RMAs, consume that source's allowance less their immutable cancelled remainders. Neither duplicate SKU lines nor a different lot can borrow it. Authorizing a return emits `return.authorized` and creates no physical movement. Normal `receive` records exact receipt-line movement into receiving; approved putaway makes it allocatable. Original shipped allocations and OMS fulfillment evidence remain unchanged. Source ownership is normalized and immutable independently of cached receipt JSON. Full reconciliation includes `returns` rows comparing original shipped, authorized and received quantities. Refunds and automated quality disposition are outside this contract.

Completed handoffs have immutable `wms_shipment_evidence`, captured atomically from the original scoped `fulfillment.shipped` event after canonical order/line/allocation and outbound movement changes. Original order-line identities/expectations cannot be rewritten; once shipped, business header/source/handoff fields and exact allocation ownership cannot change, and no extra shipment movements may be appended. Canonical cached JSON child arrays remain non-authoritative. SQL checks compare the complete typed order and actual `STAGE-01` shipment movements, including per-SKU/lot totals. A failure rolls the source record, stock, event, command receipt and delivery intents back together.

Return eligibility adds `shipment_source:{kind,event_id,version}` (package handoffs additionally retain `event_ids`, described below). Caps come from the immutable original allocation facts, including summed split-bin allocations; mutable operational rows cannot create new allowance. `kind:"event"` identifies the original committed handoff. `legacy_snapshot` binds the exact retained, write-fenced pre-relational snapshot; `synthetic_fixture` identifies a disclosed demo baseline with its original immutable fixture activity. Baselines retain a null event ID and never fabricate a physical event. The dashboard labels them explicitly. Conditional OMS writeback still requires an actual `event` source and its original event ID.

Full reconciliation adds `shipments` rows with order ID, source kind/event/version and consistency. It compares all retained source records, canonical normalized facts and the full outbound movement history, outside the operational display window. Missing, ambiguous or differing historical source evidence blocks migration; it is not replaced with a current-row guess. A pre-relational source is checked again during the actual Store import. Conflicting provenance holds RMA authorization and first OMS send; the original native journal stays intact without a remote attempt. The ledger calls out shipment-source differences separately from balance differences and preserves them in the exported proof.

`cancel_receipt` closes all remaining unreceived units of an expected or partially received inbound/RMA receipt. It requires the strict reviewed warehouse `expected_version` and omits `resource_versions`. It never changes expected quantities, received quantities, stock or movements. A fully received or already cancelled receipt cannot be cancelled again. The terminal `cancelled` receipt retains `cancellation:{reason,actor,cancelled_at,lines:{[line_id]:{sku,lot,quantity,received,cancelled}}}`. Receipt-line identities, SKU/lot and expected quantities are immutable from creation, including before cancellation. Every cancellation line freezes received and unreceived quantities too, including fully received lines with zero cancelled units. No further receiving or reopening is allowed; an additional delivery requires a separately reviewed new receipt/reference.

For an RMA, only cancelled, unreceived units release that exact original order-line/lot allowance. For example, authorizing three and receiving one consumes one after cancelling the remaining two; another two can then be authorized. A fully received same-SKU line keeps its own allowance consumed. This holds after the original RMA leaves the operational history window and under concurrent repeatable-read SQL. Cancellation headers and line snapshots are normalized, immutable and reconstructed independently of cached JSON. Full reconciliation adds `receipt_cancellations` rows for all receipt terminal states and frozen quantities. The atomic `receipt.cancelled` event includes the canonical `receipt` and reason; `inventory_changes` is empty. Exact retries replay the original actor/payload-bound result after subsequent work or archive restore.

Replenishment planning accepts `{rules:[{sku,destination,minimum,target,source_zone:null}]}` with 1–100 unique SKU/destination rules and `1 <= minimum <= target <= 1,000,000`. Destinations are storage bins. A named source zone must exist. Planning uses only unreserved storage, excludes every configured pick-face destination from sources and shares virtual source capacity across all rules; receiving/staging cannot fund it. It returns `{warehouse_id,version,generated_at,tasks,shortages}` with at most 500 suggested moves. Each task contains a normal `move_stock` command and exact resource revisions. `shortages` explicitly reports unavailable quantities. Planning reserves/moves nothing. After physically confirming one move, use the original request journal and replan before another. Rules are supplied per review, not persisted or executed by a background scheduler.

`on_hand` is physical warehouse custody, including receiving and outbound staging. `reserved` is the subset held for outstanding order lines. A bin/lot must satisfy `0 <= reserved <= on_hand`. Only storage locations can supply new allocations. Picking transfers on-hand/reserved units from storage to `STAGE-01`; handoff removes the staged stock and consumes its reservation.

Orders transition `pending → allocated → picking → picked → packed → shipped`; a one-step complete pick can go directly from `allocated` to `picked`. `pending`/`allocated` may transition to `cancelled`, releasing only that order's reservations. Partial receipts transition `expected → partial → received`. The original `pack`/`ship` path requires all lines picked. Package mode supports available-stock allocation and independent partial handoffs as described below. Blocking exceptions prevent allocation/pick/pack/ship; resolving one records evidence and does not manufacture stock.

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

Before its first send, each event/destination freezes a SHA256 fingerprint of the exact configured URL in the same transaction as its delivery lease. Retries, expired leases and administrator requeues must use that original URL. A changed URL moves the item to a dead letter without sending or incrementing the attempt counter. Restoring the original URL and explicitly requeuing permits recovery; its credential may rotate. The database rejects changes to the retained event, tenant, destination or receiver fingerprint. Each attempt increment requires a matching transaction-local receiver assertion; older workers that do not supply it are rejected before their claim commits or HTTP send begins. A URL fingerprint proves endpoint continuity; this generic consumer contract has no remote tenant-identity endpoint, so credential-to-tenant mapping still requires deployment verification.

Migration 008 marks previously attempted deliveries without a retained receiver identity as `unverified`, including rows whose counter was reset but whose attempt history remains. Completed deliveries remain completed. Pending or expired historical deliveries are held for independent receiver reconciliation; ordinary requeue cannot infer or replace their original endpoint. Never-attempted events can bind their first configured receiver. No URL or credential is stored in the fingerprint field.

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

The HTTP timeout is 10 seconds and lease 30 seconds. Automatic delivery permits at most eight claimed attempts per administrator-authorized cycle, including attempts whose sender crashed before committing an outcome. After an eighth lease expires, the worker retains uncertainty and dead-letters without another HTTP request. Exponential backoff applies between retryable attempts. Late responses are fenced by the retired lease token and cannot overwrite a dead letter or retained uncertain attempt. Audited administrator requeue starts another explicit cycle while preserving the event ID and full history. The same event ID survives retries/lease recovery/requeue. HTTP 2xx means the receiver accepted the envelope; it does not mean downstream order or transport execution completed. Delivery attempts and administrative requeues are retained independently of current queue status.


## Physical inter-facility transfers

Operators/admins use the ordinary `POST /v1/warehouses/{warehouse}/commands` journal. Both actions require the strict reviewed warehouse version and no resource map. Dispatch at the source records the actual handoff and removes only unreserved storage stock:

```json
{"request_id":"original-transfer-dispatch","expected_version":42,"command":{"type":"dispatch_transfer","reference":"TRANSFER-104","destination_warehouse_id":"wh-east","carrier":"Reviewed carrier","handoff_reference":"CARRIER-104","lines":[{"id":"line-a","sku":"SKU-A","lot":"LOT-A","location":"A-01","quantity":3}]}}
```

The destination must exist in the same tenant with every SKU explicitly configured. Admission locks both facility heads in sorted ID order. Dispatch changes only source inventory/version; the destination receives no available stock or commercial order. The immutable transfer ID is `result.entity_id`. Original line IDs, lots, quantities, source bins, carrier, handoff, actor, time and source event/version are retained. References are unique per source facility, including completed transfers.

List the original work with `GET /v1/warehouses/{warehouse}/transfers?after=0&limit=50&open_only=true`. The response contains `warehouse_id`, a consistent reviewed `version`, `items:[{cursor,data}]`, and `next_cursor`. Cursors advance in ascending creation order; limit is 1–100. `open_only=false` includes completed history. Each document includes both facility IDs, provenance, `ok`, and exact lines with `quantity` dispatched and `received`; `quantity - received - lost` remains in transit. `lost` is net original loss minus capped reversals; `loss_record_count` counts retained loss and reversal records. Original records are paginated separately. Source and destination can inspect the same original work. A transfer with inconsistent evidence cannot receive more stock. When warehouse version changes between pages, refresh the complete list before using it as a receiving review.

Receive at the original destination, confirming actual each quantity, original line/lot, receiving bin and arrival reference:

```json
{"request_id":"original-transfer-arrival","expected_version":12,"command":{"type":"receive_transfer","transfer_id":"ORIGINAL-TRANSFER-ID","line_id":"line-a","quantity":2,"location":"RECV-01","arrival_reference":"Observed two each on arrival"}}
```

Partial arrivals keep the exact remainder in transit. Each line is capped independently, including same-SKU/same-lot lines from different bins. Receiving increases only destination receiving stock; ordinary putaway is required before allocation. No inbound receipt or OMS fulfillment order is fabricated. A transfer cannot use ordinary `receive` or customer-return authorization to consume its remainder. Missing/changed evidence, wrong destination, over-arrivals and stale versions are rejected. Dispatched ownership cannot be edited or silently cancelled; shortage, loss or damage needs an exception and a separately reviewed disposition procedure. Confirmed losses use the administrator journal below. Unconfirmed shortages remain in transit; rerouting remains unsupported.

Stock, immutable movements/ownership, activity, original command receipt, self-contained `transfer.dispatched`/`transfer.received` event and outbox intent commit atomically. Exact retries use the original actor, warehouse, key and full payload even after later work. Generic inventory events describe these physical transitions; they do not perform native OMS fulfillment or create a Freight planned load. Independent reconciliation exports `transfers` with original source identity and per-line dispatched/received/in-transit quantities, and includes their consistency in its overall `ok`.


## Partial shipments and backorders

Use `GET /v1/warehouses/{warehouse}/orders/{order}/shipments` for a consistent review: `{warehouse_id,order_id,version,ok,order,packages,progress}`. Each progress line identifies its original line/SKU and ordered, allocated, picked, shipped and unallocated quantities. Packages include original packing actor/time/event/version, exact allocation lines and a null or immutable `closure` (`unpacked` or `handed_off`) with its original event/version and evidence. A missing tenant/warehouse/order returns 404. Any failed ownership proof holds changes.

All five new commands in the table above require this reviewed warehouse `expected_version` and omit `resource_versions`. Preserve the exact URI, actor, key and complete envelope across uncertain saves. Initial `allocate_available` or `pack_shipment` opts the order into `shipment_mode:"packages"`; activation freezes original business header and line quantities. The mode cannot be removed. Historical whole-order handoffs cannot be reinterpreted as packages.

`allocate_available` reserves whatever storage stock is actually available, independently per original line, including same-SKU lines. Missing quantities remain unallocated/backordered; subsequent allocation reserves only remaining demand and preserves picked and shipped ownership. It never invents stock. Existing `pick`/`unpick` work with package mode, including `partially_shipped`. Active packed claims must first be unpacked before their units can be unpicked. Packing and closure inserts advance the owning order resource revision even without stock/order-row changes; an older scoped pick/unpick review requires a fresh review after those changes. `release_unpicked` requires a reason, releases only unpicked holds and retains staged/shipped ownership. Blocking exceptions hold allocation, picking, packing and handoff while allowing unpack, unpick and release for physical recovery.

`pack_shipment` freezes 1–100 unique original `(line_id,location,lot)` tuples with whole-each quantities, each bounded by picked minus shipped minus other active package claims. A reference is unique per warehouse, including unpacked/completed packages. The package ID is `result.entity_id`. `unpack_shipment` records a terminal reason and releases the claim without changing physical stock. `handoff_shipment` consumes exactly that immutable package, records actual carrier/reference, creates exact outbound movements and leaves every other staged/storage/backorder unit untouched. No second terminal closure is possible. The ordinary command receipt replays the original result after later work.

Status becomes `partially_shipped` after any physical handoff and `shipped` only when every original ordered unit is handed off. A single complete package retains actual aggregate carrier/reference; multiple handoffs leave those aggregate fields empty and retain all actual carrier evidence in their packages. Source events are `fulfillment.available_allocated`, `fulfillment.unpicked_released`, `shipment.packed`, `shipment.unpacked` and `shipment.handed_off`; package events include canonical parent order and self-contained package evidence. No fake aggregate handoff/event or commercial order is created.

Return eligibility works before remaining units ship. Its `shipment_source` has `kind:"handoff_events"`, `event_id:null` and nonempty unique `event_ids` for actual handoffs; caps use only their immutable original line/lot quantities. One complete package instead retains `kind:"event"` and its actual closure event ID. Full reconciliation exports independent `packages` ownership/source/closure proofs and includes their consistency in overall `ok`.

The current native OMS `conditional_v1` contract accepts a single complete physical shipment event only. Partial and multiple handoffs return 409 before remote HTTP or a new native intent; generic durable integration events still describe the actual package transitions. Existing native recovery receipts remain available. Package labels/rate purchasing and fractional units are not implemented. Serial command admission is described below and still requires operator and release qualification.


### Cancel all unshipped demand after partial handoff

Read `GET /v1/warehouses/{id}/orders/{order_id}/shipments`, review all original ordered/shipped/unallocated quantities and retain its `version`. Submit the ordinary command envelope with that `expected_version`, a fresh original `request_id`, no `resource_versions`, and `command:{"type":"cancel_remainder","order_id":"original-order-id","reason":"Customer cancelled all remaining demand"}`. Keep the complete original envelope through any uncertain response. MCP exposes the identical operation as `wms_cancel_remainder`.

The order must be in package mode and `partially_shipped`, with at least one actual handoff and unshipped demand. Active packages must first be unpacked. Every picked-but-unshipped unit must then be physically returned with an explicit `unpick`; unpacking alone does not move stock. The cancellation releases only this order’s remaining unpicked reservations and closes **all** unshipped demand, including unallocated backorders. It records no movement or new shipment and changes no original ordered quantity or actual handoff. Blocking warehouse exceptions do not prohibit this recovery/closure, and remain open until independently resolved.

The terminal order has `status:"closed_partial"` and `remainder_cancellation:{reason,actor,cancelled_at,lines:{original_line_id:{sku,ordered,shipped,cancelled}}}`. `ordered = shipped + cancelled` for each original line; fully shipped lines retain zero cancelled units. Both `GET .../orders/{order_id}` and retained history include this evidence. The shipment review adds `cancelled` to every progress row; `unallocated` excludes cancelled demand. Existing fully cancelled orders remain `cancelled` and show their whole original demand as cancelled. Closed partial work leaves active-order counts, but its original details and command journal remain available.

Source-linked OMS service identities may close eligible remainders; they cannot close manually sourced work. Viewers cannot close orders. Exact committed retries retain the original result/version after later work. A stale review, competing closure, new packing/picking or wrong actor cannot reuse released ownership. Cancellation metadata/lines are immutable and commit atomically with original actor-bound command, `fulfillment.remainder_cancelled` event and OMS outbox intent. The event includes the exact closed order and does not imply upstream acceptance.

Physical customer returns continue to use only actual original shipped line/lot units, including after closure. Cancelled demand creates no return allowance. This command does not refund money, cancel an upstream OMS order, purchase/revoke transport or make the original partial handoff eligible for whole-order native fulfillment. The current native receiver remains held before external HTTP or intent creation.


## Lot expiry, quarantine and availability

Controls are scoped to `(tenant,warehouse,sku,lot)` and separate availability from custody. A product's `expiry_required` defaults to false for existing work. Required stock without a known expiry is unavailable; a known expiry is always enforced even for optional products. At `now >= expires_at` a lot is expired. Allocation chooses earliest eligible expiry, then stable bin/lot order; optional unlabelled lots follow known dates. A hold blocks new work independently of expiry. Existing stock, reservations, staged claims and earlier actual handoffs remain retained.

`GET /v1/warehouses/{warehouse}/lots` returns `{warehouse_id,version,checked_at,items,next_cursor}` from one tenant-scoped repeatable read. Optional exact `sku`, `limit` from 1–100 (default 50) and `after` page the ordered SKU/lot keys. Pass the original JSON-string `next_cursor` unchanged, URL-encoded as a query value. Each item includes `sku`, `lot`, `physical`, `reserved`, `unreserved`, `usable`, `availability`, `expiry_required`, `control` and its cursor. `usable` includes only unreserved eligible storage units; physical includes receiving/staging and unavailable stock. A pre-receive held lot is visible with zero custody. Availability is one of `available`, `quarantined`, `expired`, `expiry_missing` or `invalid`. Controls retain `expiry_recorded`, nullable exact `expires_at` and active holds `{id,reason,actor,quarantined_at}`. Hold status takes display precedence; releasing it does not clear expiry or another hold.

Submit these through the ordinary command endpoint using the captured warehouse `expected_version`, a fresh original `request_id`, an attributable reason and no `resource_versions`:

| Command | Fields in addition to `type` | Permission |
|---|---|---|
| `set_product_expiry_policy` | `sku`, `required`, `reason` | Administrator |
| `record_lot_expiry` | `sku`, `lot`, `expires_at`, `reason` | Operator or administrator |
| `correct_lot_expiry` | `sku`, `lot`, nullable `expires_at`, `reason` | Administrator |
| `quarantine_lot` | `sku`, `lot`, `reason` | Operator or administrator |
| `release_lot_hold` | Exact `hold_id`, `reason` | Administrator |

For example, an original label command is `{"type":"record_lot_expiry","sku":"SKU-A","lot":"LOT-A","expires_at":"2027-05-01T00:00:00-07:00","reason":"Reviewed original manufacturer label and facility cutoff"}`. Supply an exact RFC3339 instant with an explicit UTC offset, ordinary seconds and at most microsecond precision; the server normalizes UTC. Offsetless/date-only labels and leap seconds are rejected. The first recording cannot replace an existing original expiry. Corrections require the explicit supervisor operation; setting it null makes a required lot unavailable again. Reasons and actor identities are nonempty, at most 240 UTF-8 bytes, with no surrounding whitespace or control characters. Clients cannot set the recorded actor or admission clock. Preserve the entire original envelope through uncertainty; exact committed retries retain their original outcome after later policy changes.

Fresh admission checks cover allocation, pick, pack, package pack/handoff, legacy ship and transfer dispatch. Replenishment and native readiness use eligible availability. Counts, actual receiving, putaway/moves, unpack, physical unpick, reservation release and eligible cancellation remain recovery paths. A clock crossing or policy update never manufactures a physical movement or silently releases ownership. Operational snapshots include optional server-owned `availability_checked_at`; pure reducer snapshots omit it. Resource-scoped forward commands include relevant product/lot revisions, so a hold followed by release cannot freshen an old review. A coherent new review remains subject to the actual admission clock.

New transfer arrivals preserve actual receiving custody and the earliest known expiry from original dispatch, current source and current destination. Conflicting labels, active source holds or missing required source expiry add independent destination restrictions. Existing destination holds remain. Source changes and arrival are ordered under the same SKU policy locks, including absent policy rows. A later source quarantine does not retroactively rewrite an earlier completed arrival; apply a separately reviewed destination hold when required. This is not a tenant-wide recall or transfer loss-settlement procedure.

Migration 017 records immutable `wms_lot_enforcement`, `wms_lot_changes` and `wms_lot_decisions`. The baseline explicitly leaves pre-upgrade eligibility unknown; no historical dates/decisions are fabricated. Full reconciliation adds `lot_controls` with original policy/eligibility proof. Legitimate current expiry or quarantine does not turn a valid historical decision into a failure. Missing/forged original evidence holds new forward work for repair; custody recovery remains possible. Schema-version 2 events include original `lot_changes`, `lot_decisions` and relevant `previous_order` facts. Consumers must retain these as evidence, not infer a physical movement from a metadata update.


## Cross-facility lot trace

`GET /v1/lot-trace` requires exact case-sensitive `sku` and `lot`, each 1–240 UTF-8 bytes without surrounding whitespace or control characters. It is a tenant-scoped authenticated read, including for viewers, operators and administrators. Shared labels do not prove common manufacturer batch genealogy. No parameter selects another tenant, mutates inventory or authorizes a recall. No matching records return an empty trace; another tenant's matching labels remain invisible.

`section` is one of `facilities` (default), `positions`, `allocations`, `receipts`, `shipments`, `transfers`, `movements`, `policies` or `decisions`. `limit` is 1–100, default 50. The response contains `schema_version:1`, exact labels, `review`, database `checked_at`, `section`, `items`, `next_cursor`, `summary`, `page_source_ok`, `source_checks` and explicit limits. Items retain `warehouse_id`, typed original sort `key`, `document` and `source_ok`. Summary distinguishes physical, reserved, usable storage, receiving and staged quantities, physical reconstruction validity and each section's total count. Transfers retain actual dispatched/arrived quantities and remaining transit; transit is not duplicated into stock at either facility. Shipments retain active packing separately from actual handoffs and explicit legacy/synthetic baselines.

Use the original `review` when selecting another section and the returned `next_cursor` unchanged as `after` when continuing that section. Every page uses one repeatable-read transaction; the review fingerprints all matched facility versions. A matching facility can be observed through history or policy with zero current stock, or as a transfer destination before arrival. Any committed work in a matched facility, or a newly matched facility, returns 409 for the old review. Discard all old pages and explicitly start a fresh review. Changed-and-restored work also invalidates it; an unrelated empty catalog facility does not. A review is a read fingerprint, not an `expected_version`, actor credential or authority for a later write.

Cursors bind schema version, review, section, warehouse and typed original key and are limited to 4,096 bytes. Reviews are 64 lowercase hexadecimal characters. Unknown/duplicate parameters, invalid limits, malformed/mismatched cursors and invalid labels return 422. Current policy/source corruption can fail closed with 503. Read conflicts carry no command-rejection evidence and cannot retire a pending write journal. Preserve long original cursors without shortening or rebuilding them.

`source_checks` is exactly `physical_and_reservation_reconstruction_plus_selected_page_sources`. Independent movement/allocation reconstruction covers reported physical/reservation totals. Per-item/page proofs cover the selected original records; they do not certify all unloaded history or an actual physical count. A false proof is incident evidence and must remain visible. Original shipment and return sources bind their original event, semantic command, actor/result and exact line/lot authorization; legacy/synthetic shipment baselines remain separately labelled. Each page has a fresh database observation clock: expiry may change availability while `review` remains unchanged. Retain each page's clock and refresh stale observations rather than treating all pages as one simultaneous physical inspection.


## Confirmed transfer loss and reversal

Both commands use the ordinary destination warehouse command journal, an administrator identity, the strict captured `expected_version` and no resource map. Source facilities can inspect the records but cannot settle or reverse a destination loss. `record_transfer_loss` requires `transfer_id`, exact `line_id`, positive whole-each `quantity`, `incident_reference` and attributed `reason`. `reverse_transfer_loss` requires one exact original `loss_id`, positive whole-each `quantity` and `reason`. Quantities are 1–1,000,000, additionally capped by the original line's unresolved transit or the original loss's unreversed quantity. Strings are 1–240 UTF-8 bytes without surrounding whitespace or control characters. Actor and time come from server admission.

```json
{"request_id":"original-confirmed-loss","expected_version":42,"command":{"type":"record_transfer_loss","transfer_id":"ORIGINAL-TRANSFER","line_id":"original-a","quantity":2,"incident_reference":"CARRIER-CASE-17","reason":"Carrier and inventory controller confirmed the original missing units"}}
```

`result.entity_id` is the immutable loss/reversal record ID. Original events are `transfer.loss_recorded` and `transfer.loss_reversed`, with a self-contained `transfer_loss_record`, canonical command and original actor/result. Per line, `dispatched = received + lost + in_transit`; `lost` is the sum of original loss quantities minus reversals. No loss/reversal edits stock, physical movements, original dispatch quantities, reservations or prior arrivals. A reversal reopens unresolved transit. Subsequent actual arrivals consume the line's available transit allowance. Untracked quantities do not identify an individual unit or pair an arrival to one loss; controlled SKUs additionally require the exact original serial observations described below. Arrival still enters receiving and preserves conservative expiry/quarantine before ordinary putaway.

Loss/reversal records are append-only. Reversal cannot target another reversal, another line or another facility, and multiple losses cannot borrow one another's reversal allowance. Concurrent arrival/loss work uses the same destination head; strict reviews and independent deferred caps reject double consumption. A final event/outbox failure rolls back the journal and warehouse version. Exact retries preserve the complete original actor-bound request after later work. A loss-settled transfer is excluded from `open_only=true` only when every line has zero remaining transit; it is not labelled a complete physical arrival. Use `open_only=false` to inspect settled history and review a reversal.

`GET /v1/warehouses/{warehouse}/transfers/{transfer_id}/losses` returns `warehouse_id`, its `version`, original `transfer_id`, `destination_warehouse_id`, destination `review`, bounded `items`, `next_cursor` and `page_source_ok`. `limit` is 1–100, default 50. Items retain the original ID/kind, line, quantity, nullable original-loss/incident identities, reason, actor, time, destination version/event and `source_ok`. An original loss also reports its current `reversed` total; reversal rows report zero there. The cursor is the exclusive original destination version. Pass `next_cursor` unchanged as `after` with the original `review`; nonzero `after` without review is rejected. A destination change returns 409 even when reading from an unchanged source. Discard old pages and explicitly refresh. Invalid/duplicate/unknown parameters return 422; foreign/unrelated transfer scopes return 404. Read conflicts contain no command-rejection proof and cannot retire a pending write.

The journal validates original event/command/actor/result sources and the original transfer/caps. False source flags remain incidents and hold changes; quantities alone cannot establish a valid original record. List/trace/reconciliation summaries carry totals and record counts rather than embedding unbounded history. Fetch journal pages for original records. An incident reference records reviewed external evidence; it does not verify a carrier claim, create physical observations or settle commercial liability. Serial command admission adds literal unit ownership as described below. Rerouting and automated damage/disposition remain separate gaps.

## Reviewed pack units

Custody, reservations, plans and canonical command quantities remain integral **each** units. An administrator records `set_unit_pack` with exact `sku`, uppercase `code`, human-readable `label`, integral `each_per_pack`, `enabled` and an evidence `reason`. `EA` is reserved for base each input. A product can enable at most twenty pack codes; factors range from 1 to 1,000,000. Every change creates a new immutable definition ID, including a change back to a previous size. Disabling a code changes no stock.

The operational snapshot includes active `unit_packs`. A client may attach an optional top-level `unit_inputs` array to the original command request:

```json
{
  "request_id": "original-reviewed-request",
  "expected_version": 42,
  "unit_inputs": [{
    "target": [], "sku": "SKU-A", "pack_code": "CASE",
    "definition_id": "original-reviewed-definition-id",
    "pack_quantity": 2, "each_per_pack": 12
  }],
  "command": {
    "type": "move_stock", "sku": "SKU-A", "lot": "LOT-A",
    "from": "RECV-01", "to": "A-01", "quantity": 24
  }
}
```

The server verifies the original current definition and exact checked multiplication; the canonical command must already contain the resulting each quantity. It never rounds, guesses packaging from product names, or substitutes a current factor during replay. Inputs can cover selected quantities in a mixed each/pack request. At most 100 unique targets are allowed, with each resulting quantity capped at 1,000,000.

| Command quantity | Original target |
|---|---|
| `receive`, `move_stock`, `count_stock`, `pick`, `unpick`, `receive_transfer`, `record_transfer_loss`, `reverse_transfer_loss` | `[]` for the one explicit quantity |
| `create_receipt`, `create_order`, `create_return`, `dispatch_transfer` | `[line_id]` from that command's original input line |
| `pack_shipment` | `[line_id, location, lot]` from that exact allocation tuple |

Only a physical `count_stock` may observe zero packs. Commands with implicit quantities, such as allocation, package unpacking and carrier handoff, do not accept conversion inputs. Transfer definitions belong to the facility performing the action: a destination cannot borrow its source's pack ID. A loss or reversal observation still moves no stock. Native OMS/TMS payload quantities retain their existing each contract.

An obsolete or disabled definition on a fresh request returns 409 and requires explicit review. Metadata changes bump the product resource revision, fencing scanner work reviewed before a size changed—even if it subsequently changed back. An already committed original request replays its original result after later changes. Keep its complete `unit_inputs`, original ID and command unchanged during recovery. Empty inputs are omitted from serialization, preserving legacy request fingerprints.

Both read endpoints require an exact `sku`; optional `code`, `limit` (1–100, default 50), `review` and original `after` cursor bind the page scope. `unit-packs` additionally accepts `history=true`. Responses contain `items`, `total_records`, `next_cursor`, `version`, `page_source_ok` and per-record `source_ok`. Current pages include disabled codes. Preserve the original returned version and cursor; a 409 requires a new complete review. These reads create no work. Full reconciliation separately reports `units.ok`, definition-change and observation counts; failed source evidence requires reconciliation rather than a compensating stock count.

The browser offers reviewed pack definitions in Inventory and explicit whole-pack entry in receipt/order plans, receiving, moves, physical counts, picks/returns to bin, package packing, customer return authorizations, transfer dispatch/arrival and loss records. Uncertain saves display the original count, factor and definition ID. Serial command admission is described below; its dashboard scanning workflow is still in progress. Fractional measured goods and packaging genealogy remain unsupported.


<a id="serial-command-admission-in-development"></a>

## Planning: putaway, batch picking, cycle counts and replenishment

These endpoints suggest work. **None of them reserves, moves or counts anything.** Each task carries the exact canonical command to send through `POST /v1/warehouses/{warehouse}/commands`, usually with the reviewed `resource_versions` it was planned against. The command journal, version fences and database proofs apply exactly as for a hand-entered command. All plans need an `operator` or `admin` credential and return 503 if the warehouse needs reconciliation.

| Endpoint | Suggests | Each task is |
|---|---|---|
| `POST /v1/warehouses/{w}/putaway/plan` | a storage bin for stock waiting in receiving | `move_stock` |
| `POST /v1/warehouses/{w}/picking/batch-plan` | one walk that picks several orders | the line's `pick` |
| `POST /v1/warehouses/{w}/counts/plan` | bins and lots due for a cycle count | `count_stock`, once the counter fills in `counted` |
| `GET /v1/warehouses/{w}/replenishment/due` | pick-face refills under the saved rules | `move_stock` |

### Directed putaway

Body (all optional): `from` (receiving bins), `skus`, `zone` (preferred zone for empty bins) and `exclude` (bins never to suggest). For each SKU and lot in a receiving bin, the plan chooses, in order:

1. a storage bin already holding the same SKU and lot (`consolidate_same_lot`);
2. one holding the same SKU (`consolidate_same_sku`);
3. an empty bin in the requested zone (`empty_bin_in_requested_zone`);
4. an empty bin in a zone that already stores the SKU (`empty_bin_in_sku_zone`);
5. any empty storage bin (`empty_bin`).

Ties go to the lowest bin code. Stock with no eligible bin is listed under `unplaced`. There is **no bin capacity model**, so confirm that the stock physically fits. `serial_labels_required` marks tasks that need the serial review.

### Batch picking

Body: `order_ids` (exact orders, in tote order) or `max_orders` (1–50, default 20: the most urgent pickable orders, oldest first), plus an optional `zone`. The response lists `totes` (one per order) and `stops`. Stops are sorted by zone, then bin code with numbers compared numerically (`A-2` before `A-10`), then SKU and lot. Each stop holds its picks with tote, order, line, quantity and command.

- Orders with an open blocking exception, the wrong status, or nothing left to pick are listed in `held_orders`.
- Allocated lots that are quarantined, expired or missing a required expiry are listed in `unavailable`, not planned.
- Picks at the same stop advance the same stock resource version. **After each committed pick, refresh the review or re-plan** before sending the next pick from that stop; the dashboard re-plans automatically.

### Cycle counts

Body (all optional): `interval_days` (`[A, B, C]`, default `[7, 30, 90]`, with A ≤ B ≤ C), `cumulative_shares` (default `[0.8, 0.95]`), `lookback_days` (1–365, default 90), `limit` (1–500, default 200) and `blind` (default `true`).

- SKUs are classed by units picked in the lookback window: the SKUs making up the first 80% of picked volume are A, the next 15% B, and the rest, including never-picked SKUs, C.
- A bin and lot with stock is due when its last `count_stock` is older than its class interval. That time comes from the original count events, because a count that matches stock creates no movement. Never-counted bins come first, then the most overdue.
- Blind plans omit `expected_on_hand`, so the counter isn't biased. `reserved_minimum` shows the floor the command enforces, since a count below reserved stock is rejected.

### Saved replenishment rules

- `GET /v1/warehouses/{w}/replenishment/rules` (any warehouse role) lists the saved rules.
- `PUT` with `{"rules": [...]}` (admin only) replaces them as one change, recorded in the immutable admin audit. An empty list clears them.
- Rules are validated exactly like `POST /replenishment/plan` and are removed with their product or bin.
- `GET /replenishment/due` runs that same planner over the saved rules.

## Labels

`POST /v1/warehouses/{w}/labels` with `{"labels": [...]}` (1–200 labels, any warehouse role) returns `{format: "zpl", dpi: 203, count, zpl}`. Printing changes nothing.

| `kind` | Fields | Label |
|---|---|---|
| `location` | `code` | 4×2 in: bin barcode, zone and kind |
| `product` | `sku` | 4×2 in: SKU barcode and name |
| `package` | `id` | 4×6 in: ship-to, order and package barcode, up to 12 content lines |
| `serial` | `sku`, `serial` | 4×2 in: serial barcode, SKU, lot and custody status |
| `carrier_label` | `package_id` | the package's active carrier label, exactly as the carrier supplied it |

Barcodes are Code 128 and encode the exact stored value. A value that isn't 1–80 printable ASCII characters is rejected with 422, never altered. Free text is escaped, so stored data can't inject ZPL commands, and prints as UTF-8. Unknown records return 404.

## Carrier labels

A carrier label records the actual shipping label for a packed package. It comes from the TMS, or from an operator copying it out of a carrier system. Recording one moves no inventory and does **not** hand off the package; that is still the reviewed `handoff_shipment` command, which can use the tracking number as its `handoff_reference`.

- `POST /v1/warehouses/{w}/packages/{package}/carrier-labels` (operator or admin) records `{carrier, service, tracking_number, rate?, carton?, label}`.
  - `rate` is `{amount, currency}`, with the amount as an exact decimal string (up to 8 decimals, never a JSON number).
  - `carton` is the observed `weight_grams`, `length_mm`, `width_mm` and `height_mm`.
  - `label` is `{"format":"zpl","data":"^XA…"}` (at most 64 KiB) or `{"format":"url","url":"https://…"}`.
- A package has at most one active label. Recording a second returns 409; void the first.
- Labels can't be added after the package is handed off or unpacked (409). Recording takes the warehouse lock, so it can't race a handoff.
- `GET …/carrier-labels` lists every label, including voided ones. `POST …/carrier-labels/{label}/void` with `{"reason"}` voids the active one.
- Rows are immutable apart from that single void, enforced by the database.

**For TMS integrators:** this is the WMS side of a label contract. The TMS buys or rates the label with its carrier accounts and records it here; the WMS never calls a carrier.

## Serial command admission

Serial tracking records every unit of a product by its printed label. This section is the request contract. For how it works inside, see the [serial design guide](serial-design.md); for what has been qualified, see [evidence and limits](evidence.md).

### Choosing serial tracking for a product

- An administrator sets `serial_controlled: true` in `create_product`, **before** that SKU exists anywhere in the tenant. Every later facility catalog entry for the SKU must use the same mode.
- The flag defaults to `false` and is omitted from requests when false. Existing untracked products, and their original request bytes, keep that mode.
- Changing a catalog flag never gives existing inventory invented identities.

### Sending labels with a physical command

A physical command for a serial-controlled SKU lists the exact label of every unit it moves, in `serial_inputs`:

```json
{
  "request_id": "original-literal-move-request",
  "expected_version": 42,
  "serial_inputs": [{"target": [], "sku": "SERIAL-SKU", "serials": ["Unit-A", "unit-a"]}],
  "command": {"type": "move_stock", "sku": "SERIAL-SKU", "lot": "ORIGINAL-LOT", "from": "RECV-01", "to": "A-01", "quantity": 2}
}
```

Each `serial_inputs` group names a `target`, the part of the command it belongs to:

| Command | `target` |
|---|---|
| Receive, move, count, pick, unpick, transfer arrival, loss, loss reversal | `[]` (the single quantity) |
| Return authorization, transfer dispatch | `[input_line_id]` |
| Package packing | `[order_line_id, original_allocation_bin, lot]` |
| Handoff, unpacking, whole-order shipment, RMA cancellation | none: the server derives the exact original units and accepts no substitute set |
| Metadata receipt and order plans | none: they move no units |

### Label rules

- Labels are **case-sensitive** (`Unit-A` and `unit-a` are different units), stay in their original lot, and are unique per tenant and SKU across all facilities.
- Leading or trailing whitespace and control characters are rejected. The server never trims, normalizes or invents labels.
- The command's each quantity must equal the number of labels. With pack conversion in the same command, every resulting each must be identified.
- A count is the **complete** set physically observed. A witnessed zero is an explicit group with `serials: []`.
- One command fits up to 100 unique targets, 10,000 labels and 10,000 identity changes, within the 256 KiB body limit. Larger closures are rejected in full. Partial or truncated count sets are not valid full counts; reviewed large-count sessions are not available yet.

### Recovery and integrity

- For a retry, resend the complete original `serial_inputs`, actor, facility, request ID and reviewed command unchanged. An exact committed retry returns the original result, even after later ownership changes.
- If the final outbox write fails, serial and physical facts roll back together.
- Full reconciliation reports `serials.source_ok` (every recorded fact matches its original event, which exposes source damage even when stock balances) and `serials.physical_ok` (serial custody matches stock, staging, packages and transfers).
- New commands are held for a scanned identity whose current record has lost its original source.
- A serial observation is not a refund, a carrier claim check or an automatic physical confirmation.

### Dashboard serial observation and recovery

- The product form offers the whole-each or individual-serial choice. It is tenant-wide per SKU and cannot relabel existing inventory.
- Every explicit controlled quantity goes through a shared serial review before saving: receiving, moves, full counts, picks, returns to bin, RMA authorization, package lines, transfer dispatch and arrival, loss and reversal. Unpack, handoff, classic whole-order shipment and RMA cancellation use the server's derived original identities.

**Planning endpoint.** `POST /v1/warehouses/{warehouse}/serial-observation-plan` with `{plan_id: <UUID>, command: <canonical command>}` tells the client which label groups a command needs.

- It resolves up to 100 original target groups in one read-only repeatable-read transaction, including records outside the operational snapshot. The caller needs the command's normal role.
- The response has the correlation ID, facility, canonical command type, controlled `targets: [{target,sku,each_quantity}]`, the required label count and an informational `observed_warehouse_version`.
- Errors: missing targets or products → 404; duplicate targets, unsupported commands or more than 10,000 required labels → 422; inconsistent mode or head evidence → 503.
- **The plan authorizes no save and replaces no warehouse or resource version check.** It creates no command receipt, event, movement or outbox record. It is a REST preparation endpoint, not an MCP tool.

**Review screen.**

- Shows the original order, receipt, transfer, loss, source and destination bin and lot from the captured command. Package groups show the order line, allocation bin and lot; transfer groups show the source bin and lot. This context never changes the request.
- Captures labels exactly as scanned, with no prefill, trimming or generated identities. Repeated labels for one SKU across groups are rejected, and each target needs its full each quantity. A zero count needs an explicit physical witness.
- Shows at most 50 labels per group at once, with navigation. If the complete body would exceed 256 KiB, the review is held with every label still visible; nothing is truncated or sent.

**Saving and reload recovery.**

- Only after the operator confirms does the browser store the complete original command, request ID, actor and facility, reviewed versions, pack observations and serial groups in its session journal, and then send the save.
- After a reload, it validates the stored groups and finds or retries that exact request, without replanning or rescanning. Malformed stored observations hold recovery and keep the original browser data.
- Cancelling an unsaved review keeps the form. The server still validates sources and quantities inside the atomic command transaction.

**Inventory review panel.** Shows current custody, exact unit history and original observations across the tenant or at one facility.

- It keeps the SKU review and cursor across pages, shows source disagreement, clears records when a page is expired, invalid or incomplete, and refreshes only on request.
- It keeps at most 1,000 displayed records; a label or facility filter narrows further.
- A newer warehouse refresh invalidates displayed evidence; another facility's activity is caught on the next page or refresh.
- No read or scan is an independent physical confirmation. Browser checks simulate keyboard-wedge scanners; scanner hardware and facility acceptance are separate.

### Serial identity review pages

Three read endpoints, available to viewers, all scoped to the authenticated tenant:

| Endpoint | Required | Optional filters |
|---|---|---|
| `GET /v1/serials` (current custody) | exact `sku` | `serial`, `warehouse_id`, `status` (`on_hand`, `staged`, `shipped`, `in_transit`, `lost`, `missing`) |
| `GET /v1/serial-history` (unit history) | exact `sku` and case-sensitive `serial` | `warehouse_id` |
| `GET /v1/serial-observations` (scanned label sets) | exact `sku` | `serial`, `warehouse_id` |

`warehouse_id` means the unit's facility (or either transfer facility) for current custody, and the facility that performed the action for history and observations.

**Paging.**

- `limit` is 1–100 (default 50). To fetch the next page, send the returned `review` and `next_cursor` (as `after`) with the same filters.
- Pages are tied to the **tenant/SKU identity version** (`version_scope: "tenant_sku_identity"`), so an arrival at another facility invalidates them even if this warehouse's version is unchanged. A 409 means: discard the retained pages and start the review again.
- Malformed, unknown or changed-scope cursors return 422. Page reviews do not replace the version checks on physical commands.

**Responses.**

- Every response has `items`, `total_records`, `next_cursor`, `page_source_ok` and original event references. History adds `current_source_ok` and each unit's exact before and after custody, revisions, original targets, actor and time.
- Current rows keep their last source reference; damaged rows stay visible with false source flags.
- Observation pages return one label occurrence per item, with its original event, group ordinal, member ordinal and group size. A genuinely empty group has `observed_serial: null`, `serial_ordinal: 0` and size zero. Unchanged and zero counts stay visible.
- Items are limited by count and by a 128 KiB encoded JSON budget. A large page stops before an item and returns a continuation; no response copies an unbounded label array or a complete command.

**Errors.**

| Condition | Status |
|---|---|
| SKU is not serial-controlled | 422 |
| Unknown unit, history view | 404 (current and observation views return empty results) |
| Invalid mode or head evidence, steps without a current record, a requested label found only in observations, or one oversized stored record | 503 |

**What a page does and doesn't prove.**

- Source flags check mode and head evidence plus the **selected records and their source events**, in one read-only repeatable-read transaction. Full reconciliation is separate.
- `total_records` counts stored records, not verified physical units. An empty whole-SKU page does not prove that no identity appears in any historical observation, and a partial page is not a complete physical observation.
- Neither a read nor a zero result confirms a physical count. High-cardinality read cost and deployed MCP and device acceptance are not yet qualified.


## Visual operations console

Authenticated GET `/v1/warehouses/{warehouse}/console` supplies reconciliation-checked flow, exact decimal-string each totals, version-fenced bin pages and optional configured floor-plan geometry. GET `/v1/warehouses/{warehouse}/console/products/{sku}` supplies original catalog/label evidence and optional image metadata. Its `/image` subresource requires the original review version, image SHA256 and identity scope. All three are read-only and use `Cache-Control: no-store`. See [configuration, contract and fallback behavior](visual-console.md).
