// The built operator UI exercises the real Rust reducer through intercepted HTTP.
// Scanner hardware/print quality and live PostgreSQL remain separate acceptance checks.
import { expect, test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(process.cwd(), "..");
function domain(value?: unknown) {
  const result = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    {
      input: value ? JSON.stringify(value) : "",
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    },
  );
  if (result.status !== 0)
    throw new Error(result.stderr || "Build the Rust domain driver first");
  return JSON.parse(result.stdout);
}
async function setup(
  page: Page,
  loseResponse = false,
  recoveryForbidden = false,
) {
  let state = domain();
  const apply = (command: unknown) => {
    const result = domain({ state, command });
    if (result.error) throw new Error(result.error);
    state = result.state;
  };
  apply({
    type: "create_receipt",
    reference: "SCAN-RECEIPT",
    supplier: "Scanner acceptance",
    lines: [
      { id: "scan-receipt-a", sku: "SS-CAP-004", lot: "SCAN-LOT", quantity: 4 },
      { id: "scan-receipt-b", sku: "SS-CAP-004", lot: "SCAN-LOT", quantity: 3 },
    ],
  });
  apply({
    type: "create_order",
    reference: "SCAN-ORDER",
    source_order_id: null,
    customer: "Scanner acceptance",
    destination: "Verified destination",
    priority: "normal",
    lines: [
      { id: "scan-order-a", sku: "SS-CAP-004", quantity: 2 },
      { id: "scan-order-b", sku: "SS-CAP-004", quantity: 3 },
    ],
  });
  const order = state.orders.find(
    (item: any) => item.reference === "SCAN-ORDER",
  );
  apply({ type: "allocate", order_id: order.id });
  const submissions: any[] = [];
  const detailPaths: string[] = [];
  const receipts = new Map<string, any>();
  let dropped = false;
  await page.route("https://warehouse.test/**", async (route) => {
    const pathname = new URL(route.request().url()).pathname;
    const send = (body: unknown, status = 200) =>
      route.fulfill({
        status,
        contentType: "application/json",
        body: JSON.stringify(body),
      });
    if (pathname === "/auth/config") return send({ provider: "api_key" });
    if (pathname === "/v1/session")
      return send({
        identity: { tenant: "demo", actor: "operator:scanner", role: "admin" },
        demo: true,
        integrations: [],
      });
    if (pathname === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
      });
    if (pathname.endsWith("/outbox")) return send({ items: [] });
    if (pathname.endsWith("/reconciliation")) return send(domain({ state }));
    const detail = pathname.match(
      /\/warehouses\/wh-pdx\/(orders|receipts)\/([^/]+)$/,
    );
    if (detail) {
      detailPaths.push(pathname);
      const record = state[detail[1]].find(
        (item: any) => item.id === decodeURIComponent(detail[2]),
      );
      return record ? send(record) : send({ error: "Not found" }, 404);
    }
    if (/\/commands\/.+$/.test(pathname)) {
      if (recoveryForbidden) return send({ found: false });
      const original = receipts.get(pathname.split("/").pop()!);
      return send(
        original ? { found: true, result: original } : { found: false },
      );
    }
    if (pathname.endsWith("/commands")) {
      const body = route.request().postDataJSON();
      submissions.push(body);
      if (recoveryForbidden && dropped)
        return send(
          { error: "Original actor no longer has write permission" },
          403,
        );
      const original = receipts.get(body.request_id);
      if (original) return send({ replayed: true, result: original });
      if (body.expected_version !== state.version)
        return send({ error: "Stale warehouse version" }, 409);
      const result = domain({ state, command: body.command });
      if (result.error) return send({ error: result.error }, 422);
      state = result.state;
      receipts.set(body.request_id, result.result);
      if (loseResponse && !dropped) {
        dropped = true;
        return route.abort("failed");
      }
      return send({ replayed: false, result: result.result });
    }
    if (pathname === "/v1/warehouses/wh-pdx")
      return send({ ...state, reconciliation: domain({ state }) });
    if (pathname.startsWith("/v1/"))
      return send({ error: "Unhandled API route" }, 404);
    const file = resolve(
      root,
      "dashboard/dist",
      pathname === "/" ? "index.html" : pathname.slice(1),
    );
    if (!file.startsWith(resolve(root, "dashboard/dist") + "/"))
      return route.abort();
    return route.fulfill({
      contentType: file.endsWith(".js")
        ? "text/javascript"
        : file.endsWith(".css")
          ? "text/css"
          : "text/html",
      body: readFileSync(file),
    });
  });
  await page.route("https://fonts.googleapis.com/**", (route) => route.abort());
  await page.route("https://fonts.gstatic.com/**", (route) => route.abort());
  await page.goto("https://warehouse.test/");
  await page.getByRole("button", { name: "Open synthetic demo" }).click();
  await page.getByRole("button", { name: "Scanner", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Scanner workbench" }),
  ).toBeVisible();
  return { state: () => state, submissions, detailPaths, apply };
}
async function scan(page: Page, label: string, value: string) {
  const input = page.getByRole("textbox", {
    name: `Scan ${label}`,
    exact: true,
  });
  await input.fill(value);
  await input.press("Enter");
}
async function load(
  page: Page,
  reference: string,
  mode: "receive" | "pick" = "receive",
) {
  if (mode === "pick")
    await page.getByRole("button", { name: "Pick with scanner" }).click();
  await page
    .getByLabel(mode === "receive" ? "Receipt reference" : "Order reference", {
      exact: true,
    })
    .fill(reference);
  await page.getByRole("button", { name: "Load work", exact: true }).click();
  await expect(
    page.getByRole("textbox", { name: "Scan bin", exact: true }),
  ).toBeEnabled();
}
async function verifyReceipt(page: Page) {
  const bin = page.getByRole("textbox", { name: "Scan bin", exact: true });
  await bin.fill("RECV-01");
  await bin.evaluate((input) => {
    const form = (input as HTMLInputElement).form!;
    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
  });
  await expect(
    page.getByRole("textbox", { name: "Scan SKU", exact: true }),
  ).toBeEnabled();
  await scan(page, "SKU", "SS-CAP-004");
  await scan(page, "lot", "SCAN-LOT");
}

