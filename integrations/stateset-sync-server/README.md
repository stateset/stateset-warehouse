# Conditional OMS fulfillment receiver

This is an additive companion change for `stateset-sync-server`, prepared against upstream commit `1e406933c888b1c7efa4253174d72b08d3a2531a`. It closes the receiver-side race that prevents safe WMS writeback through the existing unconditional fulfillment endpoint. The OMS worktree has not been changed and this receiver has not been deployed.

The complete [upstream patch](upstream.patch) adds one module, one migration and the existing tenant-router/RBAC registrations. It changes no existing fulfillment, tracking or NetSuite endpoint.

## Review and apply

```bash
git -C /path/to/stateset-sync-server apply --check \
  /path/to/stateset-warehouse/integrations/stateset-sync-server/upstream.patch
git -C /path/to/stateset-sync-server apply \
  /path/to/stateset-warehouse/integrations/stateset-sync-server/upstream.patch
```

Run the upstream repository's required build, authorization and database checks, review its migration and deploy through its normal release process. The added Rust module also contains three input/digest unit tests and one explicitly ignored real PostgreSQL concurrency test. Apply the migration before serving capabilities. The capability result is read from the migration-installed SQL contract function, so an application binary alone cannot advertise durable rejection support on an older database.

After deployed tenant acceptance, opt the corresponding WMS OMS connection into `shipment_writeback: "conditional_v1"`. Credentials need `orders:read` for capabilities/source review and `orders:write` for handoff. The WMS continues holding writeback by default. It must never fall back to the old unconditional endpoint.

## Wire contract

Both methods use `/v1/tenants/{tenant_id}/warehouse-fulfillments`, the existing tenant authentication and the normal success envelope `{ "data": ... }`. GET/HEAD require `orders:read`; POST requires `orders:write`. GET returns:

```json
{
  "data": {
    "contract": "stateset.wms-fulfillment.v1",
    "tenant_id": "tenant-a",
    "conditional_revision": true,
    "terminal_state_fence": true,
    "durable_idempotency": true,
    "durable_rejections": true
  }
}
```

POST accepts exactly these fields and rejects unknown top-level/line fields:

```json
{
  "order_id": "00000000-0000-0000-0000-000000000001",
  "request_id": "00000000-0000-0000-0000-000000000004",
  "event_id": "00000000-0000-0000-0000-000000000002",
  "expected_updated_at": "2026-10-17T00:00:00Z",
  "warehouse_id": "wh-main",
  "warehouse_order_id": "00000000-0000-0000-0000-000000000003",
  "carrier": "Carrier",
  "handoff_reference": "HANDOFF-1",
  "handed_off_at": "2026-10-17T01:00:00Z",
  "lines": [
    {
      "line_id": "oms-line-97fe5e70932d872b5cf921533a09b06d77167d49b49f04edec61ec9330582476",
      "sku": "SKU-A",
      "quantity_each": 2
    }
  ]
}
```

`event_id` identifies one physical handoff. `request_id` identifies one immutable, reviewed delivery attempt. It is also the HTTP `Idempotency-Key`. A retry after a timeout must preserve the entire original body and key. A validated, durable rejected acknowledgment permits an operator to review current OMS state and submit a new request ID for the same physical event. An unknown outcome never permits a new identity or refreshed revision.

UUIDs use canonical lowercase hyphenated spelling. `warehouse_id` is 1–100 ASCII letters, digits, hyphens or underscores. Carrier and handoff reference are 1–240 UTF-8 bytes; SKU is 1–100 UTF-8 bytes. These text fields reject surrounding whitespace/control characters. Timestamps are RFC3339 strings of at most 64 bytes and retain their input spelling for fingerprints. Lines are 1–100 distinct identities with integer quantities in 1–1,000,000 whole each units. The contract does not impose a future-handoff clock tolerance.

