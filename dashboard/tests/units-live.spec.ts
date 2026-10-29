import { navigateTo } from "./support/navigation";
// Built browser → authenticated HTTP → isolated PostgreSQL; no interception.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
test("reviewed pack definitions retain original form scanner count and recovery quantities", async ({
  page,
}, testInfo) => {
  test.setTimeout(120000);
  page.setDefaultTimeout(15000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` };
  const suffix = randomUUID().slice(0, 8),
    sku = `PACK-${suffix}`,
    bin = `PK-${suffix}`,
    reference = `CASE-RECEIPT-${suffix}`,
    lot = "CASE-LOT";
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
    sku: `A-PACK-DECOY-${suffix}`,
    name: "Unrelated packaging fixture",
    category: "Test",
    reorder_point: 0,
  });
  await command({
    type: "create_product",
    sku,
    name: "Synthetic pack product",
    category: "Test",
    reorder_point: 0,
  });
  await command({
    type: "create_location",
    code: bin,
    zone: "Synthetic",
    kind: "storage",
  });
  const errors: string[] = [],
    saved: any[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("request", (r) => {
    if (
      r.method() === "POST" &&
      new URL(r.url()).pathname.endsWith("/commands")
    )
      saved.push(r.postDataJSON());
  });
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(token);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  const details = page
    .locator("details")
    .filter({ has: page.locator("summary", { hasText: /^Pack units$/ }) });
  await details.locator("summary").click();
  const panel = page.getByRole("region", { name: "Pack units", exact: true });
  await panel.getByLabel("Pack product", { exact: true }).selectOption(sku);
  await panel
    .getByLabel("Pack label", { exact: true })
    .fill("Reviewed physical case");
  await panel.getByLabel("Each per pack", { exact: true }).fill("12");
  await panel
    .getByLabel("Pack evidence / reason", { exact: true })
    .fill("Read original synthetic case label");
  await panel
    .getByRole("button", { name: "Review pack change", exact: true })
    .click();
  await expect(panel.getByText(/Original: new pack code/)).toBeVisible();
  await panel
    .getByRole("button", { name: "Confirm pack definition", exact: true })
    .click();
  await expect
    .poll(
      async () =>
        ((await state()).unit_packs ?? []).find((p: any) => p.sku === sku)
          ?.each_per_pack,
    )
    .toBe(12);
  const pack = (await state()).unit_packs.find((p: any) => p.sku === sku);
  await navigateTo(page, "Receiving");
  await page.getByRole("button", { name: "New receipt", exact: true }).click();
  const modal = page.getByRole("dialog");
  await modal.getByLabel("Receipt reference", { exact: true }).fill(reference);
  await modal
    .getByLabel("Supplier", { exact: true })
    .fill("Synthetic supplier");
  await modal
    .getByRole("combobox", { name: "SKU line 1", exact: true })
    .selectOption(sku);
  await modal
    .getByRole("textbox", { name: "Lot line 1", exact: true })
    .fill(lot);
  await modal
    .getByRole("combobox", { name: "Units for Quantity line 1", exact: true })
    .selectOption(pack.id);
  await modal
    .getByRole("spinbutton", {
      name: "Quantity line 1 (CASE packs)",
      exact: true,
    })
    .fill("3");
  await expect(
    modal.getByText("3 × 12 = 36 each", { exact: true }),
  ).toBeVisible();
  await modal
    .getByRole("button", { name: "Create receipt", exact: true })
    .click();
  await expect(modal).toHaveCount(0);
  await page.getByRole("button", { name: "Scanner", exact: true }).click();
  await page.getByLabel("Receipt reference", { exact: true }).fill(reference);
  await page.getByRole("button", { name: "Load work", exact: true }).click();
  for (const [label, value] of [
    ["bin", "RECV-01"],
    ["SKU", sku],
    ["lot", lot],
  ]) {
    const input = page.getByRole("textbox", {
      name: `Scan ${label}`,
      exact: true,
    });
    await expect(input).toBeEnabled();
    await input.fill(value);
    await input.press("Enter");
  }
  await page
    .getByRole("combobox", {
      name: "Units for Physical quantity (each)",
      exact: true,
    })
    .selectOption(pack.id);
  await page
    .getByRole("spinbutton", {
      name: "Physical quantity (each) (CASE packs)",
      exact: true,
    })
    .fill("2");
  await page
    .getByRole("button", { name: "Confirm scanned receipt", exact: true })
    .click();
  await expect(page.getByText(/Save confirmed\. Reload work/)).toBeVisible();
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  const sourceRow = page
    .getByRole("row")
    .filter({ hasText: sku })
    .filter({ hasText: "RECV-01" });
  await sourceRow.getByRole("button", { name: "Move", exact: true }).click();
  await modal
    .getByRole("combobox", { name: "To storage location", exact: true })
    .selectOption(bin);
  await modal
    .getByRole("combobox", { name: "Units for Quantity (each)", exact: true })
    .selectOption(pack.id);
  await modal
    .getByRole("spinbutton", {
      name: "Quantity (each) (CASE packs)",
      exact: true,
    })
    .fill("1");
  await modal
    .getByRole("button", { name: "Confirm movement", exact: true })
    .click();
  await expect(modal).toHaveCount(0);
  const targetRow = page
    .getByRole("row")
    .filter({ hasText: sku })
    .filter({ hasText: bin });
  await targetRow.getByRole("button", { name: "Count", exact: true }).click();
  const countLabel = `Counted each units · ${sku} / ${bin}`;
  await modal
    .getByRole("combobox", { name: `Units for ${countLabel}`, exact: true })
    .selectOption(pack.id);
  await modal
    .getByRole("spinbutton", {
      name: `${countLabel} (CASE packs)`,
      exact: true,
    })
    .fill("1");
  await modal
    .getByLabel("Count evidence / reason", { exact: true })
    .fill("Observed one original case");
  await modal
    .getByRole("button", { name: "Apply count adjustment", exact: true })
    .click();
  await expect(modal).toHaveCount(0);
  if (!(await details.evaluate((n) => (n as HTMLDetailsElement).open)))
    await details.locator("summary").click();
  await panel.getByLabel("Pack product", { exact: true }).selectOption(sku);
  await expect(panel.getByLabel("Pack product", { exact: true })).toHaveValue(
    sku,
  );
  await panel.getByLabel("Each per pack", { exact: true }).fill("6");
  await panel
    .getByLabel("Pack evidence / reason", { exact: true })
    .fill("New actual synthetic case size");
  await panel
    .getByRole("button", { name: "Review pack change", exact: true })
    .click();
  await expect(panel.getByText(/Original: 12 each/)).toBeVisible();
  await panel
    .getByRole("button", { name: "Confirm pack definition", exact: true })
    .click();
  await expect
    .poll(
      async () =>
        ((await state()).unit_packs ?? []).find((p: any) => p.sku === sku)
          ?.each_per_pack,
    )
    .toBe(6);
  const originals = saved.filter((r) => r.unit_inputs?.length);
  expect(originals.map((r) => r.command.type)).toEqual([
    "create_receipt",
    "receive",
    "move_stock",
    "count_stock",
  ]);
  for (const original of originals) {
    expect(original.unit_inputs[0].definition_id).toBe(pack.id);
    expect(original.unit_inputs[0].each_per_pack).toBe(12);
    const response = await page.request.post(
      "/v1/warehouses/wh-main/commands",
      { headers, data: original },
    );
    expect(response.ok(), await response.text()).toBe(true);
    expect((await response.json()).replayed).toBe(true);
  }
  const receive = originals.find((r) => r.command.type === "receive"),
    session = await get("/v1/session");
  await page.evaluate(
    ({ key, journal }) => sessionStorage.setItem(key, JSON.stringify(journal)),
    {
      key: `wms-pending:${session.identity.tenant}:${session.identity.actor}`,
      journal: {
        tenant: session.identity.tenant,
        actor: session.identity.actor,
        warehouse: "wh-main",
        request: receive,
      },
    },
  );
  await page.reload();
  await expect(page.getByText(/Original pack observation/)).toContainText(
    "2 CASE × 12 = 24 each",
  );
  await expect(page.getByText(/Original pack observation/)).toContainText(
    pack.id,
  );
  await page
    .getByRole("button", { name: "Check original save", exact: true })
    .click();
  await expect(
    page.getByText("A save needs confirmation", { exact: true }),
  ).toHaveCount(0);
  const before = await state(),
    stale = {
      ...receive,
      request_id: randomUUID(),
      expected_version: before.version,
    };
  delete stale.resource_versions;
  const rejected = await page.request.post("/v1/warehouses/wh-main/commands", {
    headers,
    data: stale,
  });
  expect(rejected.status()).toBe(409);
  expect((await state()).version).toBe(before.version);
  const observations = await get(
    `/v1/warehouses/wh-main/unit-observations?sku=${sku}`,
  );
  expect(observations.total_records).toBe(4);
  expect(observations.page_source_ok).toBe(true);
  for (const row of observations.items) expect(row.each_per_pack).toBe(12);
  expect((await get("/v1/warehouses/wh-main/reconciliation")).units.ok).toBe(
    true,
  );
  expect(
    (await state()).stocks
      .filter((s: any) => s.sku === sku)
      .reduce((n: number, s: any) => n + s.on_hand, 0),
  ).toBe(24);
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  await details.locator("summary").click();
  await panel.getByLabel("Pack product", { exact: true }).selectOption(sku);
  await panel
    .getByRole("button", { name: "Review pack observations", exact: true })
    .click();
  await expect(
    panel.getByText(/4 of 4 original observations records/),
  ).toBeVisible();
  await page.screenshot({
    path: testInfo.outputPath("pack-units-desktop.png"),
    fullPage: true,
  });
  await page.setViewportSize({ width: 390, height: 844 });
  const overflow = await page.locator("body *").evaluateAll((nodes) =>
    nodes
      .filter((n) => n.getBoundingClientRect().right > window.innerWidth + 0.5)
      .map((n) => ({
        tag: n.tagName,
        className: n.className,
        right: n.getBoundingClientRect().right,
        width: n.getBoundingClientRect().width,
      })),
  );
  await testInfo.attach("pack-mobile-overflow.json", {
    body: JSON.stringify(overflow, null, 2),
    contentType: "application/json",
  });
  await expect(page.locator("html")).toHaveJSProperty("scrollWidth", 390);
  await page.screenshot({
    path: testInfo.outputPath("pack-units-mobile.png"),
    fullPage: true,
  });
  expect(errors).toEqual([]);
});
