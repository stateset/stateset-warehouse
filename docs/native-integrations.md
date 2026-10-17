# Native OMS and TMS contracts

StateSet Sync owns commercial orders, this repository owns physical warehouse inventory and execution, and StateSet Freight owns transportation planning and execution. The native adapters in `src/native.rs` use the HTTP contracts present in the sibling repositories. An ordinary warehouse event is not sent to either application's `/events` route.

Configure an explicit mapping for each warehouse tenant:

```json
{
  "acme": {
    "oms": {
      "base_url": "https://oms.example.test",
      "tenant_id": "oms-acme",
      "api_key": "replace-with-tenant-scoped-secret"
    },
    "tms": {
      "base_url": "https://freight.example.test",
      "tenant_id": "freight-acme",
      "bearer_token": "replace-with-tenant-scoped-operations-secret"
    }
  }
}
```

Supply this map through `WMS_NATIVE_INTEGRATIONS_JSON`. Configuration and summaries never serialize the credentials. HTTP is accepted for loopback and the named internal service hosts; other hosts require HTTPS. The client does not follow redirects, limits connection establishment to three seconds and the whole request to eight seconds, and bounds response bodies at 2 MiB, including streamed responses without Content-Length. Errors report the service and status without returning remote response bodies or secrets.

The authenticated session exposes `native_integrations` with this shape:

```json
{
  "oms": {"configured": true, "read_contract": "GET /v1/tenants/{tenant}/orders", "shipment_writeback": "held_missing_remote_precondition"},
  "tms": {"configured": true, "write_contract": "POST /v1/sync/import", "behavior": "source_verified_planned_load", "requires_operator_transport_plan": true}
}
```

## Warehouse API and durable recovery

Every route uses the authenticated warehouse tenant; caller-supplied tenant and role headers cannot change that identity. Native external operations require an operator or administrator. The `oms` role may import an order using its explicitly configured upstream source, and inspect its own native receipt. Demo mode cannot make native HTTP requests.

| Warehouse route | Behavior |
| --- | --- |
| `GET /v1/integrations/oms/orders?page=1` | List the configured tenant's canonical OMS orders. |
| `POST /v1/warehouses/{warehouse}/imports/oms` | Import a reviewed source revision through the ordinary atomic warehouse command path. |
| `GET /v1/warehouses/{warehouse}/orders/{order}/readiness` | Export packed readiness and the explicit OMS writeback hold. |
| `POST /v1/warehouses/{warehouse}/orders/{order}/transport` | Retain an exact native transport intent before making any TMS mutation. |
| `GET /v1/warehouses/{warehouse}/native-requests/{request_id}` | Recover the original actor's result, retained evidence, and delivery attempt history. |
| `GET /v1/warehouses/{warehouse}/orders/{order}/oms-fulfillment` | Return a source-checked held proposal linked to a committed physical handoff event. No OMS POST occurs. |

An import body is:

```json
{
  "request_id": "operator-generated-stable-request-id",
  "expected_version": 42,
  "oms_order_id": "00000000-0000-4000-8000-000000000999",
  "source_updated_at": "2026-10-11T12:00:00Z",
  "details": {"customer": "Verified customer", "destination": "Verified destination", "priority": "normal"}
}
```

The source revision must match the order returned by OMS. Unknown upstream fields remain in the complete original JSON response envelope in `source_snapshot`; the typed OMS projection and translated command are frozen separately. A retry uses that command and its original actor-bound normal command receipt. It never fetches a changed source to reinterpret an import that might already have committed.

A transport body is `{request_id, expected_version, plan}`, where `plan` is the exact Freight load plan below. Native request IDs contain 8–100 letters, digits, hyphens or underscores. The input fingerprint binds the whole typed request, operation, warehouse and path order. Reusing an ID with another actor or body returns a conflict. A foreign tenant cannot resolve the warehouse or its requests.

Migration `003_native_operations.sql` gives each order at most one transport intent. Preparation checks the version under `SELECT FOR UPDATE` on the relational warehouse head, verifies packed state and open exceptions, and freezes the exact request and source response before HTTP. The first send rechecks packed state, exceptions, source identity and line quantities under the same warehouse lock; unrelated stock work may advance the warehouse without changing those facts. No database lock spans a remote request.

Transport receipt states are `frozen`, `sending`, `uncertain`, and `completed`. A local import can also enter `local_conflict` when its original warehouse command cannot commit. Delivery claims use a 30-second lease and compare the lease token when committing the outcome. A process crash or any remote error leaves acceptance uncertain. Retrying the same retained transport intent uses its original remote idempotency key, payload, normalized recipient base URL and Freight tenant, even if the source or local pick state changed after an earlier possible acceptance. Changing the endpoint or tenant holds the original intent rather than delivering it to a new authority. Creating another request for the same order is blocked. The receipt retains each attempt's accepted or uncertain outcome; expired leases retain the original unfinished attempt as uncertain before another claim.

