// UI contracts on intercepted HTTP. serials-live exercises actual API/SQL.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), ".."),
  sku = "SERIAL-UI";
function domain(value?: unknown) {
  const p = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    {
      input: value ? JSON.stringify(value) : "",
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    },
  );
  if (p.status !== 0) throw Error(p.stderr);
  return JSON.parse(p.stdout);
}
async function setup(
  page: Page,
  fault?:
    | "before"
    | "after"
    | "scope"
    | "stale"
    | "trace-source"
    | "trace-stale",
) {
  let state = domain();
  const apply = (command: any) => {
    const r = domain({ state, command });
    if (r.error) throw Error(r.error);
    state = r.state;
    return r.result;
  };
  apply({
    type: "create_product",
    sku,
    name: "Synthetic serial UI",
    category: "Synthetic",
    reorder_point: 0,
    serial_controlled: true,
  });
  apply({
    type: "count_stock",
    sku,
    lot: "LOT",
    location: "RECV-01",
    counted: 1,
    reason: "Synthetic UI fixture; source proof requires live database",
  });
  const requests: any[] = [],
    receipts = new Map<string, any>();
  let dropped = false;
  let plans = 0;
  await page.route("https://warehouse.test/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    const send = (v: unknown, status = 200) =>
      route.fulfill({
        status,
        contentType: "application/json",
        body: JSON.stringify(v),
      });
    if (path === "/auth/config") return send({ provider: "api_key" });
    if (path === "/v1/session")
      return send({
        identity: { tenant: "demo", actor: "serial:ui", role: "admin" },
        demo: true,
        integrations: [],
      });
    if (path === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
      });
    if (path.endsWith("/outbox")) return send({ items: [] });
    if (path === "/v1/serials") {
      const q = new URL(route.request().url()).searchParams;
      if (q.has("after") && fault === "trace-stale")
        return send(
          { error: "Global SKU identity changed; refresh the complete review" },
          409,
        );
      return send({
        schema_version: 1,
        version_scope: "tenant_sku_identity",
        sku,
        serial: q.get("serial"),
        warehouse_id: q.get("warehouse_id"),
        view: "current",
        review: 3,
        total_records: 2,
        next_cursor: q.has("after") ? null : "original-page-position",
        current_source_ok: null,
        page_source_ok: fault !== "trace-source",
        items: [
          {
            sku,
            serial: q.has("after") ? "unit-a" : "Unit-A",
            lot: "LOT",
            revision: 1,
            place: {
              status: "on_hand",
              warehouse_id: state.warehouse.id,
              location: "RECV-01",
            },
            event_id: "original-event",
            source_ok: fault !== "trace-source",
          },
        ],
      });
    }
    if (path.endsWith("/reconciliation")) return send(domain({ state }));
    if (path.endsWith("/serial-observation-plan")) {
      const b = route.request().postDataJSON();
      plans++;
      return send({
        schema_version: 1,
        plan_id: b.plan_id,
        warehouse_id: fault === "scope" ? "wh-other" : state.warehouse.id,
        command_type: b.command.type,
        targets: [{ target: [], sku, each_quantity: b.command.counted }],
        required_labels: b.command.counted,
        resolved_targets: 1,
        observed_warehouse_version: state.version + 99,
      });
    }
    if (/\/commands\/.+$/.test(path)) {
      const id = path.split("/").pop()!;
      return send(
        receipts.has(id)
          ? { found: true, result: receipts.get(id) }
          : { found: false },
      );
    }
    if (path.endsWith("/commands")) {
      const b = route.request().postDataJSON();
      requests.push(b);
      if (fault === "before" && !dropped) {
        dropped = true;
        return route.abort("failed");
      }
      if (fault === "stale")
        return send({ error: "Original reviewed stock changed" }, 409);
      if (receipts.has(b.request_id))
        return send({ replayed: true, result: receipts.get(b.request_id) });
      const result = apply(b.command);
      receipts.set(b.request_id, result);
      if (fault === "after" && !dropped) {
        dropped = true;
        return route.abort("failed");
      }
      return send({ replayed: false, result });
    }
    if (path === "/v1/warehouses/wh-pdx")
      return send({ ...state, reconciliation: domain({ state }) });
    if (path.startsWith("/v1/"))
      return send({ error: "Unexpected test API route" }, 404);
    const file = resolve(
      root,
      "dashboard/dist",
      path === "/" ? "index.html" : path.slice(1),
    );
    if (!file.startsWith(resolve(root, "dashboard/dist") + "/"))
      return route.abort();
    return route.fulfill({
      body: readFileSync(file),
      contentType: file.endsWith(".js")
        ? "text/javascript"
        : file.endsWith(".css")
          ? "text/css"
          : "text/html",
    });
  });
  await page.goto("https://warehouse.test/");
  await page.getByRole("button", { name: "Open synthetic demo" }).click();
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  return { requests, plans: () => plans, version: () => state.version };
}
async function count(
  page: Page,
  n: number,
  reason = "Synthetic complete literal observation",
) {
  await page
    .getByRole("row")
    .filter({ hasText: sku })
    .getByRole("button", { name: "Count", exact: true })
    .click();
  await page
    .getByLabel(`Counted each units · ${sku} / RECV-01`, { exact: true })
    .fill(String(n));
  await page
    .getByLabel("Count evidence / reason", { exact: true })
    .fill(reason);
  await page
    .getByRole("button", { name: "Apply count adjustment", exact: true })
    .click();
}
async function scan(page: Page, s: string) {
  const f = page.getByRole("textbox", {
    name: `Scan serial for ${sku}`,
    exact: true,
  });
  await f.fill(s);
  await f.press("Enter");
}
for (const mobile of [false, true])
  test(`literal scans reject duplicates and preserve original fences (${mobile ? "mobile" : "desktop"})`, async ({
    page,
  }) => {
    if (mobile) await page.setViewportSize({ width: 390, height: 844 });
    const s = await setup(page);
    const version = s.version();
    await count(page, 2);
    const dialog = page.getByRole("dialog", { name: "Observe serial labels" });
    await expect(dialog).toBeVisible();
    await expect(
      page.getByRole("textbox", {
        name: `Scan serial for ${sku}`,
        exact: true,
      }),
    ).toBeFocused();
    const save = page.getByRole("button", {
      name: "Save with observed serials",
      exact: true,
    });
    await expect(save).toBeDisabled();
    await scan(page, "Unit-A");
    await scan(page, "Unit-A");
    await expect(page.getByRole("alert")).toContainText("already scanned");
    await expect(save).toBeDisabled();
    await scan(page, "unit-a");
    await expect(save).toBeEnabled();
    expect(s.requests).toHaveLength(0);
    await save.click();
    await expect(dialog).toBeHidden();
    expect(s.requests).toHaveLength(1);
    expect(s.requests[0].serial_inputs).toEqual([
      { target: [], sku, serials: ["Unit-A", "unit-a"] },
    ]);
    expect(s.requests[0].expected_version).toBe(version);
    expect(s.requests[0].expected_version).not.toBe(version + 99);
  });
