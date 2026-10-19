// Actual built browser → authenticated API → PostgreSQL. No HTTP interception.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});

test("multiple facilities keep physical stock, selected context and original creation receipts isolated", async ({
  page,
}) => {
  test.setTimeout(90000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` };
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
  const before = await get("/v1/warehouses/wh-main");
  expect((await get("/v1/session")).default_warehouse_id).toBe("wh-main");
  const suffix = randomUUID().slice(0, 8);
  const code = `FAC-${suffix}`;
  const sku = `FACILITY-${suffix}`;
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(token);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Warehouse overview", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Change warehouse", exact: true })
    .click();
  await expect(
    page.getByLabel("Active warehouse", { exact: true }),
  ).toBeEnabled();
  await page
    .getByRole("button", { name: "New warehouse", exact: true })
    .click();
  await page.getByLabel("Warehouse code", { exact: true }).fill(code);
  await page
    .getByLabel("Warehouse name", { exact: true })
    .fill(`Synthetic facility ${suffix}`);
  await page.getByLabel("City", { exact: true }).fill("Vancouver");
  await page
    .getByLabel("IANA timezone", { exact: true })
    .fill("America/Vancouver");
  const submitted = page.waitForRequest(
    (r) =>
      r.method() === "POST" &&
      r.url().endsWith("/commands") &&
      r.postDataJSON()?.command?.type === "create_warehouse",
  );
  await page
    .getByRole("button", { name: "Create warehouse", exact: true })
    .click();
  const original = await submitted;
  const originalResponse = await original.response();
  expect(originalResponse?.ok()).toBe(true);
  const originalResult = await originalResponse!.json();
  const warehouse = original.url().split("/").at(-2)!;
  const frozen = original.postDataJSON();
  expect(frozen.expected_version).toBe(0);
  expect(frozen.resource_versions).toBeUndefined();
  await expect(page.locator(".workspace-card strong")).toHaveText(code);
  let state = await get(`/v1/warehouses/${warehouse}`);
  expect(state.version).toBe(1);
  expect(state.products).toEqual([]);
  expect(state.stocks).toEqual([]);
  expect(state.locations.map((l: any) => l.code).sort()).toEqual([
    "RECV-01",
    "STAGE-01",
  ]);
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  await page.getByRole("button", { name: "Add product", exact: true }).click();
  await page.getByLabel("SKU", { exact: true }).fill(sku);
  await page
    .getByLabel("Product name", { exact: true })
    .fill("Synthetic isolated product");
  await page.getByLabel("Reorder point (each)", { exact: true }).fill("0");
  await page
    .getByRole("button", { name: "Create product", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const command = async (value: unknown) => {
    const current = await get(`/v1/warehouses/${warehouse}`);
    const result = await page.request.post(
      `/v1/warehouses/${warehouse}/commands`,
      {
        headers,
        data: {
          request_id: randomUUID(),
          expected_version: current.version,
          command: value,
        },
      },
    );
    expect(result.ok(), await result.text()).toBe(true);
    return result.json();
  };
  await command({
    type: "create_location",
    code: "FAC-A",
    zone: "Storage",
    kind: "storage",
  });
  await command({
    type: "count_stock",
    sku,
    lot: "FAC-LOT",
    location: "FAC-A",
    counted: 5,
    reason: "Synthetic facility isolation fixture",
  });
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await page
    .locator("tr")
    .filter({ hasText: sku })
    .getByRole("button", { name: "Count", exact: true })
    .click();
  await page
    .getByLabel(`Counted each units · ${sku} / FAC-A`, { exact: true })
    .fill("7");
  await page
    .getByLabel("Count evidence / reason", { exact: true })
    .fill("Synthetic browser confirms stock in the selected facility");
  await page
    .getByRole("button", { name: "Apply count adjustment", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  state = await get(`/v1/warehouses/${warehouse}`);
  expect(state.stocks.find((s: any) => s.sku === sku).on_hand).toBe(7);
  expect(await get("/v1/warehouses/wh-main")).toEqual(before);
  await page
    .getByRole("button", { name: "Change warehouse", exact: true })
    .click();
  await expect(
    page.getByLabel("Active warehouse", { exact: true }),
  ).toBeEnabled();
  await page
    .getByLabel("Active warehouse", { exact: true })
    .selectOption("wh-main");
  await expect(page.locator(".workspace-card strong")).toHaveText(
    before.warehouse.code,
  );
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  await expect(page.locator("tr").filter({ hasText: sku })).toHaveCount(0);
  await page
    .getByRole("button", { name: "Change warehouse", exact: true })
    .click();
  await expect(
    page.getByLabel("Active warehouse", { exact: true }),
  ).toBeEnabled();
  await page
    .getByLabel("Active warehouse", { exact: true })
    .selectOption(warehouse);
  await expect(page.locator(".workspace-card strong")).toHaveText(code);
  await page.reload();
  await expect(page.locator(".workspace-card strong")).toHaveText(code);
  const replay = await page.request.post(
    `/v1/warehouses/${warehouse}/commands`,
    { headers, data: frozen },
  );
  expect(replay.ok(), await replay.text()).toBe(true);
  const receipt = await replay.json();
  expect(receipt.replayed).toBe(true);
  expect(receipt.result).toEqual(originalResult.result);
  expect(await get(`/v1/warehouses/${warehouse}`)).toEqual(state);
  for (const id of [warehouse, "wh-main"])
    expect((await get(`/v1/warehouses/${id}/reconciliation`)).ok).toBe(true);
  expect(errors).toEqual([]);
});
