# Ghostty visual operations console

Home shortcut `m` and menu 21 open operations flow, stock by bin, configured floor plans and product label/photo previews. The canonical warehouse snapshot and its existing reconciliation verdict supply all operational values. The server refuses visual evidence while reconciliation is held. These endpoints neither execute commands nor append requests, movements, events or delivery intents. Warehouse work continues through the original command and recovery interfaces.

## Facility assets

Set `WMS_CONSOLE_ASSETS_FILE` on the API server to a trusted administrator-owned JSON file. Restart the API to load a changed configuration. Image content is read per request. Paths are relative to the configuration file's canonical directory, including when the configuration itself is a symlink. Each tenant/facility must be named explicitly; there is no wildcard or default plan/photo. Use real measured bin coordinates and authorized product images. This illustrative configuration contains synthetic identities/geometry:

```json
{
  "schema_version": 1,
  "facilities": [{
    "tenant": "synthetic-tenant",
    "warehouse_id": "wh-main",
    "layout": {
      "name": "Synthetic example only",
      "unit": "m",
      "width": 100,
      "height": 60,
      "bins": [
        {"code": "RECV-01", "x": 0, "y": 0, "width": 20, "height": 20},
        {"code": "STAGE-01", "x": 30, "y": 0, "width": 20, "height": 20}
      ]
    },
    "product_images": {"Exact-Sku": "photos/exact-sku.png"}
  }]
}
```

`layout` can be null and `product_images` can be empty or omitted. Coordinates are nonnegative integers; dimensions are positive integers, bounded to 1,000,000 units. Use `mm`, `cm`, `m` or explicit `grid` units. Rectangles must fit and must not overlap. A plan has at most 1,024 bins; partial mapping is allowed and its coverage is displayed. Any configured bin absent from the current canonical facility makes the plan unavailable (`bin_sources_changed`), without inventing a replacement position.

Configuration is limited to 1 MiB and 1,000 explicit facilities; unknown fields, duplicates and unsafe paths reject the configuration. A malformed configured file holds visual endpoints with 503; other warehouse interfaces retain their normal behavior. Only local relative `.png` paths are accepted. Canonical paths must stay inside the configured directory; URL fetches and parent/absolute paths are unavailable. Administrators must protect that directory from concurrent untrusted modification. Photos must be static PNGs no larger than 4 MiB and 2,048 × 2,048 pixels. Chunk boundaries/CRC and animation are checked; the client additionally bounds decoded scanlines and removes ancillary metadata before terminal transmission. A missing or invalid photo returns metadata with `image: null, image_status: "unavailable"`; original catalog and label evidence remain readable. No image configuration returns `not_configured`.

## Server contract

All routes require the existing bearer authentication and facility/tenant access policy, including viewer access. Response bodies contain `schema_version: 1`, current tenant/facility identity, `demo`, `version` as a decimal string, `captured_at`, and `source_ok: true`. Responses are `no-store`. A failed reconciliation or changed review returns 409. Clients must not substitute a previous tenant or invent quantities after an error.

| GET path | Meaning |
| --- | --- |
| `/v1/warehouses/{warehouse}/console` | Flow, exact each totals, bin page and optional floor plan |
| `/v1/warehouses/{warehouse}/console/products/{sku}` | Exact product catalog, SKU label and optional photo metadata |
| `/v1/warehouses/{warehouse}/console/products/{sku}/image` | Bounded `image/png` bound to the reviewed identity/version/digest |

The console accepts `limit` (1–64, default 32), `after` (exact last bin code), and `review` (original version). `after` requires `review`; clients also retain the original version when navigating back to the first page. Unknown query keys or malformed limits/versions return 422. Bin order follows UTF-8 byte ordering. Empty bins are included. Totals and stage counts are decimal strings to avoid JavaScript integer rounding. Bins expose `code`, `zone`, `kind`, `on_hand_each`, `reserved_each`, and `lot_positions`, with `total` and `next_after`. Floor plans expose configured geometry, revision and coverage, alongside quantities from that same snapshot. They never imply bin capacity, route accessibility, real-time device telemetry or sellable availability.

Flow statuses, in order, are `pending`, `allocated`, `picking`, `picked`, `packed`, and `partially_shipped`. They count original orders at those stages, not throughput rates or invented SLA targets. Stock bars compare physical/reserved each quantities within the displayed bin page.

Product metadata retains original SKU case and name, category, serial/expiry policy and exact reorder point. `label.value` is the exact SKU and `unit` is `each`; `symbology` is `code128` for 1–80 printable ASCII characters, otherwise null. Product labels do not stand in for actually observed serials, shipping labels or GS1 identifiers. Optional `review` fences metadata reads. A photo includes SHA256, width, height and original byte count. To fetch it, supply `review`, `sha256`, and `scope` from `image_scope`. The latter binds tenant, actor, role and warehouse; it is a consistency fence, not a credential. Changing identity, committed warehouse version or image content invalidates the old request. No bearer token appears in metadata or image URLs. Clients authenticate every binary read, refuse redirects and validate bytes, dimensions and SHA256 before rendering.

## Client and acceptance

See [terminal controls](../cli/README.md#graphics-and-visual-operations). Automatic mode requires an affirmative [Kitty capability query](https://sw.kovidgoyal.net/kitty/graphics-protocol/); [Ghostty supports the protocol](https://ghostty.org/docs/features). The client uses direct byte transfers with base64 chunks of at most 4,096 bytes, bounded raster dimensions and pane placement. It deletes only its own image IDs. No server-generated escape sequences, shared-memory/file transfers, terminal configuration changes or new runtime npm dependencies are used.

Complete exact text remains available at compact pane sizes and without graphics. Visual views refresh only on operator request; resize/pan/zoom stay local. A failed read retains the previous evidence marked STALE. Plans/photos are unavailable until configured. Code 128 is a screen preview: independent software decoding checks do not establish physical scanner or printer acceptance. Real Ghostty rendering, fonts/scale, deployed scanners and facility workflows still require witnessed operator acceptance.