test("a witnessed zero requires explicit confirmation and cancel preserves the original form", async ({
  page,
}) => {
  const s = await setup(page);
  await count(page, 0);
  const save = page.getByRole("button", { name: "Save with observed serials" });
  await expect(save).toBeDisabled();
  await page.getByRole("button", { name: "Cancel serial review" }).click();
  await expect(
    page.getByRole("dialog", { name: "Record physical count" }),
  ).toBeVisible();
  await expect(page.getByLabel("Count evidence / reason")).toHaveValue(
    "Synthetic complete literal observation",
  );
  expect(s.requests).toHaveLength(0);
  await page.getByRole("button", { name: "Apply count adjustment" }).click();
  await page
    .getByLabel("I physically observed no units for this complete count.")
    .check();
  await page
    .getByRole("button", { name: "Save with observed serials" })
    .click();
  expect(s.requests[0].serial_inputs).toEqual([
    { target: [], sku, serials: [] },
  ]);
});
for (const fault of ["before", "after"] as const)
  test(`reload recovery retains exact serial request after response loss ${fault} arrival`, async ({
    page,
  }) => {
    const s = await setup(page, fault);
    await count(page, 2);
    await scan(page, "Original-1");
    await scan(page, "original-1");
    await page
      .getByRole("button", { name: "Save with observed serials" })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save" }),
    ).toBeVisible();
    const original = structuredClone(s.requests[0]);
    const stored = await page.evaluate(() =>
      sessionStorage.getItem("wms-pending:demo:serial:ui"),
    );
    expect(JSON.parse(stored!).request).toEqual(original);
    await page.reload();
    await expect(
      page.getByRole("button", { name: "Check original save" }),
    ).toBeVisible();
    await page.getByText(/Original serial observation/).click();
    await expect(
      page.getByLabel(`Original serials for ${sku} group 1`),
    ).toHaveValue("Original-1\noriginal-1");
    await page.getByRole("button", { name: "Check original save" }).click();
    await expect(
      page.getByRole("button", { name: "Check original save" }),
    ).toBeHidden();
    expect(s.plans()).toBe(1);
    if (fault === "before") {
      expect(s.requests).toHaveLength(2);
      expect(s.requests[1]).toEqual(original);
    } else expect(s.requests).toHaveLength(1);
  });
