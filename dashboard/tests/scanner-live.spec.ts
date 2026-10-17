// Real browser → HTTP → SQLx → PostgreSQL qualification. Keyboard-wedge input
// is simulated; scanner hardware and printed labels require facility acceptance.
import { expect, test } from "@playwright/test";
import { randomUUID } from "node:crypto";

test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});

test("scanner receiving and exact-line picking survive a concurrent operator count through physical handoff", async ({
  page,
}) => {
  test.setTimeout(120000);
  page.setDefaultTimeout(10000);
  await page.setViewportSize({ width: 390, height: 844 });
  const suffix = `${Date.now()}-${randomUUID().slice(0, 8)}`;
  const sku = `SCAN-LIVE-${suffix}`;
  const lot = `SCAN-LOT-${suffix}`;
  const receiptReference = `SCAN-RCV-${suffix}`;
  const orderReference = `SCAN-SO-${suffix}`;
  const firstLine = `scan-line-a-${suffix}`;
  const secondLine = `scan-line-b-${suffix}`;
  const receiving = "RECV-01";
  const storage = `SCAN-BIN-${suffix}`;
  const root = "/v1/warehouses/wh-main";
  const operatorToken =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${operatorToken}` };
  const counterHeaders = {
    Authorization: `Bearer ${process.env.WMS_SCANNER_COUNTER_TOKEN || "scanner-live-counter-credential-0001"}`,
  };
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  const read = async (path = root, auth = headers): Promise<any> => {
    const response = await page.request.get(path, { headers: auth });
    expect(response.ok(), `GET ${path}: ${await response.text()}`).toBe(true);
    return response.json();
  };
  const command = async (value: unknown, auth = headers): Promise<any> => {
    const current = await read(root, auth);
    const response = await page.request.post(`${root}/commands`, {
      headers: auth,
      data: {
        request_id: randomUUID(),
        expected_version: current.version,
        command: value,
      },
    });
    expect(response.ok(), `POST command: ${await response.text()}`).toBe(true);
    return response.json();
  };
  const operator = await read("/v1/session");
  const counter = await read("/v1/session", counterHeaders);
  expect(operator.demo).toBe(false);
  expect(counter.demo).toBe(false);
  expect(operator.identity.tenant).toBe("scanner-qualification");
  expect(counter.identity.tenant).toBe(operator.identity.tenant);
  expect(counter.identity.actor).not.toBe(operator.identity.actor);
  expect(["operator", "admin"]).toContain(counter.identity.role);
  const navigate = async (label: string) => {
    await page.getByRole("button", { name: label, exact: true }).click();
  };
  const scan = async (label: string, value: string) => {
    const input = page.getByRole("textbox", {
      name: `Scan ${label}`,
      exact: true,
    });
    await input.fill(value);
    await input.press("Enter");
  };
  const verify = async (bin: string) => {
    await scan("bin", bin);
    await scan("SKU", sku);
    await scan("lot", lot);
  };
  const confirm = async (label: string, quantity: number) => {
    await page
      .getByLabel("Physical quantity (each)", { exact: true })
      .fill(String(quantity));
    const response = page.waitForResponse(
      (response) =>
        response.url().endsWith(`${root}/commands`) &&
        response.request().method() === "POST",
    );
    await page.getByRole("button", { name: label, exact: true }).click();
    expect((await response).ok()).toBe(true);
    await expect(page.getByText(/Save confirmed\. Reload work/)).toBeVisible();
  };
  const loadReceipt = async (line: string) => {
    await page
      .getByLabel("Receipt reference", { exact: true })
      .fill(receiptReference);
    await page.getByRole("button", { name: "Load work", exact: true }).click();
    await page
      .getByRole("combobox", { name: "Receipt line", exact: true })
      .selectOption(line);
    await expect(
      page.getByRole("textbox", { name: "Scan bin", exact: true }),
    ).toBeEnabled();
  };
  const allocationKey = (line: string) => JSON.stringify([line, storage, lot]);
  const loadPick = async (line: string, reload = false) => {
    if (!reload)
      await page
        .getByLabel("Order reference", { exact: true })
        .fill(orderReference);
    await page
      .getByRole("button", {
        name: reload ? "Reload work" : "Load work",
        exact: true,
      })
      .click();
    await page
      .getByRole("combobox", { name: "Reserved allocation", exact: true })
      .selectOption(allocationKey(line));
    await expect(
      page.getByRole("textbox", { name: "Scan bin", exact: true }),
    ).toBeEnabled();
  };
  const openOrder = async () => {
    await navigate("Fulfillment");
    await page
      .getByRole("button", { name: orderReference, exact: true })
      .click();
    await expect(page.getByRole("dialog")).toContainText(orderReference);
  };
  const saved = async () => expect(page.getByRole("dialog")).toHaveCount(0);

  await command({
    type: "create_product",
    sku,
    name: "Live scanner qualification product",
    category: "Scanner qualification",
    reorder_point: 0,
  });
  await command({
    type: "create_location",
    code: storage,
    zone: "Scanner qualification",
    kind: "storage",
  });
  const receipt = await command({
    type: "create_receipt",
    reference: receiptReference,
    supplier: "Synthetic scanner qualification supplier",
    lines: [
      { id: firstLine, sku, lot, quantity: 3 },
      { id: secondLine, sku, lot, quantity: 2 },
    ],
  });
  const receiptId = receipt.result.entity_id;
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(operatorToken);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Warehouse overview", exact: true }),
  ).toBeVisible();
  await navigate("Scanner");
  await loadReceipt(secondLine);
  const beforeScan = await read();
  await scan("bin", storage);
  await expect(page.getByRole("alert")).toContainText("Wrong location");
  await scan("bin", receiving);
  await scan("SKU", "WRONG-SKU");
  await expect(page.getByRole("alert")).toContainText("Wrong SKU");
  await scan("SKU", sku);
  await scan("lot", "WRONG-LOT");
  await expect(page.getByRole("alert")).toContainText("Wrong lot");
  await scan("lot", lot);
  await page.getByLabel("Physical quantity (each)").fill("2");
  await page.getByLabel("Physical quantity (each)").press("Enter");
  expect((await read()).version).toBe(beforeScan.version);
  await confirm("Confirm scanned receipt", 2);
  let canonicalReceipt = await read(`${root}/receipts/${receiptId}`);
  expect(
    canonicalReceipt.lines.find((line: any) => line.id === firstLine).received,
  ).toBe(0);
  expect(
    canonicalReceipt.lines.find((line: any) => line.id === secondLine).received,
  ).toBe(2);
  expect(
    (await read()).stocks.find(
      (stock: any) => stock.sku === sku && stock.location === receiving,
    ).on_hand,
  ).toBe(2);

  await loadReceipt(firstLine);
  await verify(receiving);
  await confirm("Confirm scanned receipt", 3);
  canonicalReceipt = await read(`${root}/receipts/${receiptId}`);
  expect(canonicalReceipt.status).toBe("received");
  expect(
    canonicalReceipt.lines.map((line: any) => line.received).sort(),
  ).toEqual([2, 3]);
  await navigate("Inventory");
  await page
    .getByRole("row")
    .filter({ hasText: sku })
    .filter({ hasText: receiving })
    .getByRole("button", { name: "Move", exact: true })
    .click();
  await page
    .getByRole("combobox", { name: "To storage location", exact: true })
    .selectOption(storage);
  await page
    .getByRole("button", { name: "Confirm movement", exact: true })
    .click();
  await saved();
  expect(
    (await read()).stocks.find(
      (stock: any) => stock.sku === sku && stock.location === storage,
    ).on_hand,
  ).toBe(5);

  const created = await command({
    type: "create_order",
    reference: orderReference,
    source_order_id: `oms-scanner-qualification-${suffix}`,
    customer: "Synthetic scanner qualification customer",
    destination: "Verified synthetic destination",
    priority: "normal",
    lines: [
      { id: firstLine, sku, quantity: 2 },
      { id: secondLine, sku, quantity: 3 },
    ],
  });
  const orderId = created.result.entity_id;
  await command({ type: "allocate", order_id: orderId });
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await navigate("Scanner");
  await page
    .getByRole("button", { name: "Pick with scanner", exact: true })
    .click();
  await loadPick(secondLine);
  await verify(storage);
  await page.getByLabel("Physical quantity (each)").fill("1");
  const frozenVersion = (await read()).version;
  await command(
    {
      type: "count_stock",
      sku,
      lot,
      location: storage,
      counted: 5,
      reason:
        "Distinct qualification operator verified the complete bin while the scanner review was open",
    },
    counterHeaders,
  );
  const counted = await read();
  expect(counted.version).toBe(frozenVersion + 1);
  expect(
    counted.activity.some(
      (activity: any) =>
        activity.kind === "inventory.counted" &&
        activity.actor === counter.identity.actor &&
        activity.reference.includes(sku),
    ),
  ).toBe(true);
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText(
    "Warehouse changed after this work was selected",
  );
  await expect(
    page.getByRole("button", { name: "Confirm scanned pick", exact: true }),
  ).toBeDisabled();
  let order = await read(`${root}/orders/${orderId}`);
  expect(
    order.allocations.every((allocation: any) => allocation.picked === 0),
  ).toBe(true);
  await loadPick(secondLine, true);
  await verify(storage);
  await confirm("Confirm scanned pick", 1);
  order = await read(`${root}/orders/${orderId}`);
  expect(
    order.allocations.find(
      (allocation: any) => allocation.line_id === firstLine,
    ).picked,
  ).toBe(0);
  expect(
    order.allocations.find(
      (allocation: any) => allocation.line_id === secondLine,
    ).picked,
  ).toBe(1);

  await page.reload();
  await expect(
    page.getByRole("heading", { name: "Scanner workbench", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Pick with scanner", exact: true })
    .click();
  await loadPick(secondLine);
  await verify(storage);
  await confirm("Confirm scanned pick", 2);
  await loadPick(firstLine);
  await verify(storage);
  await confirm("Confirm scanned pick", 2);
  order = await read(`${root}/orders/${orderId}`);
  expect(order.status).toBe("picked");
  expect(
    order.allocations.map((allocation: any) => allocation.picked).sort(),
  ).toEqual([2, 3]);
  await openOrder();
  await page
    .getByRole("button", { name: "Confirm packed", exact: true })
    .click();
  await saved();
  await openOrder();
  await page
    .getByRole("button", { name: "Record handoff", exact: true })
    .click();
  await page
    .getByLabel("Carrier", { exact: true })
    .fill("Scanner qualification carrier");
  await page
    .getByLabel("Carrier pickup / tracking reference", { exact: true })
    .fill(`SCAN-PICKUP-${suffix}`);
  await page
    .getByRole("button", { name: "Confirm physical handoff", exact: true })
    .click();
  await saved();
  await page.reload();
  order = await read(`${root}/orders/${orderId}`);
  expect(order.status).toBe("shipped");
  expect(order.handoff_reference).toBe(`SCAN-PICKUP-${suffix}`);
  expect(
    order.allocations.map((allocation: any) => allocation.shipped).sort(),
  ).toEqual([2, 3]);
  const persisted = await read();
  expect(
    persisted.stocks
      .filter((stock: any) => stock.sku === sku)
      .every((stock: any) => stock.on_hand === 0 && stock.reserved === 0),
  ).toBe(true);
  const receipts = persisted.movements.filter(
    (movement: any) => movement.receipt_id === receiptId,
  );
  expect(
    receipts
      .map((movement: any) => [movement.receipt_line_id, movement.quantity])
      .sort(),
  ).toEqual(
    [
      [firstLine, 3],
      [secondLine, 2],
    ].sort(),
  );
  expect(
    persisted.movements
      .filter(
        (movement: any) => movement.sku === sku && movement.kind === "pick",
      )
      .map((movement: any) => movement.quantity)
      .sort(),
  ).toEqual([1, 2, 2]);
  const proof = await read(`${root}/reconciliation`);
  expect(proof.ok).toBe(true);
  expect(proof.version).toBe(persisted.version);
  expect(proof.method).toBe(
    "full_append_only_ledger_and_order_line_allocations",
  );
  expect(pageErrors).toEqual([]);
});
