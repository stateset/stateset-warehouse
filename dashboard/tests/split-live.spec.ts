// Actual built browser → API → PostgreSQL; no HTTP interception.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
test("partial shipments allocate real backorders and preserve each handoff, early return and original request", async ({
  page,
}) => {
  test.setTimeout(120000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` };
  const suffix = randomUUID().slice(0, 8),
    sku = `SPLIT-${suffix}`,
    bin = `PART-${suffix}`,
    lot = "BACKORDER-LOT",
    reference = `PARTIAL-${suffix}`;
  const get = async (path: string) => {
    const r = await page.request.get(path, { headers });
    expect(r.ok(), await r.text()).toBe(true);
    const body = await r.json();
    if (/^\/v1\/warehouses\/[^/]+$/.test(path)) {
      // This is a fresh read observation, not persisted warehouse evidence.
      const { availability_checked_at, ...persisted } = body;
      expect(availability_checked_at).toMatch(
        /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6}Z$/,
      );
      expect(Number.isFinite(Date.parse(availability_checked_at))).toBe(true);
      return persisted;
    }
    return body;
  };
  const state = () => get("/v1/warehouses/wh-main");
  const command = async (command: unknown) => {
    const before = await state();
    const r = await page.request.post("/v1/warehouses/wh-main/commands", {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: before.version,
        command,
      },
    });
    expect(r.ok(), await r.text()).toBe(true);
    return r.json();
  };
  await command({
    type: "create_product",
    sku,
    name: "Synthetic partial shipment",
    category: "General",
    reorder_point: 0,
  });
  await command({
    type: "create_location",
    code: bin,
    zone: "Storage",
    kind: "storage",
  });
  await command({
    type: "count_stock",
    sku,
    lot,
    location: bin,
    counted: 7,
    reason: "Witnessed synthetic opening count",
  });
  const created = await command({
    type: "create_order",
    reference,
    source_order_id: null,
    customer: "Partial shipment customer",
    destination: "Verified customer destination",
    priority: "normal",
    lines: [
      { id: "line-a", sku, quantity: 3 },
      { id: "line-b", sku, quantity: 5 },
    ],
  });
  const oid = created.result.entity_id;
  const packageView = () =>
    get(`/v1/warehouses/wh-main/orders/${oid}/shipments`);
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(token);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  const open = async () => {
    await page.getByRole("button", { name: reference, exact: true }).click();
    await expect(
      page.getByRole("heading", {
        name: "Shipment packages and backorders",
        exact: true,
      }),
    ).toBeVisible();
  };
  const clickCommand = async (type: string, name: string) => {
    const sent = page.waitForRequest(
      (r) =>
        r.method() === "POST" &&
        r.url().endsWith("/commands") &&
        r.postDataJSON()?.command?.type === type,
    );
    await page.getByRole("button", { name, exact: true }).click();
    const request = await sent;
    const response = await request.response();
    expect(response?.ok(), await response?.text()).toBe(true);
    const result = await response!.json();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    return { request, result };
  };
  await open();
  const allocated = await clickCommand(
    "allocate_available",
    "Allocate available stock",
  );
  let view = await packageView();
  expect(view.progress.map((l: any) => l.unallocated)).toEqual([0, 1]);
  expect(view.order.shipment_mode).toBe("packages");
  const pick = async (line: string, q: number) => {
    await open();
    await page
      .locator(".allocation")
      .filter({ hasText: `line ${line}` })
      .getByRole("button", { name: "Pick units", exact: true })
      .click();
    await page
      .getByLabel("Picked quantity (each)", { exact: true })
      .fill(String(q));
    await clickCommand("pick", "Confirm pick");
  };
  const pack = async (line: string, q: number, pref: string) => {
    await open();
    await page.getByLabel("Package reference", { exact: true }).fill(pref);
    await page
      .getByLabel(`Package each units ${line} ${bin} ${lot}`, { exact: true })
      .fill(String(q));
    return clickCommand("pack_shipment", "Confirm packed shipment");
  };
  const handoff = async (pref: string, carrier: string) => {
    await open();
    await page
      .getByRole("button", { name: `Hand off ${pref}`, exact: true })
      .click();
    await page.getByLabel("Shipment carrier", { exact: true }).fill(carrier);
    await page
      .getByLabel("Shipment handoff reference", { exact: true })
      .fill(`${pref}-HANDOFF`);
    await page
      .getByLabel("Carrier has physically received this exact package", {
        exact: true,
      })
      .check();
    return clickCommand("handoff_shipment", "Record shipment handoff");
  };
  await pick("line-a", 3);
  const first = await pack("line-a", 3, `PACKAGE-A-${suffix}`);
  const handed = await handoff(`PACKAGE-A-${suffix}`, "Carrier-A");
  view = await packageView();
  expect(view.order.status).toBe("partially_shipped");
  expect(view.packages[0].closure.carrier).toBe("Carrier-A");
  expect(view.progress.map((l: any) => l.shipped)).toEqual([3, 0]);
  await open();
  await page
    .getByRole("button", { name: "Review return allowance", exact: true })
    .click();
  await expect(
    page.getByText(
      "Allowance checked against the recorded shipment handoffs. Unshipped units are excluded.",
      { exact: true },
    ),
  ).toBeVisible();
  await page
    .getByLabel("RMA reference", { exact: true })
    .fill(`EARLY-RMA-${suffix}`);
  await page
    .getByLabel("Return reason / inspection evidence", { exact: true })
    .fill("One unit returned while the other line remains backordered");
  await clickCommand("create_return", "Authorize return receipt");
  await command({
    type: "count_stock",
    sku,
    lot,
    location: bin,
    counted: 5,
    reason: "Witnessed one additional backordered unit at storage",
  });
  await open();
  await expect(
    page.getByRole("button", { name: "Allocate available stock", exact: true }),
  ).toBeDisabled();
  await page
    .getByRole("button", { name: "Refresh shipment review", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Allocate available stock", exact: true }),
  ).toBeEnabled();
  await clickCommand("allocate_available", "Allocate available stock");
  await pick("line-b", 5);
  await pack("line-b", 5, `PACKAGE-B-${suffix}`);
  await handoff(`PACKAGE-B-${suffix}`, "Carrier-B");
  view = await packageView();
  expect(view.ok).toBe(true);
  expect(view.order.status).toBe("shipped");
  expect(view.order.carrier).toBe("");
  expect(view.order.handoff_reference).toBe("");
  expect(view.packages.map((p: any) => p.closure.carrier)).toEqual([
    "Carrier-A",
    "Carrier-B",
  ]);
  expect(view.progress.map((l: any) => l.unallocated)).toEqual([0, 0]);
  const proof = await get("/v1/warehouses/wh-main/reconciliation");
  expect(proof.ok).toBe(true);
  expect(
    proof.packages.find((p: any) => p.order_id === oid).packages,
  ).toHaveLength(2);
  const allowance = await get(
    `/v1/warehouses/wh-main/orders/${oid}/return-eligibility`,
  );
  expect(allowance.lines.map((l: any) => l.remaining)).toEqual([2, 5]);
  expect(allowance.shipment_source.event_ids).toEqual(
    view.packages.map((p: any) => p.closure.event_id),
  );
  const held = await page.request.post(
    `/v1/warehouses/wh-main/orders/${oid}/oms-fulfillment`,
    {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: (await state()).version,
      },
    },
  );
  expect(held.status()).toBe(409);
  expect((await held.json()).error).toContain("multiple shipment handoffs");
  const before = await state();
  for (const original of [allocated, first, handed]) {
    const replay = await page.request.post(original.request.url(), {
      headers,
      data: original.request.postDataJSON(),
    });
    expect(replay.ok()).toBe(true);
    expect(await replay.json()).toEqual({
      replayed: true,
      result: original.result.result,
    });
  }
  expect(await state()).toEqual(before);
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  const exported = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export CSV", exact: true }).click();
  const csv = readFileSync((await (await exported).path())!, "utf8");
  for (const field of [
    "package_id",
    "order_line_id",
    "allocation_location",
    "shipment_event_id",
  ])
    expect(csv.split("\r\n")[0]).toContain(`"${field}"`);
  for (const p of view.packages) {
    expect(csv).toContain(`"${p.id}"`);
    expect(csv).toContain(`"${p.closure.event_id}"`);
  }
  expect(errors).toEqual([]);
});

test("partly shipped remainder closure requires physical recovery and retains original demand, handoff, return allowance and replay", async ({
  page,
}) => {
  test.setTimeout(120000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` },
    suffix = randomUUID().slice(0, 8),
    sku = `CLOSE-${suffix}`,
    bin = `CLOSE-${suffix}`,
    lot = "CLOSED-LOT",
    reference = `CLOSED-${suffix}`;
  const get = async (path: string) => {
    const response = await page.request.get(path, { headers });
    expect(response.ok(), await response.text()).toBe(true);
    const body = await response.json();
    if (/^\/v1\/warehouses\/[^/]+$/.test(path)) {
      // This is a fresh read observation, not persisted warehouse evidence.
      const { availability_checked_at, ...persisted } = body;
      expect(availability_checked_at).toMatch(
        /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6}Z$/,
      );
      expect(Number.isFinite(Date.parse(availability_checked_at))).toBe(true);
      return persisted;
    }
    return body;
  };
  const state = () => get("/v1/warehouses/wh-main");
  const command = async (command: unknown) => {
    const response = await page.request.post(
      "/v1/warehouses/wh-main/commands",
      {
        headers,
        data: {
          request_id: randomUUID(),
          expected_version: (await state()).version,
          command,
        },
      },
    );
    expect(response.ok(), await response.text()).toBe(true);
    return response.json();
  };
  await command({
    type: "create_product",
    sku,
    name: "Synthetic cancelled remainder",
    category: "Test",
    reorder_point: 0,
  });
  await command({
    type: "create_location",
    code: bin,
    zone: "Storage",
    kind: "storage",
  });
  await command({
    type: "count_stock",
    sku,
    lot,
    location: bin,
    counted: 7,
    reason: "Witnessed synthetic opening count",
  });
  const created = await command({
    type: "create_order",
    reference,
    source_order_id: null,
    customer: "Synthetic cancellation customer",
    destination: "Verified destination",
    priority: "normal",
    lines: [
      { id: "line-a", sku, quantity: 3 },
      { id: "line-b", sku, quantity: 5 },
    ],
  });
  const oid = created.result.entity_id;
  await command({ type: "allocate_available", order_id: oid });
  await command({
    type: "pick",
    order_id: oid,
    line_id: "line-a",
    location: bin,
    lot,
    quantity: 3,
  });
  const sent = await command({
    type: "pack_shipment",
    order_id: oid,
    reference: `SENT-${suffix}`,
    lines: [{ line_id: "line-a", location: bin, lot, quantity: 3 }],
  });
  await command({
    type: "handoff_shipment",
    shipment_id: sent.result.entity_id,
    carrier: "Observed original carrier",
    handoff_reference: `ACTUAL-${suffix}`,
  });
  await command({
    type: "pick",
    order_id: oid,
    line_id: "line-b",
    location: bin,
    lot,
    quantity: 1,
  });
  await command({
    type: "pack_shipment",
    order_id: oid,
    reference: `STAGED-${suffix}`,
    lines: [{ line_id: "line-b", location: bin, lot, quantity: 1 }],
  });
  const review = () => get(`/v1/warehouses/wh-main/orders/${oid}/shipments`);
  const original = (await review()).order;
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(token);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  const open = async () => {
    await page.getByRole("button", { name: reference, exact: true }).click();
    await expect(
      page.getByRole("heading", {
        name: "Cancel the unshipped remainder",
        exact: true,
      }),
    ).toBeVisible();
  };
  const clickCommand = async (type: string, label: string) => {
    const pending = page.waitForRequest(
      (request) =>
        request.method() === "POST" &&
        request.url().endsWith("/commands") &&
        request.postDataJSON()?.command?.type === type,
    );
    await page.getByRole("button", { name: label, exact: true }).click();
    const request = await pending,
      response = await request.response();
    expect(response?.ok(), await response?.text()).toBe(true);
    const result = await response!.json();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    return { request, result };
  };
  await open();
  await expect(
    page.getByRole("button", {
      name: "Cancel unshipped remainder",
      exact: true,
    }),
  ).toBeDisabled();
  await expect(
    page.getByLabel(
      "I confirmed cancellation of all remaining original demand.",
      { exact: true },
    ),
  ).toBeDisabled();
  await page
    .getByRole("button", { name: `Unpack STAGED-${suffix}`, exact: true })
    .click();
  await page
    .getByLabel("Unpacking evidence / reason", { exact: true })
    .fill("Customer cancelled this unshipped package");
  await clickCommand("unpack_shipment", "Confirm unpacking");
  await open();
  await expect(
    page.getByRole("button", {
      name: "Cancel unshipped remainder",
      exact: true,
    }),
  ).toBeDisabled();
  await page
    .locator(".allocation")
    .filter({ hasText: "line line-b" })
    .getByRole("button", { name: "Return staged units", exact: true })
    .click();
  await page.getByLabel("Returned quantity (each)", { exact: true }).fill("1");
  await page
    .getByLabel("Return evidence / reason", { exact: true })
    .fill("Physically returned the original staged unit to its storage bin");
  await clickCommand("unpick", "Confirm physical return");
  await open();
  const reviewed = await review();
  await page
    .getByLabel("Remainder cancellation reason", { exact: true })
    .fill("Customer cancelled all five remaining original units");
  await page
    .getByLabel("I confirmed cancellation of all remaining original demand.", {
      exact: true,
    })
    .check();
  const cancelled = await clickCommand(
    "cancel_remainder",
    "Cancel unshipped remainder",
  );
  expect(cancelled.request.postDataJSON().expected_version).toBe(
    reviewed.version,
  );
  expect(cancelled.request.postDataJSON().resource_versions).toBeUndefined();
  const closed = await review();
  expect(closed.ok).toBe(true);
  expect(closed.order.status).toBe("closed_partial");
  expect(closed.order.lines).toEqual(original.lines);
  expect(closed.order.shipped_at).toBe(original.shipped_at);
  expect(closed.order.carrier).toBe(original.carrier);
  expect(closed.order.handoff_reference).toBe(original.handoff_reference);
  expect(closed.progress.map((l: any) => l.cancelled)).toEqual([0, 5]);
  expect(closed.progress.map((l: any) => l.unallocated)).toEqual([0, 0]);
  const stock = (await state()).stocks.filter((b: any) => b.sku === sku);
  expect(stock.reduce((n: number, b: any) => n + b.on_hand, 0)).toBe(4);
  expect(stock.reduce((n: number, b: any) => n + b.reserved, 0)).toBe(0);
  await page.getByRole("button", { name: reference, exact: true }).click();
  await expect(
    page.getByText("Unshipped remainder cancelled", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", {
      name: "Cancel unshipped remainder",
      exact: true,
    }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Review return allowance", exact: true })
    .click();
  await page
    .getByLabel("RMA reference", { exact: true })
    .fill(`CLOSED-RMA-${suffix}`);
  await page
    .getByLabel("Return reason / inspection evidence", { exact: true })
    .fill("One original handed-off unit returned after remainder cancellation");
  await clickCommand("create_return", "Authorize return receipt");
  const allowance = await get(
    `/v1/warehouses/wh-main/orders/${oid}/return-eligibility`,
  );
  expect(allowance.lines).toHaveLength(1);
  expect(allowance.lines[0].shipped).toBe(3);
  expect(allowance.lines[0].remaining).toBe(2);
  const before = await state();
  const replay = await page.request.post(cancelled.request.url(), {
    headers,
    data: cancelled.request.postDataJSON(),
  });
  expect(await replay.json()).toEqual({
    replayed: true,
    result: cancelled.result.result,
  });
  expect(await state()).toEqual(before);
  const proof = await get("/v1/warehouses/wh-main/reconciliation");
  expect(proof.ok).toBe(true);
  const held = await page.request.post(
    `/v1/warehouses/wh-main/orders/${oid}/oms-fulfillment`,
    {
      headers,
      data: { request_id: randomUUID(), expected_version: before.version },
    },
  );
  expect(held.status()).toBe(409);
  expect(errors).toEqual([]);
});