test("scanned receiving rejects wrong identities and saves only the selected same-SKU line", async ({
  page,
}) => {
  const fixture = await setup(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await load(page, "SCAN-RECEIPT");
  expect(fixture.detailPaths.some((path) => path.includes("/receipts/"))).toBe(
    true,
  );
  await page
    .getByRole("combobox", { name: "Receipt line", exact: true })
    .selectOption("scan-receipt-b");
  await scan(page, "bin", "B-01-02");
  await expect(page.getByRole("alert")).toContainText("Wrong location");
  await scan(page, "bin", "RECV-01");
  await scan(page, "SKU", "SS-TEE-001");
  await expect(page.getByRole("alert")).toContainText("Wrong SKU");
  await scan(page, "SKU", "SS-CAP-004");
  await scan(page, "lot", "OTHER-LOT");
  await expect(page.getByRole("alert")).toContainText("Wrong lot");
  expect(fixture.submissions).toHaveLength(0);
  await scan(page, "lot", "SCAN-LOT");
  const quantity = page.getByLabel("Physical quantity (each)");
  await quantity.fill("0");
  await expect(
    page.getByRole("button", { name: "Confirm scanned receipt" }),
  ).toBeDisabled();
  await quantity.fill("4");
  await expect(
    page.getByRole("button", { name: "Confirm scanned receipt" }),
  ).toBeDisabled();
  await quantity.fill("1.5");
  await expect(
    page.getByRole("button", { name: "Confirm scanned receipt" }),
  ).toBeDisabled();
  await quantity.fill("2");
  await quantity.press("Enter");
  await quantity.press("Enter");
  expect(fixture.submissions).toHaveLength(0);
  await page
    .getByRole("button", { name: "Confirm scanned receipt" })
    .evaluate((button) => {
      button.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      button.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
  await expect(page.getByText(/Save confirmed\. Reload work/)).toBeVisible();
  expect(fixture.submissions).toHaveLength(1);
  expect(fixture.submissions[0].command).toMatchObject({
    type: "receive",
    line_id: "scan-receipt-b",
    location: "RECV-01",
    quantity: 2,
  });
  const receipt = fixture
    .state()
    .receipts.find((item: any) => item.reference === "SCAN-RECEIPT");
  expect(receipt.lines.map((line: any) => line.received)).toEqual([0, 2]);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
});

test("scanned partial picks preserve original allocation and exact same-SKU line identity", async ({
  page,
}) => {
  const fixture = await setup(page);
  await load(page, "SCAN-ORDER", "pick");
  const order = fixture
    .state()
    .orders.find((item: any) => item.reference === "SCAN-ORDER");
  const allocation = order.allocations.find(
    (item: any) => item.line_id === "scan-order-b",
  );
  await page
    .getByRole("combobox", { name: "Reserved allocation", exact: true })
    .selectOption(
      JSON.stringify([allocation.line_id, allocation.location, allocation.lot]),
    );
  await scan(page, "bin", "RECV-01");
  await expect(page.getByRole("alert")).toContainText("Wrong location");
  await scan(page, "bin", allocation.location);
  await scan(page, "SKU", allocation.sku);
  await scan(page, "lot", allocation.lot);
  await page.getByLabel("Physical quantity (each)").fill("1");
  await page.getByRole("button", { name: "Confirm scanned pick" }).click();
  await expect(page.getByText(/Save confirmed\. Reload work/)).toBeVisible();
  expect(fixture.submissions).toHaveLength(1);
  expect(fixture.submissions[0].command).toMatchObject({
    type: "pick",
    order_id: order.id,
    line_id: "scan-order-b",
    location: allocation.location,
    lot: allocation.lot,
    quantity: 1,
  });
  const updated = fixture
    .state()
    .orders.find((item: any) => item.id === order.id);
  expect(
    updated.allocations.find((item: any) => item.line_id === "scan-order-a")
      .picked,
  ).toBe(0);
  expect(
    updated.allocations.find((item: any) => item.line_id === "scan-order-b")
      .picked,
  ).toBe(1);
});

test("warehouse refresh invalidates prior scans and requires a fresh canonical work review", async ({
  page,
}) => {
  const fixture = await setup(page);
  await load(page, "SCAN-RECEIPT");
  await verifyReceipt(page);
  const frozenVersion = fixture.state().version;
  fixture.apply({
    type: "create_product",
    sku: "CONCURRENT-SKU",
    name: "Concurrent catalog update",
    category: "Test",
    reorder_point: 0,
  });
  await page.getByRole("button", { name: "Refresh warehouse" }).click();
  await expect(page.getByRole("alert")).toContainText(
    "Warehouse changed after this work was selected",
  );
  await expect(
    page.getByRole("button", { name: "Confirm scanned receipt" }),
  ).toBeDisabled();
  expect(fixture.submissions).toHaveLength(0);
  await page.getByRole("button", { name: "Reload work", exact: true }).click();
  await expect(
    page.getByRole("textbox", { name: "Scan bin", exact: true }),
  ).toBeEnabled();
  await verifyReceipt(page);
  await page.getByRole("button", { name: "Confirm scanned receipt" }).click();
  await expect(page.getByText(/Save confirmed\. Reload work/)).toBeVisible();
  expect(fixture.submissions[0].expected_version).toBe(frozenVersion + 1);
});

test("lost scanned save response uses the shared durable receipt recovery after reload", async ({
  page,
}) => {
  const fixture = await setup(page, true);
  await load(page, "SCAN-RECEIPT");
  await verifyReceipt(page);
  await page.getByRole("button", { name: "Confirm scanned receipt" }).click();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeVisible();
  expect(fixture.submissions).toHaveLength(1);
  const original = fixture.submissions[0];
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Check original save" }).click();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toHaveCount(0);
  expect(fixture.submissions).toHaveLength(1);
  const receipt = fixture
    .state()
    .receipts.find((item: any) => item.reference === "SCAN-RECEIPT");
  expect(
    receipt.lines.find((line: any) => line.id === original.command.line_id)
      .received,
  ).toBe(1);
});

test("forbidden retry preserves the original uncertain scanner request and keeps writes blocked", async ({
  page,
}) => {
  const fixture = await setup(page, true, true);
  await load(page, "SCAN-RECEIPT");
  await verifyReceipt(page);
  await page.getByRole("button", { name: "Confirm scanned receipt" }).click();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeVisible();
  const pending = await page.evaluate(() =>
    sessionStorage.getItem("wms-pending:demo:operator:scanner"),
  );
  expect(pending).not.toBeNull();
  await page.reload();
  await page.getByRole("button", { name: "Check original save" }).click();
  await expect(
    page.getByText("Original actor no longer has write permission", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeVisible();
  expect(
    await page.evaluate(() =>
      sessionStorage.getItem("wms-pending:demo:operator:scanner"),
    ),
  ).toBe(pending);
  expect(fixture.submissions).toHaveLength(2);
  expect(fixture.submissions[1]).toEqual(fixture.submissions[0]);
  await expect(
    page.getByRole("button", { name: "Load work", exact: true }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "Receive with scanner" }),
  ).toBeDisabled();
  const receipt = fixture
    .state()
    .receipts.find((item: any) => item.reference === "SCAN-RECEIPT");
  expect(
    receipt.lines.find(
      (line: any) => line.id === fixture.submissions[0].command.line_id,
    ).received,
  ).toBe(1);
});

test("an open order exception blocks scanner picks before any labels can be accepted", async ({
  page,
}) => {
  const fixture = await setup(page);
  const order = fixture
    .state()
    .orders.find((item: any) => item.reference === "SCAN-ORDER");
  fixture.apply({
    type: "report_exception",
    order_id: order.id,
    category: "short_pick",
    detail: "Physical bin count needs review before any scan is saved",
  });
  await page.getByRole("button", { name: "Refresh warehouse" }).click();
  await page.getByRole("button", { name: "Pick with scanner" }).click();
  await page.getByLabel("Order reference", { exact: true }).fill("SCAN-ORDER");
  await page.getByRole("button", { name: "Load work", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText(
    "An open exception blocks this order",
  );
  await expect(
    page.getByRole("textbox", { name: "Scan bin", exact: true }),
  ).toBeDisabled();
  expect(fixture.submissions).toHaveLength(0);
});

for (const loseResponse of [false, true])
  test(`pack scanner retains the exact reviewed definition${loseResponse ? " across lost-response recovery" : ""}`, async ({
    page,
  }) => {
    const fixture = await setup(page, loseResponse);
    fixture.apply({
      type: "set_unit_pack",
      sku: "SS-CAP-004",
      code: "CASE",
      label: "Small case",
      each_per_pack: 2,
      enabled: true,
      reason: "Reviewed actual synthetic label",
    });
    await page.getByRole("button", { name: "Refresh warehouse" }).click();
    const pack = fixture.state().unit_packs[0];
    await load(page, "SCAN-RECEIPT");
    await verifyReceipt(page);
    await page
      .getByRole("combobox", { name: "Units for Physical quantity (each)" })
      .selectOption(pack.id);
    const input = page.getByRole("spinbutton", {
      name: "Physical quantity (each) (CASE packs)",
      exact: true,
    });
    await input.fill("1.5");
    await expect(
      page.getByRole("button", { name: "Confirm scanned receipt" }),
    ).toBeDisabled();
    await input.fill("3");
    await expect(
      page.getByRole("button", { name: "Confirm scanned receipt" }),
    ).toBeDisabled();
    await input.fill("2");
    await expect(
      page.getByText("2 × 2 = 4 each", { exact: true }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Confirm scanned receipt" }).click();
    if (loseResponse) {
      await expect(
        page.getByText("A save needs confirmation", { exact: true }),
      ).toBeVisible();
      fixture.apply({
        type: "set_unit_pack",
        sku: "SS-CAP-004",
        code: "CASE",
        label: "Changed case",
        each_per_pack: 3,
        enabled: true,
        reason: "Later synthetic label",
      });
      await page.reload();
      await expect(page.getByText(/Original pack observation/)).toContainText(
        "2 CASE × 2 = 4 each",
      );
      await expect(page.getByText(/Original pack observation/)).toContainText(
        pack.id,
      );
      await page.getByRole("button", { name: "Check original save" }).click();
      await expect(
        page.getByText("A save needs confirmation", { exact: true }),
      ).toHaveCount(0);
    }
    expect(fixture.submissions).toHaveLength(1);
    expect(fixture.submissions[0].unit_inputs).toEqual([
      {
        target: [],
        sku: "SS-CAP-004",
        pack_code: "CASE",
        definition_id: pack.id,
        pack_quantity: 2,
        each_per_pack: 2,
      },
    ]);
    expect(fixture.submissions[0].command.quantity).toBe(4);
  });

test("a changed pack definition invalidates scanner work until an explicit fresh review", async ({
  page,
}) => {
  const fixture = await setup(page);
  fixture.apply({
    type: "set_unit_pack",
    sku: "SS-CAP-004",
    code: "CASE",
    label: "Original case",
    each_per_pack: 2,
    enabled: true,
    reason: "Original synthetic label",
  });
  await page.getByRole("button", { name: "Refresh warehouse" }).click();
  const pack = fixture.state().unit_packs[0];
  await load(page, "SCAN-RECEIPT");
  await verifyReceipt(page);
  await page
    .getByRole("combobox", { name: "Units for Physical quantity (each)" })
    .selectOption(pack.id);
  await page
    .getByRole("spinbutton", {
      name: "Physical quantity (each) (CASE packs)",
      exact: true,
    })
    .fill("1");
  fixture.apply({
    type: "set_unit_pack",
    sku: "SS-CAP-004",
    code: "CASE",
    label: "New case",
    each_per_pack: 3,
    enabled: true,
    reason: "Changed synthetic label",
  });
  await page.getByRole("button", { name: "Refresh warehouse" }).click();
  await expect(
    page.getByRole("button", { name: "Confirm scanned receipt" }),
  ).toBeDisabled();
  expect(fixture.submissions).toHaveLength(0);
  await page.getByRole("button", { name: "Reload work", exact: true }).click();
  await verifyReceipt(page);
  await page
    .getByRole("combobox", { name: "Units for Physical quantity (each)" })
    .selectOption(fixture.state().unit_packs[0].id);
  await page
    .getByRole("spinbutton", {
      name: "Physical quantity (each) (CASE packs)",
      exact: true,
    })
    .fill("1");
  await page.getByRole("button", { name: "Confirm scanned receipt" }).click();
  expect(fixture.submissions[0].command.quantity).toBe(3);
  expect(fixture.submissions[0].unit_inputs[0].definition_id).not.toBe(pack.id);
});

test("an unreadable captured pack conversion remains retained and holds uncertain-save recovery", async ({
  page,
}) => {
  const fixture = await setup(page, true);
  fixture.apply({
    type: "set_unit_pack",
    sku: "SS-CAP-004",
    code: "CASE",
    label: "Physical case",
    each_per_pack: 2,
    enabled: true,
    reason: "Original synthetic label",
  });
  await page.getByRole("button", { name: "Refresh warehouse" }).click();
  await load(page, "SCAN-RECEIPT");
  await verifyReceipt(page);
  await page
    .getByRole("combobox", {
      name: "Units for Physical quantity (each)",
      exact: true,
    })
    .selectOption(fixture.state().unit_packs[0].id);
  await page
    .getByRole("spinbutton", {
      name: "Physical quantity (each) (CASE packs)",
      exact: true,
    })
    .fill("1");
  await page
    .getByRole("button", { name: "Confirm scanned receipt", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Check original save", exact: true }),
  ).toBeVisible();
  const retained = await page.evaluate(() => {
    const key = Object.keys(sessionStorage).find((k) =>
      k.startsWith("wms-pending:"),
    )!;
    const body = JSON.parse(sessionStorage.getItem(key)!);
    body.request.unit_inputs[0].each_per_pack = 1.5;
    const raw = JSON.stringify(body);
    sessionStorage.setItem(key, raw);
    return { key, raw };
  });
  await page.reload();
  await expect(page.getByText(/saved request is unreadable/)).toBeVisible();
  expect(
    await page.evaluate((key) => sessionStorage.getItem(key), retained.key),
  ).toBe(retained.raw);
  await expect(
    page.getByRole("button", { name: "Receive with scanner", exact: true }),
  ).toBeDisabled();
  expect(fixture.submissions).toHaveLength(1);
});