Database triggers reject changes to frozen intent keys, actor, input fingerprint, input, source snapshot, command/plan, source version and creation time. Transport order identity is immutable; an import may populate its resulting local order identity on completion. Completed native receipts and finished attempt records cannot be rewritten or deleted. Credentials are never part of these journal records.

## Order ingress

The adapter calls these actual Sync routes using `x-stateset-api-key`:

| Request | Required response shape | Native source |
| --- | --- | --- |
| `GET /v1/tenants/{tenant}/orders?page=1&page_size=100` | `data.items`, `data.pagination` with camelCase metadata | `/home/dom/stateset-sync-server/src/http/handlers/orders.rs::list_orders`, `src/http/pagination.rs` |
| `GET /v1/tenants/{tenant}/orders/{UUID}` | `data` containing canonical `Order` | `src/http/handlers/orders.rs::get_order`, `src/domain/mod.rs::Order` |
| Authentication | `x-stateset-api-key`, path-bound tenant | `src/http/auth.rs`, `src/http/tenant.rs` |

The WMS validates UUID identity, any returned `tenantId`, revision timestamps, source status, line identity, SKU, and quantities. Only `pending` and `processing` orders become new forward warehouse work. Cancellation, return, fulfillment, shipment, delivery and unknown statuses cannot silently create another fulfillment.

Canonical Sync `Order` does not contain a customer address. An importing operator must supply the customer, verified destination, and warehouse priority. The adapter does not manufacture them from an order number. Product setup remains a separate authorized warehouse operation; an unknown SKU fails warehouse command validation.

Native `source_order_id` is `oms:{upstream-tenant}:{UUID}`. This keeps identical upstream IDs from another source tenant distinct. Each line receives a stable hash of its original `shopifyLineItemId`; two lines for the same SKU retain separate reservation owners. When the original ID is absent, the line's source position is used. Position-based identities cannot prove continuity after upstream line reordering, so line changes require reconciliation rather than automatic replacement.

Native import represents the complete OMS order. Migration `004_native_source_ownership.sql` therefore enforces one owner for that source across every warehouse belonging to the WMS tenant, including manual commands using the native `oms:` prefix. A frozen import reserves that ownership before its warehouse command commits. Native preflight returns a conflict for an existing owner; a transaction-scoped source advisory lock also serializes competing imports across different warehouse heads. Warehouse-assigned split orders require an explicit OMS assignment contract before the platform can import separate work for the same source.

Sync represents quantities as JSON numbers originating in an `f64` field. WMS uses `serde_json` with `arbitrary_precision` and parses the received decimal/exponent digits exactly. It accepts whole each quantities from 1 to 1,000,000, including `2.0`, and rejects fractional quantities such as `1.00000000000000001`. Precision already lost inside the originating service cannot be recovered by an adapter.

## Transportation handoff

The supported native TMS write is `POST /v1/sync/import`, authenticated with a tenant-scoped Bearer credential. Freight authorizes this route with `OperationsWrite`; its `integration` role alone cannot create a load. See `/home/dom/stateset-freight/src/api.rs::import_order`, `src/auth.rs::route_capability`, `src/domain.rs::NewLoad`, and `src/store.rs::fingerprint`.

Before freezing a new intent, and again before every native POST, the adapter calls Freight's actual `GET /v1/access` route (`src/api.rs::access`). It requires the returned `tenant_id` to equal the frozen configured Freight tenant and the returned capabilities to include `operations.write`. Thus a secret can rotate within the same tenant, while a credential selecting another tenant or lacking permission cannot send the retained intent. This read uses the same bounded, timed HTTP client. Endpoint and configured tenant mismatches are rejected before even making that read.

The exact body is:

```json
{
  "order_id": "00000000-0000-4000-8000-000000000999",
  "load": {
    "reference": "FR-WMS-1048",
    "customer_id": "existing-freight-customer-id",
    "mode": "FTL",
    "equipment": "Dry van",
    "commodity": "Consumer goods",
    "weight_lbs": 24000,
    "miles": 180,
    "origin": {
      "facility": "Portland DC", "city": "Portland", "state": "OR",
      "date": "2026-10-20", "address_lines": ["15 Dock Street"],
      "postal_code": "97201", "country": "US", "contact": "Dock one",
      "phone": "555-0100", "instructions": "Ready for transportation planning"
    },
    "destination": {
      "facility": "Seattle DC", "city": "Seattle", "state": "WA",
      "date": "2026-10-21", "address_lines": ["25 Receiving Ave"],
      "postal_code": "98101", "country": "US", "contact": "Receiving",
      "phone": "555-0101", "instructions": "Appointment to be confirmed"
    },
    "buy_cents": 123456,
    "sell_cents": 150000
  }
}
```

These values are illustrative. The operator supplies real facilities, dates, weight, mileage, existing TMS customer identity, and commercial prices. Zero prices are accepted only when explicitly supplied. The WMS does not invent any of these facts. The supported adapter uses Freight's direct origin/destination route subset and validates its wire-safe stop text, supported modes and equipment, cents, weight, distance and date ordering.

