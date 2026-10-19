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
  // A distinct operator counts an unrelated lot after these labels were
  // verified. Refresh must retain this receipt's unaffected physical review.
  await command(
    {
      type: "count_stock",
      sku,
      lot: `OTHER-${suffix}`,
      location: storage,
      counted: 0,
      reason: "Independent lot review while receiving labels remain verified",
    },
    counterHeaders,
  );
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Confirm scanned receipt", exact: true }),
  ).toBeEnabled();
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
      (stock: any) =>
        stock.sku === sku && stock.lot === lot && stock.location === receiving,
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
      (stock: any) =>
        stock.sku === sku && stock.lot === lot && stock.location === storage,
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

test("replenishment moves and customer returns retain exact physical and source evidence", async ({
  page,
}) => {
  test.setTimeout(120000);
  page.setDefaultTimeout(10000);
  const root = "/v1/warehouses/wh-main";
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` };
  const suffix = randomUUID().slice(0, 8);
  const sku = `WF-${suffix}`,
    lot = `LOT-${suffix}`,
    pick = `WF-PICK-${suffix}`,
    reserve = `WF-RESERVE-${suffix}`;
  const orderReference = `WF-ORDER-${suffix}`,
    rma = `RMA-${suffix}`;
  const get = async (path = root): Promise<any> => {
    const response = await page.request.get(path, { headers });
    expect(response.ok(), await response.text()).toBe(true);
    return response.json();
  };
  const command = async (command: unknown): Promise<any> => {
    const state = await get();
    const response = await page.request.post(root + "/commands", {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: state.version,
        command,
      },
    });
    expect(response.ok(), await response.text()).toBe(true);
    return response.json();
  };
  await command({
    type: "create_product",
    sku,
    name: "Workflow qualification",
    category: "Synthetic",
    reorder_point: 2,
  });
  await command({
    type: "create_location",
    code: pick,
    zone: "Pick",
    kind: "storage",
  });
  await command({
    type: "create_location",
    code: reserve,
    zone: `Reserve-${suffix}`,
    kind: "storage",
  });
  await command({
    type: "count_stock",
    sku,
    lot,
    location: reserve,
    counted: 10,
    reason: "Synthetic opening count",
  });
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(token);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Warehouse overview", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  await page.getByText("Pick-face replenishment", { exact: true }).click();
  await page.getByLabel("Replenishment SKU", { exact: true }).selectOption(sku);
  await page
    .getByLabel("Pick-face destination", { exact: true })
    .selectOption(pick);
  await page
    .getByLabel("Reserve source zone", { exact: true })
    .selectOption(`Reserve-${suffix}`);
  await page.getByLabel("Minimum available (each)", { exact: true }).fill("2");
  await page.getByLabel("Target available (each)", { exact: true }).fill("5");
  await page
    .getByRole("button", { name: "Plan replenishment", exact: true })
    .click();
  await expect(
    page.getByText("0 available · 5 planned · 0 still unavailable", {
      exact: true,
    }),
  ).toBeVisible();
  const moved = page.waitForResponse(
    (r) =>
      r.url().endsWith(root + "/commands") && r.request().method() === "POST",
  );
  await page
    .getByRole("button", { name: "Confirm physical move", exact: true })
    .click();
  const moveResponse = await moved;
  expect(moveResponse.ok()).toBe(true);
  const submitted = moveResponse.request().postDataJSON();
  expect(Object.keys(submitted.resource_versions)).toHaveLength(5);
  const afterMove = await get();
  expect(
    afterMove.stocks.find((s: any) => s.sku === sku && s.location === pick)
      .on_hand,
  ).toBe(5);
  expect(
    afterMove.stocks.find((s: any) => s.sku === sku && s.location === reserve)
      .on_hand,
  ).toBe(5);
  const created = await command({
    type: "create_order",
    reference: orderReference,
    source_order_id: null,
    customer: "Synthetic customer",
    destination: "Synthetic destination",
    priority: "normal",
    lines: [
      { id: `line-a-${suffix}`, sku, quantity: 2 },
      { id: `line-b-${suffix}`, sku, quantity: 1 },
    ],
  });
  const orderId = created.result.entity_id;
  await command({ type: "allocate", order_id: orderId });
  const allocated = await get(`${root}/orders/${orderId}`);
  for (const a of allocated.allocations)
    await command({
      type: "pick",
      order_id: orderId,
      line_id: a.line_id,
      location: a.location,
      lot: a.lot,
      quantity: a.quantity,
    });
  await command({ type: "pack", order_id: orderId });
  await command({
    type: "ship",
    order_id: orderId,
    carrier: "Synthetic carrier",
    handoff_reference: `HANDOFF-${suffix}`,
  });
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await page.getByRole("button", { name: orderReference, exact: true }).click();
  await page
    .getByRole("button", { name: "Review return allowance", exact: true })
    .click();
  await expect(
    page.getByText("Allowance checked against the original recorded handoff.", {
      exact: true,
    }),
  ).toBeVisible();
  await page.getByLabel("RMA reference", { exact: true }).fill(rma);
  await page
    .getByLabel("Original line and lot", { exact: true })
    .selectOption(JSON.stringify([`line-a-${suffix}`, lot]));
  await page.getByLabel("Return quantity (each)", { exact: true }).fill("1");
  await page
    .getByLabel("Return reason / inspection evidence", { exact: true })
    .fill("Synthetic customer return; inspect before restocking");
  const authorized = page.waitForResponse(
    (r) =>
      r.url().endsWith(root + "/commands") && r.request().method() === "POST",
  );
  await page
    .getByRole("button", { name: "Authorize return receipt", exact: true })
    .click();
  expect((await authorized).ok()).toBe(true);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const afterAuthorization = await get();
  expect(
    afterAuthorization.stocks
      .filter((s: any) => s.sku === sku)
      .reduce((sum: number, s: any) => sum + s.on_hand, 0),
  ).toBe(7);
  const receipt = afterAuthorization.receipts.find(
    (r: any) => r.reference === rma,
  );
  expect(receipt.return_source.order_id).toBe(orderId);
  expect(Object.values(receipt.return_source.line_ids)).toEqual([
    `line-a-${suffix}`,
  ]);
  await page.getByRole("button", { name: "Receiving", exact: true }).click();
  await page.getByRole("button", { name: rma, exact: true }).click();
  await expect(
    page.getByText(`Customer return · original order ${orderId}`, {
      exact: false,
    }),
  ).toBeVisible();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Receive goods", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Confirm receipt", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const received = await get();
  expect(
    received.stocks.find((s: any) => s.sku === sku && s.location === "RECV-01")
      .on_hand,
  ).toBe(1);
  const original = await get(`${root}/orders/${orderId}`);
  expect(original.status).toBe("shipped");
  const allowance = await get(`${root}/orders/${orderId}/return-eligibility`);
  expect(
    allowance.lines.find((l: any) => l.order_line_id === `line-a-${suffix}`)
      .remaining,
  ).toBe(1);
  const proof = await get(root + "/reconciliation");
  expect(proof.ok).toBe(true);
  expect(proof.returns.find((r: any) => r.order_id === orderId)).toMatchObject({
    authorized: 1,
    received: 1,
    shipped: 2,
    ok: true,
  });
});
