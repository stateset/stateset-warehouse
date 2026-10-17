import { test, expect } from "@playwright/test";

test("real browser and Postgres recover a same-SKU short pick through physical handoff", async ({
  page,
}) => {
  test.setTimeout(120000);
  page.setDefaultTimeout(10000);
  const suffix = `${Date.now()}-${Math.floor(Math.random() * 10000)}`;
  const sku = `E2E-${suffix}`;
  const reference = `SO-E2E-${suffix}`;
  const lot = `LOT-${suffix}`;
  const receiptReference = `RCV-E2E-${suffix}`;
  const replenishmentReference = `RCV-REPL-${suffix}`;
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const navigate = async (name: string) =>
    page.getByRole("button", { name, exact: true }).click();
  const saved = async () => expect(page.getByRole("dialog")).toHaveCount(0);
  const openOrder = async () => {
    await navigate("Fulfillment");
    await page.getByRole("button", { name: reference, exact: true }).click();
    await expect(page.getByRole("dialog")).toContainText(reference);
  };
  const createReceiveAndPutAway = async (receipt: string, quantity: number) => {
    await navigate("Receiving");
    await page
      .getByRole("button", { name: "New receipt", exact: true })
      .click();
    await page.getByLabel("Receipt reference", { exact: true }).fill(receipt);
    await page
      .getByLabel("Supplier", { exact: true })
      .fill("Browser test supplier");
    await page.getByLabel("SKU line 1", { exact: true }).selectOption(sku);
    await page.getByLabel("Lot line 1", { exact: true }).fill(lot);
    await page
      .getByLabel("Quantity line 1", { exact: true })
      .fill(String(quantity));
    await page
      .getByRole("button", { name: "Create receipt", exact: true })
      .click();
    await saved();
    await page.getByRole("button", { name: receipt, exact: true }).click();
    await page
      .getByRole("dialog")
      .getByRole("button", { name: "Receive goods", exact: true })
      .click();
    await page
      .getByRole("button", { name: "Confirm receipt", exact: true })
      .click();
    await saved();
    await navigate("Inventory");
    await page
      .getByRole("row")
      .filter({ hasText: sku })
      .filter({ hasText: "RECV-01" })
      .getByRole("button", { name: "Move", exact: true })
      .click();
    await page
      .getByRole("combobox", { name: "To storage location", exact: true })
      .selectOption("A-01-01");
    await page
      .getByRole("button", { name: "Confirm movement", exact: true })
      .click();
    await saved();
  };

  await page.goto("/");
  await page
    .getByRole("button", { name: "Open synthetic demo", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Warehouse overview", exact: true }),
  ).toBeVisible();
  await navigate("Inventory");
  await page.getByRole("button", { name: "Add product", exact: true }).click();
  await page.getByLabel("SKU", { exact: true }).fill(sku);
  await page
    .getByLabel("Product name", { exact: true })
    .fill("Browser short pick recovery");
  await page
    .getByRole("button", { name: "Create product", exact: true })
    .click();
  await saved();
  await createReceiveAndPutAway(receiptReference, 6);

  await navigate("Fulfillment");
  await page.getByRole("button", { name: "New order", exact: true }).click();
  await page.getByLabel("Order reference", { exact: true }).fill(reference);
  await page
    .getByLabel("OMS order ID (optional)", { exact: true })
    .fill(`oms-${suffix}`);
  await page.getByLabel("Customer", { exact: true }).fill("Browser customer");
  await page.getByLabel("Destination", { exact: true }).fill("Vancouver, BC");
  await page.getByLabel("SKU line 1", { exact: true }).selectOption(sku);
  await page.getByLabel("Quantity line 1", { exact: true }).fill("3");
  await page.getByRole("button", { name: "Add item", exact: true }).click();
  await page.getByLabel("SKU line 2", { exact: true }).selectOption(sku);
  await page.getByLabel("Quantity line 2", { exact: true }).fill("2");
  await page.getByRole("button", { name: "Create order", exact: true }).click();
  await saved();
  await openOrder();
  await page
    .getByRole("button", { name: "Allocate stock", exact: true })
    .click();
  await saved();
  await openOrder();
  await page
    .getByRole("button", { name: "Pick units", exact: true })
    .first()
    .click();
  await page.getByLabel("Picked quantity (each)", { exact: true }).fill("2");
  await page.getByRole("button", { name: "Confirm pick", exact: true }).click();
  await saved();
  await openOrder();
  await page
    .getByRole("button", { name: "Report exception", exact: true })
    .click();
  await page
    .getByLabel("Observed issue", { exact: true })
    .fill(`Physical shortage ${suffix}: only two verified units remain`);
  await page
    .getByRole("button", { name: "Open exception", exact: true })
    .click();
  await saved();
  await openOrder();
  await expect(
    page.getByRole("button", { name: "Pick units", exact: true }).first(),
  ).toBeDisabled();
  await page
    .getByRole("button", { name: "Return staged units", exact: true })
    .click();
  await page
    .getByLabel("Return evidence / reason", { exact: true })
    .fill("Physically returned both verified units to original bin");
  await page
    .getByRole("button", { name: "Confirm physical return", exact: true })
    .click();
  await saved();
  await openOrder();
  await page
    .getByRole("button", { name: "Release allocation", exact: true })
    .click();
  await page
    .getByLabel("Allocation release reason", { exact: true })
    .fill("Release both same-SKU lines for physical recount");
  await page
    .getByRole("button", { name: "Release reservations", exact: true })
    .click();
  await saved();
  await navigate("Inventory");
  await page
    .getByRole("row")
    .filter({ hasText: sku })
    .filter({ hasText: "A-01-01" })
    .getByRole("button", { name: "Count", exact: true })
    .click();
  await page.getByLabel(/Counted each units/).fill("2");
  await page
    .getByLabel("Count evidence / reason", { exact: true })
    .fill("Recount confirms only two returned units; record four-unit loss");
  await page
    .getByRole("button", { name: "Apply count adjustment", exact: true })
    .click();
  await saved();
  await createReceiveAndPutAway(replenishmentReference, 3);
  await navigate("Exceptions");
  await page
    .locator("article")
    .filter({ hasText: `Physical shortage ${suffix}` })
    .getByRole("button", { name: "Resolve", exact: false })
    .click();
  await page
    .getByLabel("Resolution evidence", { exact: true })
    .fill(
      "Recount recorded the loss and three replenishment units are physically put away",
    );
  await page
    .getByRole("button", { name: "Resolve issue", exact: true })
    .click();
  await saved();
  await openOrder();
  await page
    .getByRole("button", { name: "Allocate stock", exact: true })
    .click();
  await saved();
  for (let line = 0; line < 2; line++) {
    await openOrder();
    await page
      .getByRole("button", { name: "Pick units", exact: true })
      .first()
      .click();
    await page
      .getByRole("button", { name: "Confirm pick", exact: true })
      .click();
    await saved();
  }
  await openOrder();
  await page
    .getByRole("button", { name: "Confirm packed", exact: true })
    .click();
  await saved();
  await openOrder();
  await page
    .getByRole("button", { name: "Record handoff", exact: true })
    .click();
  await page.getByLabel("Carrier", { exact: true }).fill("UPS");
  await page
    .getByLabel("Carrier pickup / tracking reference", { exact: true })
    .fill(`PICKUP-${suffix}`);
  await page
    .getByRole("button", { name: "Confirm physical handoff", exact: true })
    .click();
  await saved();
  await page.reload();
  await openOrder();
  await expect(page.getByRole("dialog")).toContainText(
    "Physical handoff recorded",
  );
  await expect(page.getByRole("dialog")).toContainText(`PICKUP-${suffix}`);
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  await navigate("Movement ledger");
  await expect(
    page.getByText("Inventory ledger reconciles", { exact: true }),
  ).toBeVisible();
  const response = await page.request.get("/v1/warehouses/wh-pdx", {
    headers: { Authorization: "Bearer demo-warehouse" },
  });
  expect(response.ok()).toBe(true);
  const persisted = await response.json();
  const completed = persisted.orders.find(
    (order: any) => order.reference === reference,
  );
  expect(completed.status).toBe("shipped");
  expect(completed.lines.map((line: any) => line.sku)).toEqual([sku, sku]);
  expect(
    completed.allocations
      .map((allocation: any) => allocation.shipped)
      .sort((a: number, b: number) => a - b),
  ).toEqual([2, 3]);
  expect(
    persisted.stocks
      .filter((stock: any) => stock.sku === sku)
      .reduce((sum: number, stock: any) => sum + stock.on_hand, 0),
  ).toBe(0);
  expect(errors).toEqual([]);
});
