// Actual built browser -> authenticated API -> PostgreSQL. No interception.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
test("lot trace reviews actual cross-facility restrictions, prior handoff, staged package and remaining transit without changing custody", async ({
  page,
}, testInfo) => {
  test.setTimeout(120000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` },
    suffix = randomUUID().slice(0, 8),
    sku = `TRACE-${suffix}`,
    lot = "TRACE-LOT",
    destination = `trace-${suffix}`;
  const get = async (path: string) => {
    const r = await page.request.get(path, { headers });
    expect(r.ok(), await r.text()).toBe(true);
    return r.json();
  };
  const command = async (warehouse: string, command: unknown) => {
    const state = await get(`/v1/warehouses/${warehouse}`);
    const r = await page.request.post(`/v1/warehouses/${warehouse}/commands`, {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: state.version,
        command,
      },
    });
    expect(r.ok(), await r.text()).toBe(true);
    return (await r.json()).result;
  };
  const create = await page.request.post(
    `/v1/warehouses/${destination}/commands`,
    {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: 0,
        command: {
          type: "create_warehouse",
          code: `TRACE-${suffix}`,
          name: "Synthetic trace destination",
          city: "Vancouver",
          timezone: "America/Vancouver",
        },
      },
    },
  );
  expect(create.ok(), await create.text()).toBe(true);
  for (const w of ["wh-main", destination])
    await command(w, {
      type: "create_product",
      sku,
      name: "Synthetic trace product",
      category: "Test",
      reorder_point: 0,
    });
  await command("wh-main", {
    type: "create_location",
    code: `TRACE-${suffix}`,
    zone: "Trace storage",
    kind: "storage",
  });
  await command("wh-main", {
    type: "count_stock",
    sku,
    lot,
    location: `TRACE-${suffix}`,
    counted: 10,
    reason: "Witnessed synthetic trace custody",
  });
  await command("wh-main", {
    type: "record_lot_expiry",
    sku,
    lot,
    expires_at: "2099-10-22T00:00:00-07:00",
    reason: "Read original source label",
  });
  const transfer = await command("wh-main", {
    type: "dispatch_transfer",
    reference: `TRACE-MOVE-${suffix}`,
    destination_warehouse_id: destination,
    carrier: "Trace transfer carrier",
    handoff_reference: `TRACE-TRANSFER-HANDOFF-${suffix}`,
    lines: [
      {
        id: "transfer-line",
        sku,
        lot,
        location: `TRACE-${suffix}`,
        quantity: 3,
      },
    ],
  });
  const order = await command("wh-main", {
    type: "create_order",
    reference: `TRACE-ORDER-${suffix}`,
    source_order_id: null,
    customer: "Synthetic trace customer",
    destination: "Observed destination",
    priority: "normal",
    lines: [{ id: "order-line", sku, quantity: 4 }],
  });
  await command("wh-main", {
    type: "allocate_available",
    order_id: order.entity_id,
  });
  await command("wh-main", {
    type: "pick",
    order_id: order.entity_id,
    line_id: "order-line",
    location: `TRACE-${suffix}`,
    lot,
    quantity: 2,
  });
  const pack = async (reference: string) =>
    command("wh-main", {
      type: "pack_shipment",
      order_id: order.entity_id,
      reference,
      lines: [
        {
          line_id: "order-line",
          location: `TRACE-${suffix}`,
          lot,
          quantity: 1,
        },
      ],
    });
  const first = await pack(`TRACE-HANDED-${suffix}`);
  await command("wh-main", {
    type: "handoff_shipment",
    shipment_id: first.entity_id,
    carrier: "Actual trace customer carrier",
    handoff_reference: `TRACE-CUSTOMER-HANDOFF-${suffix}`,
  });
  await pack(`TRACE-PACKED-${suffix}`);
  await command("wh-main", {
    type: "create_return",
    order_id: order.entity_id,
    reference: `TRACE-RMA-${suffix}`,
    reason: "One actual earlier handed-off unit",
    lines: [
      { id: "return-line", order_line_id: "order-line", lot, quantity: 1 },
    ],
  });
  await command("wh-main", {
    type: "correct_lot_expiry",
    sku,
    lot,
    expires_at: "2000-01-01T00:00:00Z",
    reason: "Supervisor corrected original label",
  });
  await command("wh-main", {
    type: "quarantine_lot",
    sku,
    lot,
    reason: "Trace actual staged and transferred stock",
  });
  await command(destination, {
    type: "receive_transfer",
    transfer_id: transfer.entity_id,
    line_id: "transfer-line",
    quantity: 1,
    location: "RECV-01",
    arrival_reference: "Observed one original arrived each",
  });
  const beforeSource = await get("/v1/warehouses/wh-main");
  const beforeDestination = await get(`/v1/warehouses/${destination}`);
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(token);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  const panel = page.locator("details").filter({
    has: page.locator("summary", {
      hasText: "Trace a lot across facilities",
    }),
  });
  await panel.locator("summary").click();
  await panel.getByLabel("Trace SKU", { exact: true }).fill(sku);
  await panel.getByLabel("Trace lot", { exact: true }).fill(lot);
  await panel
    .getByRole("button", { name: "Review lot trace", exact: true })
    .click();
  await expect(panel).toContainText(
    "Physical 7 · Reserved 3 · Usable storage 0 · Receiving 1 · Staged 1",
  );
  await expect(
    panel.getByRole("table", { name: "Lot trace records" }),
  ).toContainText(destination);
  await panel
    .getByRole("combobox", { name: "Trace records", exact: true })
    .selectOption("transfers");
  await expect(
    panel.getByRole("table", { name: "Lot trace records" }),
  ).toContainText("3 / 1 / 0 / 2");
  await panel
    .getByRole("combobox", { name: "Trace records", exact: true })
    .selectOption("shipments");
  await expect(
    panel.getByText("Recorded carrier handoff", { exact: true }),
  ).toBeVisible();
  await expect(
    panel.getByText("Active packed package", { exact: true }),
  ).toBeVisible();
  const download = page.waitForEvent("download");
  await panel
    .getByRole("button", { name: "Export loaded trace records", exact: true })
    .click();
  const report = JSON.parse(
    readFileSync((await (await download).path())!, "utf8"),
  );
  expect(report.sku).toBe(sku);
  expect(report.lot).toBe(lot);
  expect(report.section_complete).toBe(true);
  expect(report.global_trace_complete).toBe(false);
  expect(report.items).toHaveLength(2);
  expect(report.items.every((r: any) => r.source_ok)).toBe(true);
  expect((await get("/v1/warehouses/wh-main")).version).toBe(
    beforeSource.version,
  );
  expect((await get(`/v1/warehouses/${destination}`)).version).toBe(
    beforeDestination.version,
  );
  await page.screenshot({
    path: testInfo.outputPath("lot-trace-desktop.png"),
    fullPage: true,
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await expect
    .poll(() => page.evaluate(() => document.documentElement.scrollWidth))
    .toBeLessThanOrEqual(390);
  await page.screenshot({
    path: testInfo.outputPath("lot-trace-mobile.png"),
    fullPage: true,
  });
  await command(destination, {
    type: "quarantine_lot",
    sku,
    lot,
    reason: "A second investigation changes the original trace review",
  });
  await panel
    .getByRole("combobox", { name: "Trace records", exact: true })
    .selectOption("movements");
  await expect(panel.getByRole("alert")).toContainText(
    "Old pages have been discarded",
  );
  await expect(
    panel.getByRole("table", { name: "Lot trace records" }),
  ).toHaveCount(0);
  await panel
    .getByRole("button", { name: "Review lot trace", exact: true })
    .click();
  await expect(
    panel.getByRole("table", { name: "Lot trace records" }),
  ).toBeVisible();
  const source = await get("/v1/warehouses/wh-main"),
    dest = await get(`/v1/warehouses/${destination}`);
  expect(
    source.stocks
      .filter((s: any) => s.sku === sku)
      .reduce((n: number, s: any) => n + s.on_hand, 0),
  ).toBe(6);
  expect(
    dest.stocks
      .filter((s: any) => s.sku === sku)
      .reduce((n: number, s: any) => n + s.on_hand, 0),
  ).toBe(1);
  expect((await get("/v1/warehouses/wh-main/reconciliation")).ok).toBe(true);
  expect((await get(`/v1/warehouses/${destination}/reconciliation`)).ok).toBe(
    true,
  );
  expect(errors).toEqual([]);
});
