# iCommerce interoperability

Version 0.2.0 of the WMS directly uses **stateset-core and stateset-primitives 1.39.0**. `stateset_core::MoveInventory`, `CreateInventoryItem`, validation helpers, `StockLevel` and `LocationStock`, plus `stateset_primitives::Sku`, are compiled into the API. Core also brings its upstream crypto dependency. These are real crate dependencies, pinned to the public revision recorded in [UPSTREAM.json](../vendor/stateset-icommerce/UPSTREAM.json).

Builds use the checked-in, verified subset under `vendor/stateset-icommerce`. They do not require `/home/dom/stateset-icommerce`, a private checkout or a moving Git branch. The WMS keeps its own PostgreSQL schema, physical ledger and sole canonical command writer. No iCommerce database engine or second inventory writer is introduced.

## Preview a typed move

`POST /v1/warehouses/{warehouse}/icommerce/preview` requires authentication and the role required by the resulting command. It validates a typed input against current canonical state and returns `committed:false`, `reviewed_version`, `command`, serial-observation requirements, the normalized `upstream_input`, and pinned upstream revision. It creates no command receipt, event, movement or outbox entry.

```json
{
  "kind": "move_inventory",
  "input": {
    "sku": "PART-1",
    "from_location_id": 7,
    "to_location_id": 9,
    "quantity": "2.000",
    "lot_id": null
  },
  "mappings": {"locations": {"7": "RECV-01", "9": "A-01"}}
}
```

Numeric location IDs require explicit mappings to exact WMS bin labels. Optional upstream lot UUIDs require explicit `mappings.lots` entries; an absent lot maps to `UNTRACKED`. Mapping aliases, duplicate IDs, duplicate target labels, surrounding whitespace and noncanonical numeric/UUID mapping keys are rejected. SKU construction must preserve the exact original string and pass upstream validation. This narrower compatibility boundary does not change the WMS's existing literal SKU rules.

Quantities must be whole each units from 1 to 1,000,000. Decimal fractions, overflow and nonzero unsupported quantities fail without rounding. Decimal inputs must be strings, including zero and whole values. The adapter checks the original string with exact parsing before the upstream decoder, which can otherwise round beyond its precision limit. Unsupported precision, fractions, numeric JSON and overflow are rejected without conversion. Upstream `reason` and `performed_by` are unsupported for moves; authenticated WMS identity owns execution. Unknown input fields are rejected.

For `kind:"create_inventory_item"`, supply an upstream `input` and WMS `category`. The adapter uses upstream product validation, permits `EA`/`each`, and maps an optional whole reorder point. Nonzero initial stock and safety stock, descriptions, location assignment and other units are rejected because they have no equivalent product command. Receiving or an observed count establishes stock separately. This product preview creates an untracked product; use the explicit canonical WMS form to configure serial control.

Review the returned command, collect any required actual serial observations, and preserve a **complete original WMS request** before submitting it to `/commands`. A preview does not reserve a version or authorize a save. Preserve that full canonical request through uncertainty; never regenerate it by translating against changed mappings. The [warehouse TUI](../cli/README.md) follows this flow.

## Read inventory projections

`POST /v1/warehouses/{warehouse}/icommerce/inventory` accepts a mappings object and returns actual upstream stock types. Every stock location needs a positive, explicit numeric mapping. Original `source_positions` and `lot_controls` remain included, with warehouse version and pinned upstream revision. The request is read-only and available to viewers.

`total_available` means physical on-hand minus reserved. It does **not** establish eligibility under expiry, quarantine or fulfillment rules; retain original lot controls and use canonical WMS allocation. Inventory projections never fabricate lot UUIDs or change original balances.

MCP exposes `wms_icommerce_preview` and `wms_icommerce_inventory` with the same authenticated handlers. The terminal offers both under iCommerce interop. Neither adds a second execution path.
