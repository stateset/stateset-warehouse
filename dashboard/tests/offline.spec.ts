// These exercise the built UI and Rust domain through intercepted requests.
// They do not claim to exercise SQLx, HTTP sockets or actual integrations.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync, readdirSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), "..");
const driver = resolve(root, "target/debug/examples/domain_driver");
function domain(value?: unknown) {
  const r = spawnSync(driver, [], {
    input: value ? JSON.stringify(value) : "",
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  if (r.status !== 0)
    throw new Error(
      r.stderr ||
        "Build the domain driver: cargo build --example domain_driver",
    );
  return JSON.parse(r.stdout);
}
async function setup(page: Page, loseFirstResponse = false) {
  let state = domain();
  const receipts = new Map<string, unknown>();
  let dropped = false;
  await page.route("https://warehouse.test/**", async (route) => {
    const url = new URL(route.request().url());
    const send = (body: unknown, status = 200) =>
      route.fulfill({
        status,
        contentType: "application/json",
        body: JSON.stringify(body),
      });
    if (url.pathname === "/v1/session")
      return send({
        identity: { tenant: "demo", actor: "operator:demo", role: "admin" },
        demo: true,
        integrations: [
          {
            id: "oms",
            name: "StateSet Sync",
            role: "Order management",
            configured: false,
          },
          {
            id: "tms",
            name: "StateSet Freight",
            role: "Transportation management",
            configured: false,
          },
          {
            id: "edi",
            name: "StateSet EDI",
            role: "Document exchange",
            configured: false,
          },
        ],
      });
    if (url.pathname === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
      });
    if (url.pathname.endsWith("/reconciliation"))
      return send(domain({ state }));
    if (url.pathname.endsWith("/outbox")) return send({ items: [] });
    if (url.pathname.match(/\/commands\/.+$/)) {
      const result = receipts.get(url.pathname.split("/").pop()!);
      return send(result ? { found: true, result } : { found: false });
    }
    if (url.pathname.endsWith("/commands")) {
      const req = route.request().postDataJSON();
      if (req.expected_version !== state.version)
        return send({ error: "Warehouse changed. Refresh and review." }, 409);
      const r = domain({ state, command: req.command });
      if (r.error) return send({ error: r.error }, 422);
      state = r.state;
      receipts.set(req.request_id, r.result);
      if (loseFirstResponse && !dropped) {
        dropped = true;
        return route.abort("failed");
      }
      return send({ replayed: false, result: r.result });
    }
    if (url.pathname === "/v1/warehouses/wh-pdx")
      return send({ ...state, reconciliation: domain({ state }) });
    if (url.pathname.startsWith("/v1/"))
      return send({ error: "Unhandled test API route" }, 404);
    const path = url.pathname === "/" ? "index.html" : url.pathname.slice(1);
    const file = resolve(root, "dashboard/dist", path);
    if (!file.startsWith(resolve(root, "dashboard/dist") + "/"))
      return route.abort();
    const contentType = file.endsWith(".js")
      ? "text/javascript"
      : file.endsWith(".css")
        ? "text/css"
        : "text/html";
    return route.fulfill({ contentType, body: readFileSync(file) });
  });
  // Remote font loading is optional and excluded from the offline check.
  await page.route("https://fonts.googleapis.com/**", (r) => r.abort());
  await page.route("https://fonts.gstatic.com/**", (r) => r.abort());
  await page.goto("https://warehouse.test/");
  await page.getByRole("button", { name: "Open synthetic demo" }).click();
  await expect(
    page.getByRole("heading", { name: "Warehouse overview" }),
  ).toBeVisible();
  return () => state;
}

test("overview, filtering, stock and responsive layout", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.setViewportSize({ width: 1440, height: 1040 });
  await setup(page);
  await expect(page.getByText("1,577", { exact: true })).toBeVisible();
  mkdirSync(resolve(root, "docs/screenshots"), { recursive: true });
  await page.screenshot({
    path: resolve(root, "docs/screenshots/desktop.png"),
    fullPage: true,
  });
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  await page
    .getByRole("textbox", { name: "Search current view" })
    .fill("SS-CAP-004");
  await expect(
    page.getByRole("cell", { name: /Five Panel Cap/ }).first(),
  ).toBeVisible();
  await expect(
    page.getByRole("cell", { name: /Everyday Cotton Tee/ }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Overview", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  await page.screenshot({
    path: resolve(root, "docs/screenshots/mobile.png"),
    fullPage: true,
  });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  expect(errors).toEqual([]);
});