Preparation requires a reconciled, fully packed warehouse order with a matching native OMS source tenant. `prepare_transport` returns a serializable `PreparedTransport` containing the exact body, stable tenant/warehouse/order idempotency key, and warehouse readiness evidence. The server must persist this value in its command receipt before calling `execute_transport`. Retrying sends the same frozen request. Freight's own `sync.import` fingerprint binds the operator's source order ID and load plan while allowing its internally refreshed OMS snapshot to change between attempts.

For a new intent, `verify_transport_source` fetches the configured canonical OMS order and verifies its active status and exact line identities, SKUs, and each quantities against warehouse readiness. Existing uncertain requests retry the frozen import without repeating this preflight, allowing Freight to replay its original receipt after a later source change. The returned Freight source snapshot is also checked against frozen readiness. A source mismatch after HTTP is reported as uncertain and requires review of the possible planned load rather than a false acceptance.

Freight independently fetches the canonical OMS order before creating the linked planned load. It rejects unsafe source statuses and duplicate source links. Its response is `{data: Load, replayed: boolean}`; the adapter checks that it confirms the exact source order and reference with initial `planned` status. The warehouse does not claim tender acceptance, dispatch, pickup, transit, delivery, a signed POD, or carrier capacity.

The Freight credential selects a Freight tenant. That tenant's own Sync configuration must point at the same upstream OMS tenant configured here. The canonical Sync `Order` omits tenant identity, so identical order IDs alone cannot attest the remote deployment's tenant mapping. Source-line comparison detects differing source content, and an explicitly returned mismatched `tenantId` is rejected; deployment validation must still verify the two services' tenant mappings. A source change between preflight and Freight's fetch may create a planned load requiring review. Freight's source-review controls remain responsible for later transportation commitments.

The readiness evidence remains an explicit `warehouse.order_ready` export with warehouse version, source reference, stable line IDs, and quantities in each. Current Freight has no native endpoint accepting that WMS readiness object. Creating a planned load is the supported integration; it is not confirmation that Freight consumed every warehouse readiness field. Freight currently links one load per OMS order, so the adapter does not claim native support for splitting that source into several transport loads.

## OMS writeback is held at an actual contract gap

Sync's `POST /orders/{UUID}/fulfillment` accepts `{status, fulfillmentData, netsuiteCashSaleId}` and returns `data.ok`. However, `/home/dom/stateset-sync-server/src/database/orders.rs::update_order_fulfillment` unconditionally updates **both** order status and fulfillment status. It has no expected-version predicate, `If-Match` handling, or terminal-state fence. Fetching the latest order before posting cannot prevent a cancellation or delivery from committing between those requests. The alternative `/tracking` endpoint unconditionally sets `SHIPPED`, so it cannot safely carry readiness either.

`prepare_oms_fulfillment` re-fetches the source, verifies stable lines and quantities and permissible source status, and produces the exact concrete fulfillment proposal only after a complete physical warehouse handoff. The proposal includes the source revision, event ID, actor-independent stable idempotency identity, carrier, dock handoff reference and handed-off timestamp. `dispatch_oms_shipment` **does not issue the unsafe POST**. It returns the explicit `OMS_WRITEBACK_HOLD` reason for durable review and reconciliation. Readiness, partial picking and partial handoffs never become a fabricated upstream `FULFILLED` state.

Automatic native OMS mutation requires an OMS change: enforce a source revision and terminal-state rule in the same database statement that records fulfillment, and retain the original event ID idempotently. This repository cannot guarantee that behavior by adding a client header that the actual OMS ignores. Until that contract exists, the retained WMS evidence and held proposal are the truthful supported result.

## Validation and limits

`tests/native_contracts.rs` contains source-derived canonical OMS and Freight fixtures plus local HTTP contract servers. It verifies tenant-bound routes and credentials, actual response envelopes, duplicate-SKU line ownership, decimal precision, source status restrictions, explicit transport planning, native TMS idempotency replay, source/key scope rejection, response bounds, redirect refusal, and the absence of an unsafe OMS POST.

`tests/native_api.rs` adds permission and body-validation checks without a database, plus Postgres tests for immutable evidence, expired leases, first-send readiness, one intent per order and crash recovery after a normal warehouse command committed. Its complete authenticated HTTP/Postgres test lists and imports an original OMS response with unknown fields, rejects a stale source revision, replays the original import, allocates and packs duplicate-SKU lines, then simulates a TMS commit followed by HTTP 500. Retrying after a later OMS cancellation and local unpick produces one planned load with the same original key and payload, records uncertain then accepted attempts, and makes no OMS mutation.

A concurrent two-warehouse regression proves that only one frozen full-source import can win. It then commits that owner and verifies that bypassing the native route with a manual native-prefix command still cannot create a second warehouse order for the same source.

These tests do not establish deployed credentials, cross-service tenant mappings, real carrier acceptance, physical inventory counts, or live warehouse usability. Those require the configured applications and operational evidence. The sibling applications remain unchanged by this adapter implementation.
