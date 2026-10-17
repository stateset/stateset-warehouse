import { test, expect } from "@playwright/test";
test("real API, Postgres and browser persist a warehouse action", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Open synthetic demo" }).click();
  await expect(
    page.getByRole("heading", { name: "Warehouse overview" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  await page.getByRole("button", { name: "Add product" }).click();
  const sku = `E2E-${Date.now()}`;
  await page.getByLabel("SKU", { exact: true }).fill(sku);
  await page.getByLabel("Product name").fill("Browser persisted product");
  await page.getByRole("button", { name: "Create product" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.reload();
  await expect(page.getByText(sku, { exact: false }).first()).toBeVisible();
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  await expect(
    page.getByText("Inventory ledger reconciles", { exact: true }),
  ).toBeVisible();
});