test("partial receive and putaway execute the real domain", async ({
  page,
}) => {
  const state = await setup(page);
  await page.getByRole("button", { name: "Receiving", exact: true }).click();
  await page
    .getByRole("row")
    .filter({ hasText: "RCV-207" })
    .getByRole("button", { name: "Receive goods" })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Receive goods" })
    .click();
  await page.getByLabel("Received quantity (each)").fill("12");
  await page.getByRole("button", { name: "Confirm receipt" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(
    state().receipts.find((r: any) => r.reference === "RCV-207").lines[0]
      .received,
  ).toBe(12);
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  const row = page
    .getByRole("row")
    .filter({ hasText: "SS-CAP-004" })
    .filter({ hasText: "RECV-01" })
    .filter({ hasText: "LOT-2611" });
  await row.getByRole("button", { name: "Move", exact: true }).click();
  await page.getByLabel("To storage location").selectOption("B-01-02");
  await page.getByRole("button", { name: "Confirm movement" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(
    state().stocks.find(
      (b: any) =>
        b.sku === "SS-CAP-004" &&
        b.location === "B-01-02" &&
        b.lot === "LOT-2611",
    ).on_hand,
  ).toBe(12);
});

test("fulfillment allocation, pick, packing and physical handoff", async ({
  page,
}) => {
  const state = await setup(page);
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await page.getByRole("button", { name: "SO-1041", exact: true }).click();
  await page.getByRole("button", { name: "Allocate stock" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("button", { name: "SO-1041", exact: true }).click();
  await page.getByRole("button", { name: "Pick units" }).click();
  await page.getByRole("button", { name: "Confirm pick" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("button", { name: "SO-1041", exact: true }).click();
  await page.getByRole("button", { name: "Confirm packed" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("button", { name: "SO-1041", exact: true }).click();
  await page.getByRole("button", { name: "Record handoff" }).click();
  await page.getByLabel("Carrier", { exact: true }).fill("UPS");
  await page
    .getByLabel("Carrier pickup / tracking reference")
    .fill("TEST-PICKUP-001");
  await page.getByRole("button", { name: "Confirm physical handoff" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(
    state().orders.find((o: any) => o.reference === "SO-1041").status,
  ).toBe("shipped");
  expect(state().stocks.reduce((n: number, b: any) => n + b.on_hand, 0)).toBe(
    1553,
  );
  expect(domain({ state: state() }).ok).toBe(true);
});

test("accepted save with a lost response recovers after reload", async ({
  page,
}) => {
  const state = await setup(page, true);
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await page.getByRole("button", { name: "SO-1041", exact: true }).click();
  await page.getByRole("button", { name: "Allocate stock" }).click();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeEnabled();
  const version = state().version;
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Check original save" }).click();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toHaveCount(0);
  expect(state().version).toBe(version);
});

test("a count opened before concurrent work cannot overwrite current stock", async ({
  page,
}) => {
  const state = await setup(page);
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  await page
    .getByRole("row")
    .filter({ hasText: "SS-CAP-004" })
    .filter({ hasText: "B-01-02" })
    .getByRole("button", { name: "Count", exact: true })
    .click();
  await page.getByLabel(/Counted each units/).fill("20");
  await page
    .getByLabel("Count evidence / reason")
    .fill("Observed before another operator moved stock");
  const changed = domain({
    state: state(),
    command: {
      type: "count_stock",
      sku: "SS-CAP-004",
      lot: "LOT-2610",
      location: "B-01-02",
      counted: 27,
      reason: "Concurrent operator count",
    },
  });
  Object.assign(state(), changed.state);
  await page.getByRole("button", { name: "Apply count adjustment" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect(page.getByLabel(/Counted each units/)).toHaveValue("20");
  await expect(page.getByRole("dialog").getByRole("alert")).toContainText(
    "Close and reopen",
  );
  expect(
    state().stocks.find(
      (b: any) => b.sku === "SS-CAP-004" && b.location === "B-01-02",
    ).on_hand,
  ).toBe(27);
});
