// Built UI + actual Rust reducer through intercepted HTTP. Live database coverage
// belongs to lot-controls-live.spec.ts; these fixtures do not claim SQL admission.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), "..");
const sku = "QUALITY-SKU",
  lot = "QUALITY-LOT",
  bin = "A-01-01";
function domain(value?: unknown) {
  const r = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    {
      input: value ? JSON.stringify(value) : "",
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    },
  );
  if (r.status !== 0) throw new Error(r.stderr || "Build domain driver");
  return JSON.parse(r.stdout);
}
async function setup(
  page: Page,
  options: {
    role?: string;
    loss?: "before" | "after";
    expiry?: string;
    held?: boolean;
    serverTime?: string;
    malformed?: "available_hold" | "wrong_version";
    allocation?: boolean;
    reorderControl?: boolean;
    qualityProof?: boolean;
    inconsistentQualityOverall?: boolean;
  } = {},
) {
  let state = domain();
  const apply = (command: unknown) => {
    const r = domain({ state, command });
    if (r.error) throw new Error(r.error);
    state = r.state;
    return r.result;
  };
  apply({
    type: "create_product",
    sku,
    name: "Synthetic quality fixture",
    category: "Test",
    reorder_point: 1,
  });
  apply({
    type: "count_stock",
    sku,
    lot,
    location: bin,
    counted: 10,
    reason: "Observed synthetic opening stock",
  });
  if (options.expiry)
    apply({
      type: "record_lot_expiry",
      sku,
      lot,
      expires_at: options.expiry,
      reason: "Read original label",
    });
  if (options.allocation) {
    const o = apply({
      type: "create_order",
      reference: "QUALITY-ORDER",
      customer: "Synthetic",
      destination: "Test",
      priority: "normal",
      source_order_id: null,
      lines: [{ id: "quality-line", sku, quantity: 3 }],
    });
    apply({ type: "allocate", order_id: o.entity_id });
  }
  if (options.held)
    apply({
      type: "quarantine_lot",
      sku,
      lot,
      reason: "Original active investigation",
    });
  const submissions: any[] = [];
  const receipts = new Map<string, any>();
  let lost = false;
  const clock = () => options.serverTime || new Date().toISOString();
  const lotPage = () => {
    const control =
      state.lot_controls?.find((c: any) => c.sku === sku && c.lot === lot) ??
      null;
    const stocks = state.stocks.filter(
      (s: any) => s.sku === sku && s.lot === lot,
    );
    const physical = stocks.reduce((n: number, s: any) => n + s.on_hand, 0),
      reserved = stocks.reduce((n: number, s: any) => n + s.reserved, 0);
    const status = control?.holds.length
      ? "quarantined"
      : control?.expires_at &&
          Date.parse(control.expires_at) <= Date.parse(clock())
        ? "expired"
        : state.products.find((p: any) => p.sku === sku)?.expiry_required &&
            !control?.expires_at
          ? "expiry_missing"
          : "available";
    return {
      warehouse_id: state.warehouse.id,
      version: state.version + (options.malformed === "wrong_version" ? 1 : 0),
      checked_at: clock(),
      items: [
        {
          cursor: JSON.stringify([sku, lot]),
          sku,
          lot,
          physical,
          reserved,
          unreserved: physical - reserved,
          usable: status === "available" ? physical - reserved : 0,
          availability:
            options.malformed === "available_hold" ? "available" : status,
          expiry_required: !!state.products.find((p: any) => p.sku === sku)
            ?.expiry_required,
          control:
            options.reorderControl && control
              ? {
                  sku: control.sku,
                  lot: control.lot,
                  expiry_recorded: control.expiry_recorded,
                  expires_at: control.expires_at,
                  holds: control.holds,
                }
              : control,
        },
      ],
      next_cursor: null,
    };
  };
  await page.route("https://warehouse.test/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    const send = (body: unknown, status = 200) =>
      route.fulfill({
        status,
        contentType: "application/json",
        body: JSON.stringify(body),
      });
    if (path === "/auth/config") return send({ provider: "api_key" });
    if (path === "/v1/session")
      return send({
        identity: {
          tenant: "quality-test",
          actor: "operator:demo",
          role: options.role || "admin",
        },
        demo: true,
        integrations: [],
      });
    if (path === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
      });
    if (path.endsWith("/outbox")) return send({ items: [] });
    if (path.endsWith("/reconciliation")) {
      const proof = domain({ state });
      return send({
        ...proof,
        method: "full_append_only_ledger_and_order_line_allocations",
        ok: options.inconsistentQualityOverall
          ? true
          : proof.ok && options.qualityProof !== false,
        ...(options.qualityProof === undefined
          ? {}
          : {
              lot_controls: {
                ok: options.qualityProof,
                baseline_version: 0,
                policy_changes: 3,
                eligibility_decisions: 2,
              },
            }),
      });
    }
    if (path.endsWith("/lots"))
      return url.searchParams.get("sku") === sku
        ? send(lotPage())
        : send({
            warehouse_id: state.warehouse.id,
            version: state.version,
            checked_at: clock(),
            items: [],
            next_cursor: null,
          });
    const detail = path.match(/\/(orders|receipts)\/([^/]+)$/);
    if (detail) {
      const r = state[detail[1]].find(
        (r: any) => r.id === decodeURIComponent(detail[2]),
      );
      return r ? send(r) : send({ error: "Not found" }, 404);
    }
    if (/\/commands\/.+$/.test(path)) {
      const r = receipts.get(path.split("/").pop()!);
      return send(r ? { found: true, result: r } : { found: false });
    }
    if (path.endsWith("/commands")) {
      const body = route.request().postDataJSON();
      submissions.push(body);
      const prior = receipts.get(body.request_id);
      if (prior) return send({ replayed: true, result: prior });
      if (options.loss === "before" && !lost) {
        lost = true;
        return route.abort("failed");
      }
      if (body.expected_version !== state.version)
        return send({ error: "Warehouse changed" }, 409);
      const r = domain({ state, command: body.command });
      if (r.error) return send({ error: r.error }, 422);
      state = r.state;
      receipts.set(body.request_id, r.result);
      if (options.loss === "after" && !lost) {
        lost = true;
        return route.abort("failed");
      }
      return send({ replayed: false, result: r.result });
    }
    if (path === `/v1/warehouses/${state.warehouse.id}`)
      return send({
        ...state,
        availability_checked_at: clock(),
        reconciliation: domain({ state }),
      });
    if (path.startsWith("/v1/"))
      return send({ error: "Unhandled test route" }, 404);
    const file = resolve(
      root,
      "dashboard/dist",
      path === "/" ? "index.html" : path.slice(1),
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
  await page.route("https://fonts.googleapis.com/**", (r) => r.abort());
  await page.route("https://fonts.gstatic.com/**", (r) => r.abort());
  await page.goto("https://warehouse.test/");
  await page.getByRole("button", { name: "Open synthetic demo" }).click();
  await expect(
    page.getByRole("heading", { name: "Warehouse overview" }),
  ).toBeVisible();
  return { state: () => state, apply, submissions };
}
async function openLots(page: Page, table = true) {
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  const panel = page.locator("details").filter({
    has: page.locator("summary", { hasText: "Lot expiry and quarantine" }),
  });
  await panel.locator("summary").click();
  await panel.getByLabel("Lot-control SKU", { exact: true }).selectOption(sku);
  if (table)
    await expect(
      panel.getByRole("table", { name: "Lot availability" }),
    ).toBeVisible();
  return panel;
}
async function prepareHold(page: Page) {
  const panel = await openLots(page);
  await panel.getByLabel("Lot-control action").selectOption("quarantine_lot");
  await panel.getByLabel("Exact lot", { exact: true }).fill(lot);
  await panel
    .getByLabel("Lot-control reason")
    .fill("Observed separate quality concern");
  await panel
    .getByRole("button", { name: "Review lot change", exact: true })
    .click();
  await panel
    .getByRole("checkbox", {
      name: "I checked the original label or investigation evidence for this change.",
    })
    .check();
  return panel;
}
test("lot expiry requires an exact offset and retains independent quarantine on supervisor correction", async ({
  page,
}) => {
  const f = await setup(page, { held: true });
  const panel = await openLots(page);
  await panel.getByLabel("Exact lot", { exact: true }).fill(lot);
  await panel
    .getByLabel("Lot-control reason")
    .fill("Read exact manufacturer label");
  await panel
    .getByLabel("Expiry instant including UTC offset")
    .fill("2099-10-21");
  await panel
    .getByRole("button", { name: "Review lot change", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText("RFC3339");
  expect(f.submissions).toHaveLength(0);
  await panel
    .getByLabel("Expiry instant including UTC offset")
    .fill("2099-10-21T05:00:00-07:00");
  await panel
    .getByRole("button", { name: "Review lot change", exact: true })
    .click();
  await expect(
    panel.getByRole("button", { name: "Confirm lot change", exact: true }),
  ).toBeDisabled();
  await panel.screenshot({ path: "/tmp/wms-lot-controls-ui.png" });
  await panel
    .getByRole("checkbox", {
      name: "I checked the original label or investigation evidence for this change.",
    })
    .check();
  await panel
    .getByRole("button", { name: "Confirm lot change", exact: true })
    .click();
  await expect
    .poll(
      () => f.state().lot_controls.find((c: any) => c.sku === sku).expires_at,
    )
    .toBe("2099-10-21T12:00:00+00:00");
  expect(
    f.state().lot_controls.find((c: any) => c.sku === sku).holds,
  ).toHaveLength(1);
  expect(f.submissions[0].resource_versions).toBeUndefined();
  await panel
    .getByLabel("Lot-control action")
    .selectOption("correct_lot_expiry");
  await panel
    .getByLabel("Expiry instant including UTC offset")
    .fill("2099-10-22T12:00:00Z");
  await panel
    .getByLabel("Lot-control reason")
    .fill("Supervisor checked transcription");
  await panel
    .getByRole("button", { name: "Review lot change", exact: true })
    .click();
  await panel
    .getByRole("checkbox", {
      name: "I checked the original label or investigation evidence for this change.",
    })
    .check();
  await panel
    .getByRole("button", { name: "Confirm lot change", exact: true })
    .click();
  await expect
    .poll(
      () => f.state().lot_controls.find((c: any) => c.sku === sku).expires_at,
    )
    .toBe("2099-10-22T12:00:00+00:00");
  expect(
    f.state().lot_controls.find((c: any) => c.sku === sku).holds,
  ).toHaveLength(1);
});
for (const loss of ["before", "after"] as const)
  test(`lost ${loss} arrival quarantine preserves the exact original journal through reload`, async ({
    page,
  }) => {
    const f = await setup(page, { loss });
    const before = JSON.stringify(f.state().stocks);
    const panel = await prepareHold(page);
    await panel
      .getByRole("button", { name: "Confirm lot change", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save" }),
    ).toBeVisible();
    const original = structuredClone(f.submissions[0]);
    expect(original.command.type).toBe("quarantine_lot");
    await page.reload();
    await expect(
      page.getByRole("button", { name: "Check original save" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Check original save" }).click();
    await expect(
      page.getByRole("button", { name: "Check original save" }),
    ).toHaveCount(0);
    await expect
      .poll(
        () =>
          f.state().lot_controls?.find((c: any) => c.sku === sku)?.holds.length,
      )
      .toBe(1);
    expect(JSON.stringify(f.state().stocks)).toBe(before);
    for (const submitted of f.submissions) expect(submitted).toEqual(original);
  });
test("a reviewed lot change is held when the warehouse changes before confirmation", async ({
  page,
}) => {
  const f = await setup(page);
  const panel = await prepareHold(page);
  f.apply({
    type: "count_stock",
    sku,
    lot,
    location: bin,
    counted: 10,
    reason: "Concurrent witnessed count",
  });
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText(
    "reviewed warehouse version changed",
  );
  await expect(
    panel.getByRole("button", { name: "Confirm lot change", exact: true }),
  ).toBeDisabled();
  expect(f.submissions).toHaveLength(0);
});
for (const malformed of ["available_hold", "wrong_version"] as const)
  test(`malformed ${malformed} lot review cannot create a new control`, async ({
    page,
  }) => {
    const f = await setup(page, { held: true, malformed });
    const panel = await openLots(page, false);
    await expect(
      panel.getByRole(malformed === "available_hold" ? "alert" : "status"),
    ).toContainText(
      malformed === "available_hold"
        ? "availability disagrees"
        : "Warehouse changed",
    );
    await expect(
      panel.getByRole("button", { name: "Review lot change", exact: true }),
    ).toBeDisabled();
    expect(f.submissions).toHaveLength(0);
  });
test("operators can quarantine but cannot correct, change policy or release a hold", async ({
  page,
}) => {
  await setup(page, { role: "operator", held: true });
  const panel = await openLots(page);
  const values = await panel
    .getByLabel("Lot-control action")
    .locator("option")
    .evaluateAll((o) => o.map((n) => (n as HTMLOptionElement).value));
  expect(values).toEqual(["record_lot_expiry", "quarantine_lot"]);
  await expect(
    panel.getByRole("table", { name: "Lot availability" }),
  ).toContainText("Quarantined");
});
test("held scanner pick stays blocked while actual receiving remains available", async ({
  page,
}) => {
  const f = await setup(page, { held: true, allocation: true });
  f.apply({
    type: "create_receipt",
    reference: "QUALITY-RECEIPT",
    supplier: "Synthetic",
    lines: [{ id: "receipt-line", sku, lot, quantity: 2 }],
  });
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await page.getByRole("button", { name: "Scanner", exact: true }).click();
  await page.getByRole("button", { name: "Pick with scanner" }).click();
  await page
    .getByLabel("Order reference", { exact: true })
    .fill("QUALITY-ORDER");
  await page.getByRole("button", { name: "Load work", exact: true }).click();
  await expect(
    page.getByText("Picking is held.", { exact: false }),
  ).toBeVisible();
  await expect(
    page.getByRole("textbox", { name: "Scan bin", exact: true }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "Receive with scanner" }).click();
  await page
    .getByLabel("Receipt reference", { exact: true })
    .fill("QUALITY-RECEIPT");
  await page.getByRole("button", { name: "Load work", exact: true }).click();
  await expect(
    page.getByRole("textbox", { name: "Scan bin", exact: true }),
  ).toBeEnabled();
  await expect(
    page.getByText("Receiving actual custody is still allowed", {
      exact: false,
    }),
  ).toBeVisible();
  expect(
    f.state().stocks.find((s: any) => s.sku === sku && s.location === bin)
      .reserved,
  ).toBe(3);
});
test("scanner expiry follows server time even when the device UTC clock is wrong", async ({
  page,
}) => {
  await page.addInitScript(() => {
    Date.now = () => Date.parse("1900-01-01T00:00:00Z");
  });
  await setup(page, {
    allocation: true,
    expiry: "2099-01-01T00:00:00Z",
    serverTime: "2099-01-01T00:00:00Z",
  });
  await page.getByRole("button", { name: "Scanner", exact: true }).click();
  await page.getByRole("button", { name: "Pick with scanner" }).click();
  await page
    .getByLabel("Order reference", { exact: true })
    .fill("QUALITY-ORDER");
  await page.getByRole("button", { name: "Load work", exact: true }).click();
  await expect(page.getByText("Lot: Expired.", { exact: false })).toBeVisible();
  await expect(
    page.getByRole("textbox", { name: "Scan bin", exact: true }),
  ).toBeDisabled();
});

test("viewers inspect lot custody and original holds without mutation controls", async ({
  page,
}) => {
  await setup(page, { role: "viewer", held: true });
  const panel = await openLots(page);
  await expect(
    panel.getByRole("table", { name: "Lot availability" }),
  ).toContainText("Quarantined");
  await expect(panel.getByLabel("Lot-control action")).toHaveCount(0);
  await expect(
    panel.getByRole("button", { name: "Review lot change", exact: true }),
  ).toHaveCount(0);
});

test("supervisor releases only the selected hold and expiry remains enforced after both releases", async ({
  page,
}) => {
  const f = await setup(page, { expiry: "2000-01-01T00:00:00Z", held: true });
  const panel = await prepareHold(page);
  await panel
    .getByRole("button", { name: "Confirm lot change", exact: true })
    .click();
  await expect
    .poll(
      () => f.state().lot_controls.find((c: any) => c.sku === sku).holds.length,
    )
    .toBe(2);
  const originalIds = f
    .state()
    .lot_controls.find((c: any) => c.sku === sku)
    .holds.map((h: any) => h.id);
  for (const id of originalIds) {
    await panel
      .getByLabel("Lot-control action")
      .selectOption("release_lot_hold");
    await panel
      .getByLabel("Exact active hold", { exact: true })
      .selectOption(id);
    await panel
      .getByLabel("Lot-control reason")
      .fill("Supervisor closed this exact investigation");
    await panel
      .getByRole("button", { name: "Review lot change", exact: true })
      .click();
    await panel
      .getByRole("checkbox", {
        name: "I checked the original label or investigation evidence for this change.",
      })
      .check();
    await panel
      .getByRole("button", { name: "Confirm lot change", exact: true })
      .click();
    await expect
      .poll(() =>
        f
          .state()
          .lot_controls.find((c: any) => c.sku === sku)
          .holds.some((h: any) => h.id === id),
      )
      .toBe(false);
  }
  await expect(
    panel.getByRole("table", { name: "Lot availability" }),
  ).toContainText("Expired");
  expect(
    f.state().lot_controls.find((c: any) => c.sku === sku).expires_at,
  ).toBe("2000-01-01T00:00:00+00:00");
  expect(
    f.state().stocks.find((s: any) => s.sku === sku && s.location === bin)
      .on_hand,
  ).toBe(10);
});
test("required-label policy changes availability without changing physical custody", async ({
  page,
}) => {
  const f = await setup(page);
  const availableMetric = page
    .locator(".metric")
    .filter({ has: page.getByText("Available in storage", { exact: true }) })
    .locator("strong");
  const availableBefore = Number(
    (await availableMetric.innerText()).replaceAll(",", ""),
  );
  const before = JSON.stringify(f.state().stocks);
  const panel = await openLots(page);
  await panel
    .getByLabel("Lot-control action")
    .selectOption("set_product_expiry_policy");
  await panel
    .getByRole("checkbox", {
      name: "Require an expiry label for every lot of this SKU",
    })
    .check();
  await panel
    .getByLabel("Lot-control reason")
    .fill("Manufacturer requires expiry labels");
  await panel
    .getByRole("button", { name: "Review lot change", exact: true })
    .click();
  await panel
    .getByRole("checkbox", {
      name: "I checked the original label or investigation evidence for this change.",
    })
    .check();
  await panel
    .getByRole("button", { name: "Confirm lot change", exact: true })
    .click();
  await expect(
    panel.getByRole("table", { name: "Lot availability" }),
  ).toContainText("Expiry label required");
  expect(JSON.stringify(f.state().stocks)).toBe(before);
  await page.getByRole("button", { name: "Overview", exact: true }).click();
  await expect(availableMetric).toHaveText(
    new Intl.NumberFormat("en-US").format(availableBefore - 10),
  );
});

test("semantically identical lot evidence stays reviewable across different JSON field order", async ({
  page,
}) => {
  const f = await setup(page, {
    expiry: "2099-10-21T00:00:00Z",
    reorderControl: true,
  });
  const panel = await openLots(page);
  await panel.getByLabel("Lot-control action").selectOption("quarantine_lot");
  await panel.getByLabel("Exact lot", { exact: true }).fill(lot);
  await panel
    .getByLabel("Lot-control reason")
    .fill("Check actual evidence independent of JSON key ordering");
  await expect(
    panel.getByRole("button", { name: "Review lot change", exact: true }),
  ).toBeEnabled();
  await panel
    .getByRole("button", { name: "Review lot change", exact: true })
    .click();
  await expect(
    panel.getByRole("region", { name: "Reviewed lot change" }),
  ).toContainText("independent quarantine");
  expect(f.submissions).toHaveLength(0);
});

test("an expiry passing after label verification disables the original scanner confirmation without changing reservations", async ({
  page,
}) => {
  const f = await setup(page, {
    allocation: true,
    expiry: "2099-01-01T00:00:00Z",
    serverTime: "2098-12-31T23:59:52Z",
  });
  await page.getByRole("button", { name: "Scanner", exact: true }).click();
  await page.getByRole("button", { name: "Pick with scanner" }).click();
  await page
    .getByLabel("Order reference", { exact: true })
    .fill("QUALITY-ORDER");
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
  const confirm = page.getByRole("button", {
    name: "Confirm scanned pick",
    exact: true,
  });
  await expect(confirm).toBeEnabled();
  await expect(page.getByText("Lot: Expired.", { exact: false })).toBeVisible({
    timeout: 12000,
  });
  await expect(confirm).toBeDisabled();
  expect(f.submissions).toHaveLength(0);
  expect(
    f.state().orders.find((o: any) => o.reference === "QUALITY-ORDER")
      .allocations[0].picked,
  ).toBe(0);
  expect(
    f.state().stocks.find((s: any) => s.sku === sku && s.location === bin)
      .reserved,
  ).toBe(3);
});

test("lot controls remain usable on a narrow operator screen with readable evidence and contained table scrolling", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await setup(page, { held: true, expiry: "2099-10-21T00:00:00Z" });
  const panel = await openLots(page);
  await expect(
    panel.getByRole("table", { name: "Lot availability" }),
  ).toBeVisible();
  expect(
    await panel
      .getByRole("columnheader")
      .first()
      .evaluate((n) => parseFloat(getComputedStyle(n).fontSize)),
  ).toBeGreaterThanOrEqual(12);
  const widths = await page.evaluate(() => ({
    page: document.documentElement.scrollWidth,
    viewport: window.innerWidth,
  }));
  expect(widths.page).toBeLessThanOrEqual(widths.viewport + 1);
  await panel
    .getByRole("button", { name: "Review QUALITY-LOT", exact: true })
    .scrollIntoViewIfNeeded();
  await panel
    .getByRole("button", { name: "Review QUALITY-LOT", exact: true })
    .click();
  await expect(panel.getByLabel("Exact lot", { exact: true })).toHaveValue(lot);
});

test("legitimate expiry and quarantine do not turn original custody history into a reconciliation failure", async ({
  page,
}) => {
  await setup(page, {
    held: true,
    expiry: "2000-01-01T00:00:00Z",
    qualityProof: true,
  });
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  await expect(
    page.getByText("Inventory ledger reconciles", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Lot control history reconciles.", { exact: false }),
  ).toBeVisible();
});
test("a failed original lot proof cannot be shown as healthy even when all quantities and the aggregate flag agree", async ({
  page,
}) => {
  await setup(page, { qualityProof: false, inconsistentQualityOverall: true });
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  await expect(
    page.getByText("Full reconciliation needs review", { exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("alert")).toContainText(
    "Lot policy or original eligibility evidence needs review",
  );
  await expect(
    page.getByText("Inventory ledger reconciles", { exact: true }),
  ).toHaveCount(0);
});

test("controlled stock cannot claim a healthy complete history when the lot proof is missing", async ({
  page,
}) => {
  await setup(page, { held: true, expiry: "2000-01-01T00:00:00Z" });
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  await expect(
    page.getByText("Full reconciliation needs review", { exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("alert")).toContainText(
    "Lot policy or original eligibility evidence needs review",
  );
  await expect(
    page.getByText("Inventory ledger reconciles", { exact: true }),
  ).toHaveCount(0);
});
