// Actual browser -> authenticated API -> disposable PostgreSQL, no interception.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
test("partial RMA cancellation preserves physical stock and reauthorizes only its unreceived line", async ({
  page,
}) => {
  test.setTimeout(90000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` };
  const suffix = randomUUID().slice(0, 8),
    warehouse = `wh-cancel-${suffix}`,
    reference = `RMA-${suffix}`,
    sku = `CANCEL-${suffix}`;
  const path = `/v1/warehouses/${warehouse}`;
  const get = async (url: string) => {
    const response = await page.request.get(url, { headers });
    expect(response.ok(), await response.text()).toBe(true);
    return response.json();
  };
  const command = async (body: unknown, status = 200) => {
    const state = await get(path);
    const response = await page.request.post(`${path}/commands`, {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: state.version,
        command: body,
      },
    });
    expect(response.status(), await response.text()).toBe(status);
    return response.json();
  };
  const created = await page.request.post(`${path}/commands`, {
    headers,
    data: {
      request_id: randomUUID(),
      expected_version: 0,
      command: {
        type: "create_warehouse",
        code: `CNL-${suffix}`,
        name: "Synthetic cancellation facility",
        city: "Vancouver",
        timezone: "America/Vancouver",
      },
    },
  });
  expect(created.ok(), await created.text()).toBe(true);
  await command({
    type: "create_product",
    sku,
    name: "Cancellation acceptance product",
    category: "Test",
    reorder_point: 0,
  });
  await command({
    type: "create_location",
    code: "BIN",
    zone: "Storage",
    kind: "storage",
  });
  await command({
    type: "count_stock",
    sku,
    lot: "LOT",
    location: "BIN",
    counted: 10,
    reason: "Synthetic opening fixture",
  });
  const order = (
    await command({
      type: "create_order",
      reference: `SOURCE-${suffix}`,
      source_order_id: null,
      customer: "Synthetic customer",
      destination: "Test",
      priority: "normal",
      lines: [
        { id: "a", sku, quantity: 3 },
        { id: "b", sku, quantity: 2 },
      ],
    })
  ).result.entity_id;
  await command({ type: "allocate", order_id: order });
  for (const [line, quantity] of [
    ["a", 3],
    ["b", 2],
  ])
    await command({
      type: "pick",
      order_id: order,
      line_id: line,
      quantity,
      location: "BIN",
      lot: "LOT",
    });
  await command({ type: "pack", order_id: order });
  await command({
    type: "ship",
    order_id: order,
    carrier: "Carrier",
    handoff_reference: `HANDOFF-${suffix}`,
  });
  const receipt = (
    await command({
      type: "create_return",
      reference,
      order_id: order,
      reason: "Customer authorized a physical return",
      lines: [
        { id: "ra", order_line_id: "a", lot: "LOT", quantity: 3 },
        { id: "rb", order_line_id: "b", lot: "LOT", quantity: 2 },
      ],
    })
  ).result.entity_id;
  for (const [line, quantity] of [
    ["ra", 1],
    ["rb", 2],
  ])
    await command({
      type: "receive",
      receipt_id: receipt,
      line_id: line,
      quantity,
      location: "RECV-01",
    });
  const before = await get(path);
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(token);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Change warehouse", exact: true })
    .click();
  await page
    .getByLabel("Active warehouse", { exact: true })
    .selectOption(warehouse);
  await page.getByRole("button", { name: "Receiving", exact: true }).click();
  await page
    .getByRole("row")
    .filter({ hasText: reference })
    .getByRole("button", { name: "Receive goods", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Cancel unreceived remainder", exact: true })
    .click();
  await page
    .getByLabel("Cancellation reason", { exact: true })
    .fill("Customer kept the remaining two units");
  const pending = page.waitForRequest(
    (r) =>
      r.method() === "POST" &&
      r.url().endsWith("/commands") &&
      r.postDataJSON()?.command?.type === "cancel_receipt",
  );
  await page
    .getByRole("button", {
      name: "Confirm remainder cancellation",
      exact: true,
    })
    .click();
  const original = await pending;
  const originalResponse = await original.response();
  expect(originalResponse?.ok()).toBe(true);
  const originalResult = await originalResponse!.json();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const after = await get(path);
  expect(after.stocks).toEqual(before.stocks);
  expect(after.movements).toEqual(before.movements);
  expect(after.orders).toEqual(before.orders);
  const detail = await get(`${path}/receipts/${receipt}`);
  expect(detail.cancellation.lines.ra.cancelled).toBe(2);
  expect(detail.cancellation.lines.rb.cancelled).toBe(0);
  expect(detail.lines.map((l: any) => l.received)).toEqual([1, 2]);
  await page
    .getByRole("row")
    .filter({ hasText: reference })
    .getByRole("button", { name: "View receipt", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toContainText(
    "Customer kept the remaining two units",
  );
  await expect(
    page
      .getByRole("dialog")
      .getByRole("button", { name: "Receive goods", exact: true }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  await page.getByRole("button", { name: "Scanner", exact: true }).click();
  await page.getByLabel("Receipt reference", { exact: true }).fill(reference);
  await page.getByRole("button", { name: "Load work", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText(
    "receipt remainder was cancelled",
  );
  const allowance = await get(`${path}/orders/${order}/return-eligibility`);
  expect(
    allowance.lines.find((l: any) => l.order_line_id === "a").remaining,
  ).toBe(2);
  expect(
    allowance.lines.find((l: any) => l.order_line_id === "b").remaining,
  ).toBe(0);
  await command(
    {
      type: "receive",
      receipt_id: receipt,
      line_id: "ra",
      quantity: 1,
      location: "RECV-01",
    },
    422,
  );
  await command(
    {
      type: "create_return",
      reference: `OVER-${suffix}`,
      order_id: order,
      reason: "New request",
      lines: [{ id: "new", order_line_id: "a", lot: "LOT", quantity: 3 }],
    },
    422,
  );
  await command({
    type: "create_return",
    reference: `NEW-${suffix}`,
    order_id: order,
    reason: "New authorization of the retained units",
    lines: [{ id: "new", order_line_id: "a", lot: "LOT", quantity: 2 }],
  });
  const replay = await page.request.post(`${path}/commands`, {
    headers,
    data: original.postDataJSON(),
  });
  expect(replay.ok(), await replay.text()).toBe(true);
  const recovered = await replay.json();
  expect(recovered.replayed).toBe(true);
  expect(recovered.result).toEqual(originalResult.result);
  expect((await get(path)).stocks).toEqual(before.stocks);
  expect((await get(`${path}/reconciliation`)).ok).toBe(true);
});