Line identities exactly match the WMS native importer: `oms-line-` plus lowercase SHA256 of `source:{shopifyLineItemId}`, or `position:{zero_based_index}` when the canonical line has no source identity. SQL compares every identity, SKU and exact quantity against current `orders.line_items` under the order lock. Same-SKU lines are independent. Fractional, duplicate or otherwise incompatible source lines cause a durable rejection; the receiver never rounds source quantities or infers a remaining partial shipment.

The independently calculated `request_digest` is lowercase SHA256 over compact UTF-8 JSON with all object keys recursively sorted. Array order and string/timestamp spelling remain intact. No digest field appears in the request body. The provider validates and serializes the typed body, then computes its own digest.

## Atomic application and acknowledgments

The SQL function locks the tenant's canonical order row, then checks the immutable request receipt before status/revision checks. A repeated request returns its original acknowledgment with `replayed: true`, including after a later cancellation. A changed body/digest under an existing request ID conflicts.

A new handoff requires canonical status PENDING or PROCESSING, and clearly unfulfilled status: NULL/blank, UNFULFILLED or PENDING. Partial, terminal and unknown fulfillment states, source cancellation and an incompatible source snapshot reject the full-original-order handoff. On acceptance, only `status`, `fulfillment_status` and `updated_at` change. NetSuite identifiers, tracking and source payloads remain untouched. Physical carrier/handoff evidence stays in the immutable receipt.

Accepted acknowledgments include contract, tenant/order/request/event IDs, request digest, `status: "fulfilled"`, `applied_updated_at` and `replayed`. Canonical status becomes FULFILLED. This records warehouse fulfillment evidence; it does not fabricate NetSuite settlement or carrier delivery.

Valid requests that fail a business precondition return HTTP 200 with an immutable rejected acknowledgment. It includes the same identity/digest fields, `status: "rejected"`, `observed_updated_at`, `replayed` and one reason:

- `source_revision_changed`
- `source_state_not_eligible`
- `source_lines_changed`
- `order_already_fulfilled`

The rejection and original reviewed request are durable evidence that the attempt did not apply fulfillment. Replaying an old rejected request stays rejected even after a reviewed replacement succeeds. Accepted-only unique indexes prevent another accepted receipt for the same tenant/order or physical event; rejected attempts do not consume that ownership. A final receipt-write failure rolls back the canonical order update. Missing/foreign orders return 404; malformed requests return 422; reused request identities with another payload return 409. A raw conflict or storage error supplies no durable non-application proof and must preserve the original request for retry.

## Executed verification

```bash
python3 integrations/stateset-sync-server/check.py
python3 integrations/stateset-sync-server/check-upstream.py /path/to/stateset-sync-server --tests
WMS_TEST_DATABASE_URL=postgresql:///isolated_test?... \
  python3 integrations/stateset-sync-server/check-upstream.py /path/to/stateset-sync-server --postgres
WMS_TEST_DATABASE_URL=postgresql:///isolated_test?... \
  cargo test --locked --test oms_receiver -- --ignored --test-threads=1
```

The socket-free proof executes the actual migration and functions in PostgreSQL 12.22. It verifies exact repeated-SKU receipt lines, revision/terminal/partial-state fences, immutable success/rejection replay, explicit reviewed replacement, tenant isolation, unrelated-field preservation and fault-injected rollback after the canonical update.

The compatibility runner reads an existing checked upstream library and exact dependency fingerprints, writes temporary artifacts, and typechecks the receiver and its mounted Axum 0.7 router against actual upstream `AppState`, `TenantRequest`, `ApiError`, `ApiSuccess` and database-pool types. It executes three input/digest unit cases and, with `--postgres`, the real SQLx concurrent-order/event, cancellation and rejection/review case against a unique temporary schema in an isolated test database. All four passed locally. The portable [OMS receiver suite](../../tests/oms_receiver.rs) executes the actual migration in five PostgreSQL concurrency cases without an upstream build cache, including deterministic row-lock barriers and immutable late rejection replay. It also passed. These checks do not rebuild the full OMS, execute upstream authentication middleware, or establish deployment acceptance. The full patch passes `git apply --check` against the referenced worktree.
