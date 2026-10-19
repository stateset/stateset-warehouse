// Actual built browser → API → isolated PostgreSQL. No HTTP interception.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
test("lot controls preserve real custody and original recovery through expiry, quarantine, package unpack and scanner return", async ({
  page,
}) => {
  test.setTimeout(120000);
  page.setDefaultTimeout(15000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` };
  const suffix = randomUUID().slice(0, 8),
    sku = `QUALITY-${suffix}`,
    lot = "EXACT-EXPIRY-LOT",
    bin = `QA-${suffix}`,
    reference = `QA-ORDER-${suffix}`,
    packageRef = `QA-PACK-${suffix}`;
  const get = async (path: string) => {
    const r = await page.request.get(path, { headers });
    expect(r.ok(), await r.text()).toBe(true);
    return r.json();
  };
  const state = () => get("/v1/warehouses/wh-main");
  const command = async (command: unknown) => {
    const s = await state();
    const r = await page.request.post("/v1/warehouses/wh-main/commands", {
      headers,
      data: { request_id: randomUUID(), expected_version: s.version, command },
    });
    expect(r.ok(), await r.text()).toBe(true);
    return r.json();
  };
  await command({
    type: "create_product",
    sku,
    name: "Synthetic lot-control fixture",
    category: "Test",
    reorder_point: 1,
  });
  await command({
    type: "create_location",
    code: bin,
    zone: "Quality test",
    kind: "storage",
  });
  await command({
    type: "count_stock",
    sku,
    lot,
    location: bin,
    counted: 10,
    reason: "Witnessed synthetic opening stock",
  });
  const created = await command({
    type: "create_order",
    reference,
    customer: "Synthetic quality customer",
    destination: "Test dock",
    priority: "normal",
    source_order_id: null,
    lines: [{ id: "quality-line", sku, quantity: 3 }],
  });
  const oid = created.result.entity_id;
  const browserErrors: string[] = [];
  page.on("pageerror", (e) => browserErrors.push(e.message));
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(token);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  const refresh = () =>
    page
      .getByRole("button", { name: "Refresh warehouse", exact: true })
      .click();
  const openLots = async () => {
    await page.getByRole("button", { name: "Inventory", exact: true }).click();
    const p = page.locator("details").filter({
      has: page.locator("summary", { hasText: "Lot expiry and quarantine" }),
    });
    if (!(await p.evaluate((n) => (n as HTMLDetailsElement).open)))
      await p.locator("summary").click();
    await p.getByLabel("Lot-control SKU", { exact: true }).selectOption(sku);
    await expect(
      p.getByRole("table", { name: "Lot availability" }),
    ).toContainText(lot);
    return p;
  };
  const control = async (
    mode: string,
    reason: string,
    expiry?: string,
    hold?: string,
  ) => {
    const p = await openLots();
    await p.getByLabel("Lot-control action").selectOption(mode);
    if (mode !== "set_product_expiry_policy")
      await p.getByLabel("Exact lot", { exact: true }).fill(lot);
    if (expiry)
      await p.getByLabel("Expiry instant including UTC offset").fill(expiry);
    if (hold)
      await p
        .getByLabel("Exact active hold", { exact: true })
        .selectOption(hold);
    await p.getByLabel("Lot-control reason").fill(reason);
    await p
      .getByRole("button", { name: "Review lot change", exact: true })
      .click();
    await p
      .getByRole("checkbox", {
        name: "I checked the original label or investigation evidence for this change.",
      })
      .check();
    const pending = page.waitForRequest(
      (r) =>
        r.method() === "POST" &&
        r.url().endsWith("/commands") &&
        r.postDataJSON().command.type === mode,
    );
    await p
      .getByRole("button", { name: "Confirm lot change", exact: true })
      .click();
    const original = (await pending).postDataJSON();
    await expect
      .poll(
        async () =>
          (await get(`/v1/warehouses/wh-main/commands/${original.request_id}`))
            .found,
      )
      .toBe(true);
    await expect(
      p.getByText("Lot control saved. Physical quantities were not changed.", {
        exact: true,
      }),
    ).toBeVisible();
    return original;
  };
  const originalExpiry = await control(
    "record_lot_expiry",
    "Read exact original manufacturer label",
    "2099-10-21T05:00:00-07:00",
  );
  const originalReceipt = await get(
    `/v1/warehouses/wh-main/commands/${originalExpiry.request_id}`,
  );
  expect(originalReceipt.found).toBe(true);
  await command({ type: "allocate", order_id: oid });
  await refresh();
  await control("quarantine_lot", "First independent investigation");
  await control(
    "quarantine_lot",
    "Second independent temperature investigation",
  );
  let s = await state();
  const active = s.lot_controls.find((c: any) => c.sku === sku);
  expect(active.holds).toHaveLength(2);
  expect(
    s.stocks.find((b: any) => b.sku === sku && b.location === bin).reserved,
  ).toBe(3);
  const openOrder = async () => {
    await page
      .getByRole("button", { name: "Fulfillment", exact: true })
      .click();
    await page.getByRole("button", { name: reference, exact: true }).click();
    await expect(
      page.getByRole("heading", {
        name: "Shipment packages and backorders",
        exact: true,
      }),
    ).toBeVisible();
  };
  await openOrder();
  await expect(
    page.getByRole("button", { name: "Pick units", exact: true }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  for (const h of active.holds)
    await control(
      "release_lot_hold",
      `Supervisor closed exact hold ${h.id}`,
      undefined,
      h.id,
    );
  s = await state();
  expect(s.lot_controls.find((c: any) => c.sku === sku).holds).toHaveLength(0);
  await page.getByRole("button", { name: "Scanner", exact: true }).click();
  await page
    .getByRole("button", { name: "Pick with scanner", exact: true })
    .click();
  await page.getByLabel("Order reference", { exact: true }).fill(reference);
  await page.getByRole("button", { name: "Load work", exact: true }).click();
  for (const [kind, value] of [
    ["bin", bin],
    ["SKU", sku],
    ["lot", lot],
  ]) {
    const input = page.getByRole("textbox", {
      name: `Scan ${kind}`,
      exact: true,
    });
    await input.fill(value);
    await input.press("Enter");
  }
  await page.getByLabel("Physical quantity (each)", { exact: true }).fill("3");
  await page
    .getByRole("button", { name: "Confirm scanned pick", exact: true })
    .click();
  await expect
    .poll(
      async () =>
        (await get(`/v1/warehouses/wh-main/orders/${oid}`)).allocations[0]
          .picked,
    )
    .toBe(3);
  await openOrder();
  await page.getByLabel("Package reference", { exact: true }).fill(packageRef);
  await page
    .getByRole("spinbutton", {
      name: `Package each units quality-line ${bin} ${lot}`,
      exact: true,
    })
    .fill("3");
  await page
    .getByRole("button", { name: "Confirm packed shipment", exact: true })
    .click();
  await openOrder();
  await expect(
    page.getByRole("button", { name: `Hand off ${packageRef}`, exact: true }),
  ).toBeEnabled();
  await command({
    type: "quarantine_lot",
    sku,
    lot,
    reason: "Actual issue found in the staged package",
  });
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  await refresh();
  await openOrder();
  await expect(
    page.getByRole("button", { name: `Hand off ${packageRef}`, exact: true }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", { name: `Unpack ${packageRef}`, exact: true }),
  ).toBeEnabled();
  await page
    .getByRole("button", { name: `Unpack ${packageRef}`, exact: true })
    .click();
  await page
    .getByLabel("Unpacking evidence / reason", { exact: true })
    .fill("Actually unpacked held units for recovery");
  await page
    .getByRole("button", { name: "Confirm unpacking", exact: true })
    .click();
  await openOrder();
  await page
    .getByRole("button", { name: "Return staged units", exact: true })
    .click();
  await page
    .getByLabel("Return evidence / reason", { exact: true })
    .fill("Actually returned held stock to original bin");
  await page
    .getByRole("button", { name: "Confirm physical return", exact: true })
    .click();
  await openOrder();
  await page
    .getByLabel("Reservation release reason", { exact: true })
    .fill("Recover unpicked held inventory");
  await page
    .getByRole("button", { name: "Release unpicked reservations", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await control(
    "correct_lot_expiry",
    "Supervisor corrected expiry against original label",
    "2000-01-01T00:00:00Z",
  );
  s = await state();
  const lastHold = s.lot_controls.find((c: any) => c.sku === sku).holds[0].id;
  await control(
    "release_lot_hold",
    "Supervisor closed staged investigation",
    undefined,
    lastHold,
  );
  const p = await openLots();
  await expect(
    p.getByRole("table", { name: "Lot availability" }),
  ).toContainText("Expired");
  const replay = await page.request.post("/v1/warehouses/wh-main/commands", {
    headers,
    data: originalExpiry,
  });
  expect(replay.ok(), await replay.text()).toBe(true);
  const body = await replay.json();
  expect(body.replayed).toBe(true);
  expect(body.result).toEqual(originalReceipt.result);
  s = await state();
  expect(
    s.stocks
      .filter((b: any) => b.sku === sku)
      .reduce((n: number, b: any) => n + b.on_hand, 0),
  ).toBe(10);
  expect(
    s.stocks
      .filter((b: any) => b.sku === sku)
      .reduce((n: number, b: any) => n + b.reserved, 0),
  ).toBe(0);
  expect(s.lot_controls.find((c: any) => c.sku === sku).expires_at).toBe(
    "2000-01-01T00:00:00+00:00",
  );
  const proof = await get("/v1/warehouses/wh-main/reconciliation");
  expect(proof.ok).toBe(true);
  expect(proof.lot_controls.ok).toBe(true);
  expect(proof.lot_controls.eligibility_decisions).toBeGreaterThanOrEqual(3);
  expect(browserErrors).toEqual([]);
});
