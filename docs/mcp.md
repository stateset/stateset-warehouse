# Warehouse MCP server

The WMS API includes a native MCP server at **`/mcp`**, implemented with the [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk). It uses Streamable HTTP with JSON responses and no retained transport sessions. The same process serves the dashboard and REST API. No separate Node service or database credential is needed by an agent.

Connect an MCP client that supports HTTP and explicit bearer credentials to `http://127.0.0.1:8088/mcp` for a local workspace, or your configured HTTPS ingress plus `/mcp`. Send `Authorization: Bearer <individual WMS credential>` on **every** request. The server accepts existing WMS API keys and verified WorkOS access tokens through the same authentication middleware as REST. Configure the client credential through its secret storage. This endpoint does not implement OAuth client registration, authorization discovery, or a stdio launcher; a client that requires those cannot connect merely by entering a URL.

## Start and discover

Use the normal [real-workspace setup](../README.md#real-workspaces). For example, configure an individual service key with the required tenant, actor and least necessary role in `WMS_API_KEYS_JSON`, then start the usual WMS binary. `WMS_DEMO=true` is only for disposable synthetic work. A process listening on loopback allows local MCP by default.

This actual protocol request reads the authenticated session. `WMS_MCP_TOKEN` below is a local shell variable containing an existing WMS bearer credential; it is not a server configuration variable.

```bash
curl --fail-with-body --silent --show-error http://127.0.0.1:8088/mcp \
  --header "Authorization: Bearer $WMS_MCP_TOKEN" \
  --header 'Content-Type: application/json' \
  --header 'Accept: application/json, text/event-stream' \
  --header 'MCP-Protocol-Version: 2025-11-25' \
  --data '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"wms_session","arguments":{}}}'
```

Send `{"jsonrpc":"2.0","id":2,"method":"tools/list"}` with the same headers to discover your permitted tools and complete input schemas. Clients using the older initialize lifecycle can negotiate it; current clients can use the [2026-07-28 stateless protocol](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http), including its routing headers and required request metadata. The SDK handles protocol negotiation and validation. No initialization or session ID carries warehouse authority into another HTTP request.

## Tool contract

| Tools | Use |
| --- | --- |
| `wms_session`, `wms_warehouses`, `wms_snapshot` | Identify the current principal, select a facility and capture coherent warehouse/resource versions. |
| `wms_order`, `wms_receipt`, `wms_history`, `wms_event` | Read canonical retained work and page history, including archived records. |
| `wms_transfer_losses` | Page original loss/reversal records at either original facility, retaining the destination review and original cursor. |
| `wms_lot_trace` | Read tenant-wide exact SKU/lot evidence across facilities, preserving section, original review and original cursor. |
| `wms_serials`, `wms_serial_history`, `wms_serial_observations` | Review exact literal custody, original unit lineage and original label occurrences/empty groups with a global tenant/SKU review. |
| `wms_unit_packs`, `wms_unit_observations` | Page exact-SKU definitions and original pack observations with their original review and cursor. Stock stays in each units. |
| `wms_set_unit_pack` | Administrator records an immutable reviewed pack definition or disables a code; later sizes never reinterpret earlier requests. |
| `wms_lots` | Page current lot policy, exact expiry, independent active holds and physical/reserved/usable quantities at the returned server instant. |
| `wms_shipments`, `wms_return_eligibility`, `wms_transfers` | Review original package/line/lot claims, actual handoffs, capped return allowances and goods in transit. |
| `wms_reconciliation`, `wms_outbox`, `wms_operations`, `wms_metrics` | Inspect independent proofs and delivery/operating evidence; role restrictions apply. |
| `wms_command_receipt`, `wms_native_receipt` | Recover original journaled warehouse or native requests under their original actor. |
| `wms_<command.type>` | Execute each canonical physical/business command, including partial fulfillment and facility transfers. |
| `wms_replenishment_plan`, `wms_readiness` | Review plans/readiness; these do not establish physical work. |
| `wms_oms_orders`, `wms_import_oms`, `wms_oms_proposal`, `wms_transport`, `wms_oms_fulfillment` | Use only the tenant's explicitly configured native services and existing guarded contracts. |
| `wms_requeue_delivery` | Administrator requeue of a dead letter after evidence review; queue-state review is required after a lost response. |

Path fields such as `warehouse_id` and `order_id` are top-level arguments. Optional pagination fields are also top-level. A POST with JSON accepts the **complete canonical REST body** in `request`. Canonical command discovery schemas are generated from the same Rust types as REST and retain only definitions reachable from that tool's input. The two iCommerce tools use explicit compatibility schemas over the pinned upstream models. Administrators can discover all 70 tools in version 0.2.0; other roles receive a smaller catalog. Each command tool requires its own exact `command.type`; `wms_pick` cannot submit a count, cancel or ship command. Unknown arguments, principals and target URLs are rejected. The server safely encodes resource IDs into fixed internal routes; an ID cannot choose another endpoint or host.

For a physical command, supply `request_id`, the captured `expected_version` and the complete `command`. Supply `resource_versions` only as the exact supported scope from the reviewed snapshot, following the [API concurrency contract](api.md). The adapter does not generate IDs, read fresh versions on the caller's behalf, remove stale fences or retry writes automatically. A command body larger than the REST limit of 256 KiB is rejected. MCP HTTP messages are limited to 512 KiB and returned warehouse bodies to 8 MiB. Oversized reads fail explicitly without a truncated snapshot; use paginated history or detail tools.

Every completed tool response has `structuredContent` with this wrapper:

```json
{"http_status":200,"body":{"found":false}}
```

`body` is the original REST JSON response; a non-JSON response is retained as `{"text":"original response text"}`. HTTP business errors set MCP `isError: true` and preserve the original status, message and `command_outcome` evidence. Authentication, transport or malformed protocol requests can fail at HTTP/JSON-RPC level before the tool runs. Discovery excludes tools outside the role; direct calls still enforce canonical server permissions. MCP annotations describe behavior and never grant permission. Viewers receive read tools; operators perform warehouse work; admins also manage facilities/catalog/counts and requeue; OMS identities retain only their source-linked order/import permissions.

## Physical work and recovery

Read the current work and its version, establish the actual observation, and then submit the reviewed command. A plan, a model prediction or a Freight acknowledgment is not evidence that goods were picked, dispatched, handed to a carrier or received. Preserve the original facility, tool name and entire `request` before sending a write.

After a timeout, disconnect, cancellation, 503 or malformed response, query `wms_command_receipt` with the **warehouse `request_id`**, not the MCP JSON-RPC `id`. A missing receipt does not prove that a transaction never committed. Retain and retry the exact original request through the same command tool and actor. A committed replay returns its original result/version/event, even after later work. A definitive `command_outcome` remains authoritative. Do not replace a pending request ID or refresh its reviewed versions to get past uncertainty. Use `wms_native_receipt` and the original native body for import, transport and conditional fulfillment recovery. [Operations guidance](operations.md) describes unresolved-outcome handling.

Native import/proposal/transport/fulfillment may contact configured external services. Transport records a planned load, while physical dispatch/handoff require their own observed commands. Partial or multiple handoffs remain held by the current single-event OMS receiver contract; MCP cannot bypass that hold or invent a combined event/carrier. No refunds, carrier purchasing or autonomous physical confirmation is added by this endpoint.

`wms_cancel_remainder` closes all unshipped demand on a partly shipped package order after unpacking and actual physical unpick. Use the captured warehouse version and original request journal. It retains original ordered/shipped quantities and return allowance, releases only remaining unpicked reservations, and does not cancel an upstream order or create a refund.

`wms_set_product_expiry_policy`, `wms_record_lot_expiry`, `wms_correct_lot_expiry`, `wms_quarantine_lot` and `wms_release_lot_hold` use the identical [lot-control commands](api.md#lot-expiry-quarantine-and-availability), captured warehouse version and original journal. Operators can record the initial expiry and urgent quarantine; only administrators can change requirements, correct expiry or release an exact hold. No tool chooses the actor/admission clock, clears another hold or bypasses expiry. `wms_lots` accepts exact optional SKU and original cursor pagination. Physical receiving and recovery retain custody while unavailable goods remain held from new fulfillment.

## Ingress configuration

Default allowed hosts are `localhost`, `127.0.0.1` and `[::1]`, on any port. Default allowed origins are empty: a request containing an `Origin` is rejected. CLI/backend clients without an Origin can connect. To serve a configured ingress, set explicit authorities and, only if required, trusted origins:

```dotenv
WMS_MCP_JSON={"allowed_hosts":["warehouse.example.com"],"allowed_origins":["https://agents.example.com"]}
```

Hosts are canonical lowercase `host[:port]` authorities, without schemes, credentials, paths or wildcards. A host without a port permits that host on any port. Origins must be exact canonical HTTPS origins, with HTTP allowed only on loopback. Do not include a trailing slash. Invalid configuration fails startup before database access. Configure your ingress to preserve the allowed Host authority; forwarded host headers do not replace it. Origin validation does not enable general cross-origin browser access or supply CORS headers. Terminate remote TLS at the trusted ingress and retain the usual WMS credential and native-service restrictions.

## Verification scope

The default checks exercise official SDK clients using both protocol generations, authenticated discovery, current API-key and WorkOS membership identity, denied roles, forged arguments, schema references, DNS-rebinding/Origin defenses, body bounds and protocol-header mismatches. The isolated PostgreSQL suite executes MCP partial fulfillment, early return authorization, independently reconciled stock, stale-version rejection and actor/tenant ownership. An actual TCP save discards its response before original-receipt recovery and exact replay. The production-container smoke check verifies authenticated tool discovery and identity against REST in the shipped image. These are synthetic local software checks; deployed client acceptance, real inventory and facility qualification require separate evidence.


`wms_lot_trace` requires exact `sku` and `lot`. Optional top-level `section`, `limit`, `review` and `after` have the identical [lot-trace contract](api.md#cross-facility-lot-trace) and source-check scope as REST. Keep original cursors unchanged, including values longer than resource identifiers. A changed source returns 409 and requires explicit fresh review; the adapter never freshens it on the caller's behalf. The review hash is not a warehouse write version or permission. A separate hold/release requires its own canonical current warehouse review, authorized actor and original durable command journal. Local official-client checks and immutable-container discovery cover 62 administrator tools; deployed client acceptance remains outstanding.


`wms_record_transfer_loss` and `wms_reverse_transfer_loss` use the same [destination administrator commands](api.md#confirmed-transfer-loss-and-reversal) and durable original journal as REST. Operator discovery excludes both actions and direct calls enforce the same permission. `wms_transfer_losses` is an authenticated read with required `warehouse_id` and `transfer_id`; optional `limit`, original `after` and destination `review` retain canonical paging and source flags. A read review is not a write credential. Neither action credits stock or invents arrival; actual located goods still require `wms_receive_transfer`. Preserve the original incident, exact loss identity, quantity and complete request through uncertainty. These loss tools are included in the current administrator catalog described below; deployed MCP-client acceptance remains outstanding.

Pack-aware command tools accept the same optional `request.unit_inputs` as REST. Supply the original definition ID, factor, whole pack count and exact target alongside the matching canonical each quantity. Preserve the entire original envelope after an uncertain save. See [reviewed pack units](api.md#reviewed-pack-units) for supported targets and limits.


The [locally qualified software release](qualification/release-20261019-icommerce-tui-v1/summary.json) serves **70 administrator tools** at the native `/mcp` endpoint, within the same authenticated API process as REST. Serial commands use the same `serial_inputs`, `create_product.serial_controlled`, roles and original-request recovery as the [REST contract](api.md#serial-command-admission). Three viewer-readable tools, `wms_serials`, `wms_serial_history` and `wms_serial_observations`, expose bounded custody, lineage and original label occurrences. Retain the exact SKU, literal serial when required, filters, global tenant/SKU review and cursor. Empty observations remain explicit; source flags describe selected evidence and do not supply physical confirmation. Desktop/mobile workflows and actual populated archives are exercised locally. The two sustained image profiles use REST; deployed MCP-client and physical scanner acceptance remain separate. Preserve complete literal IDs, target groups, actor, facility and original request through uncertain outcomes.
Version 0.2.0 includes read-only `wms_icommerce_preview` and `wms_icommerce_inventory` tools. Their [compatibility boundary](icommerce.md) requires explicit mappings, exact whole each quantities and canonical command execution after review. The earlier 68-tool reports remain scoped to their recorded sources.