test("a mismatched observation scope never issues a physical save and releases preparation lock", async ({
  page,
}) => {
  const s = await setup(page, "scope");
  await count(page, 1);
  await expect(page.getByRole("alert").first()).toContainText(
    "scope did not match",
  );
  expect(s.requests).toHaveLength(0);
  await expect(
    page.getByRole("button", { name: "Apply count adjustment" }),
  ).toBeEnabled();
});
test("stale save never substitutes labels or advances the original reviewed version", async ({
  page,
}) => {
  const s = await setup(page, "stale");
  await count(page, 1);
  await scan(page, "ORIGINAL");
  await page
    .getByRole("button", { name: "Save with observed serials" })
    .click();
  await expect(page.getByRole("alert").first()).toContainText(
    "Original reviewed stock changed",
  );
  expect(s.requests).toHaveLength(1);
  expect(s.requests[0].serial_inputs[0].serials).toEqual(["ORIGINAL"]);
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeHidden();
});
test("malformed retained serial observations hold recovery without dropping evidence", async ({
  page,
}) => {
  const s = await setup(page, "before");
  await count(page, 1);
  await scan(page, "ORIGINAL");
  await page
    .getByRole("button", { name: "Save with observed serials" })
    .click();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeVisible();
  await page.evaluate(() => {
    const k = "wms-pending:demo:serial:ui",
      j = JSON.parse(sessionStorage.getItem(k)!);
    j.request.serial_inputs[0].serials.push("ORIGINAL");
    sessionStorage.setItem(k, JSON.stringify(j));
  });
  await page.reload();
  await expect(page.getByText(/The retained save is unreadable/)).toBeVisible();
  expect(s.requests).toHaveLength(1);
  expect(
    await page.evaluate(() =>
      sessionStorage.getItem("wms-pending:demo:serial:ui"),
    ),
  ).toContain("ORIGINAL");
});

for (const fault of [undefined, "trace-source", "trace-stale"] as const)
  test(`serial review retains selected source status and clears expired pages (${fault ?? "paging"})`, async ({
    page,
  }) => {
    await setup(page, fault);
    await page
      .getByText("Serial custody and original observations", { exact: true })
      .click();
    await page
      .getByRole("button", { name: "Refresh serial review", exact: true })
      .click();
    if (fault === "trace-source") {
      await expect(page.getByRole("status")).toContainText(
        "Original source disagreement",
      );
      return;
    }
    await expect(page.getByRole("status")).toContainText(
      "1 of 2 retained records",
    );
    await page
      .getByRole("button", { name: "Load next serial page", exact: true })
      .click();
    if (fault === "trace-stale") {
      await expect(page.getByRole("alert").first()).toContainText(
        "Global SKU identity changed",
      );
      await expect(page.getByText("Unit-A", { exact: true })).toBeHidden();
    } else {
      await expect(page.getByRole("status")).toContainText(
        "2 of 2 retained records",
      );
      await expect(page.getByText("unit-a", { exact: true })).toBeVisible();
    }
  });

test("an oversized complete request retains all scanned labels and sends no physical save", async ({
  page,
}) => {
  test.setTimeout(60000);
  const s = await setup(page);
  await count(page, 1100);
  const labels = Array.from(
    { length: 1100 },
    (_, i) => `${String(i).padStart(4, "0")}${"x".repeat(235)}`,
  );
  // A burst of keyboard-wedge Enter events also verifies that batched React
  // renders cannot drop or duplicate observations. Each label enters the DOM
  // field and its real key handler; no component state is modified by the test.
  await page
    .getByRole("textbox", { name: `Scan serial for ${sku}`, exact: true })
    .evaluate((node, values) => {
      for (const value of values) {
        (node as HTMLInputElement).value = value;
        node.dispatchEvent(
          new KeyboardEvent("keydown", {
            key: "Enter",
            bubbles: true,
            cancelable: true,
          }),
        );
      }
    }, labels);
  await page
    .getByRole("button", { name: "Save with observed serials", exact: true })
    .click();
  await expect(
    page.getByRole("dialog", { name: "Observe serial labels" }),
  ).toBeVisible();
  await expect(page.getByRole("alert")).toContainText(
    "complete original request exceeds one save",
  );
  await expect(
    page.getByText("1100 of 1100 serials observed", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText(labels[1099], { exact: true })).toBeVisible();
  for (let n = 0; n < 21; n++)
    await page
      .getByRole("button", { name: "Previous labels", exact: true })
      .click();
  await expect(page.getByText(labels[0], { exact: true })).toBeVisible();
  expect(s.requests).toHaveLength(0);
});
