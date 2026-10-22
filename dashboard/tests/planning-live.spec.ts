import { test, expect } from "@playwright/test";

test("real browser and Postgres run putaway, batch picking, counts, labels and carrier labels", async ({
  page,
}) => {
  test.setTimeout(150000);
  page.setDefaultTimeout(10000);
  const suffix = `${Date.now()}-${Math.floor(Math.random() * 10000)}`;
  const sku = `PLAN-${suffix}`;
  const lot = `LOT-${suffix}`;
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const navigate = async (name: string) =>
    page.getByRole("button", { name, exact: true }).click();
  const saved = async () => expect(page.getByRole("dialog")).toHaveCount(0);
  const panel = (name: string) =>
    page.locator("details.workflow-panel").filter({
      has: page.locator("summary", { hasText: name }),
    });
  const open = async (name: string) => {
    const details = panel(name);
    if (!(await details.evaluate((d) => (d as HTMLDetailsElement).open)))
      await details.locator("summary").click();
    return details;
  };

  await page.goto("/");
  await page
    .getByRole("button", { name: "Open synthetic demo", exact: true })
    .click();
  await navigate("Inventory");
  await page.getByRole("button", { name: "Add product", exact: true }).click();
  await page.getByLabel("SKU", { exact: true }).fill(sku);
  await page.getByLabel("Product name", { exact: true }).fill("Browser planning");
  await page.getByRole("button", { name: "Create product", exact: true }).click();
  await saved();

  // Receive four units; they wait in RECV-01 until putaway.
  await navigate("Receiving");
  await page.getByRole("button", { name: "New receipt", exact: true }).click();
  await page.getByLabel("Receipt reference", { exact: true }).fill(`RCV-${suffix}`);
  await page.getByLabel("Supplier", { exact: true }).fill("Browser test supplier");
  await page.getByLabel("SKU line 1", { exact: true }).selectOption(sku);
  await page.getByLabel("Lot line 1", { exact: true }).fill(lot);
  await page.getByLabel("Quantity line 1", { exact: true }).fill("4");
  await page.getByRole("button", { name: "Create receipt", exact: true }).click();
  await saved();
  await page.getByRole("button", { name: `RCV-${suffix}`, exact: true }).click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Receive goods", exact: true })
    .click();
  await page.getByRole("button", { name: "Confirm receipt", exact: true }).click();
  await saved();

  // Directed putaway suggests a storage bin and the move commits.
  const putaway = await open("Directed putaway");
  await putaway.getByRole("button", { name: "Suggest putaway", exact: true }).click();
  const task = putaway.locator(".workflow-task").filter({ hasText: sku });
  await expect(task).toContainText("RECV-01 →");
  await task.getByRole("button", { name: "Confirm physical putaway", exact: true }).click();
  await expect(putaway.getByRole("status")).toContainText("Putaway confirmed");

  // Two orders for the new SKU, allocated, then picked in one batch.
  for (const n of [1, 2]) {
    await navigate("Fulfillment");
    await page.getByRole("button", { name: "New order", exact: true }).click();
    await page.getByLabel("Order reference", { exact: true }).fill(`SO-${n}-${suffix}`);
    await page.getByLabel("Customer", { exact: true }).fill("Browser customer");
    await page.getByLabel("Destination", { exact: true }).fill("Vancouver, BC");
    await page.getByLabel("SKU line 1", { exact: true }).selectOption(sku);
    await page.getByLabel("Quantity line 1", { exact: true }).fill("1");
    await page.getByRole("button", { name: "Create order", exact: true }).click();
    await saved();
    await page.getByRole("button", { name: `SO-${n}-${suffix}`, exact: true }).click();
    await page.getByRole("button", { name: "Allocate stock", exact: true }).click();
    await saved();
  }
  await navigate("Fulfillment");
  const batch = await open("Batch picking");
  await batch.getByRole("spinbutton").fill("50");
  await batch.getByRole("button", { name: "Plan batch", exact: true }).click();
  const stop = batch.locator("div").filter({ has: page.locator("h4", { hasText: sku }) });
  await expect(stop.locator("h4")).toContainText("2 each");
  for (let i = 0; i < 2; i++) {
    await stop
      .locator(".workflow-task")
      .filter({ hasText: `SO-` })
      .first()
      .getByRole("button")
      .click();
    await expect(batch.getByRole("status")).toContainText(/Picked into tote|Batch complete/);
  }
  await expect(batch.locator("h4", { hasText: sku })).toHaveCount(0);

  // Cycle counts: the putaway bin was never counted, so it is due and blind.
  await navigate("Inventory");
  const counts = await open("Scheduled cycle counts");
  await counts.getByRole("button", { name: "Show counts due", exact: true }).click();
  const due = counts.locator(".workflow-task").filter({ hasText: sku });
  await expect(due.first()).toContainText("never counted");
  await expect(due.first()).not.toContainText("expected");

  // Labels download as ZPL.
  const labels = await open("Print labels");
  await labels.getByLabel("Label type", { exact: true }).selectOption("product");
  await labels.getByLabel("Label value", { exact: true }).selectOption(sku);
  const download = page.waitForEvent("download");
  await labels.getByRole("button", { name: "Download ZPL", exact: true }).click();
  expect((await download).suggestedFilename()).toMatch(/\.zpl$/);
  await expect(labels.getByRole("status")).toContainText("1 label(s) downloaded");

  expect(errors).toEqual([]);
});
