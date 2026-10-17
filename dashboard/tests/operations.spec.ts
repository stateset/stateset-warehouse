import { expect, test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), "..");
function snapshot() {
  return {
    schema_version: 1,
    snapshot_at: new Date().toISOString(),
    outbox: {
      total: 0,
      pending: 0,
      delivering: 0,
      delivered: 0,
      dead_letter: 0,
      due: 0,
      active_leases: 0,
      expired_leases: 0,
      oldest_pending_age_seconds: 0,
      receivers: [
        { destination: "oms", configured: false },
        { destination: "tms", configured: true },
      ],
      groups: ["oms", "tms"].flatMap((destination) =>
        ["pending", "delivering", "delivered", "dead_letter"].map((status) => ({
          destination,
          status,
          count: 0,
          due: 0,
          active_leases: 0,
          expired_leases: 0,
          oldest_age_seconds: 0,
          oldest_due_age_seconds: 0,
        })),
      ),
    },
    native: {
      total: 0,
      requires_review: 0,
      uncertain: 0,
      rejected: 0,
      local_conflict: 0,
      groups: ["oms_import", "tms_transport", "oms_fulfillment"].flatMap(
        (kind) =>
          [
            "frozen",
            "sending",
            "uncertain",
            "completed",
            "local_conflict",
            "rejected",
          ].map((status) => ({
            kind,
            status,
            count: 0,
            active_leases: 0,
            expired_leases: 0,
            oldest_age_seconds: 0,
            oldest_updated_age_seconds: 0,
          })),
      ),
    },
    warehouses: {
      total: 1,
      open_exceptions: 0,
      items: [{ warehouse_id: "wh-pdx", version: 21, open_exceptions: 0 }],
      next_after: null as string | null,
    },
  };
}
async function setup(page: Page, role = "admin") {
  const output = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    { encoding: "utf8" },
  );
  if (output.status !== 0) throw new Error(output.stderr);
  const state = JSON.parse(output.stdout);
  let current = snapshot();
  let fail = false;
  let invalid = false;
  const reads: string[] = [];
  const writes: string[] = [];
  await page.route("https://warehouse.test/**", async (route) => {
    const url = new URL(route.request().url());
    const send = (data: unknown) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(data),
      });
    if (route.request().method() !== "GET") writes.push(url.pathname);
    if (url.pathname === "/v1/session")
      return send({
        identity: { tenant: "acme", actor: "operator:alice", role },
        demo: false,
        // Deliberately stale session config: the current operations snapshot wins.
        integrations: [
          { id: "oms", configured: true },
          { id: "tms", configured: false },
        ],
      });
    if (url.pathname === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
      });
    if (url.pathname === "/v1/warehouses/wh-pdx") return send(state);
    if (url.pathname.endsWith("/outbox")) return send({ items: [] });
    if (url.pathname === "/v1/operations/health") {
      reads.push(url.search);
      if (fail)
        return route.fulfill({
          status: 503,
          contentType: "application/json",
          body: JSON.stringify({ error: "Database is recovering" }),
        });
      if (invalid)
        return send({ ...current, native: { ...current.native, groups: [] } });
      if (url.searchParams.has("after"))
        return send({
          ...current,
          snapshot_at: new Date().toISOString(),
          warehouses: {
            total: 2,
            open_exceptions: 0,
            items: [
              { warehouse_id: "wh-zulu", version: 34, open_exceptions: 0 },
            ],
            next_after: null,
          },
        });
      return send(current);
    }
    if (url.pathname.startsWith("/assets/"))
      return route.fulfill({
        body: readFileSync(
          resolve(root, "dashboard/dist", url.pathname.slice(1)),
        ),
        contentType: url.pathname.endsWith(".css")
          ? "text/css"
          : "application/javascript",
      });
    return route.fulfill({
      contentType: "text/html",
      body: readFileSync(resolve(root, "dashboard/dist/index.html")),
    });
  });
  await page.addInitScript(() =>
    sessionStorage.setItem("wms-token", "fixture-key"),
  );
  await page.goto("https://warehouse.test/");
  await expect(
    page.getByRole("heading", { name: "Warehouse overview", exact: true }),
  ).toBeVisible();
  return {
    reads,
    writes,
    set: (value: ReturnType<typeof snapshot>) => {
      current = value;
    },
    fail: () => {
      fail = true;
    },
    invalid: () => {
      invalid = true;
    },
  };
}

test("operational status separates held events and immutable rejection history from active incidents on mobile", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const fixture = await setup(page);
  const state = snapshot();
  state.outbox.total = state.outbox.pending = state.outbox.due = 701;
  Object.assign(state.outbox.groups[0], {
    count: 701,
    due: 701,
    oldest_age_seconds: 3600,
    oldest_due_age_seconds: 3600,
  });
  state.native.total = state.native.requires_review = state.native.rejected = 3;
  Object.assign(
    state.native.groups.find(
      (g) => g.kind === "oms_fulfillment" && g.status === "rejected",
    )!,
    { count: 3, oldest_age_seconds: 7200, oldest_updated_age_seconds: 3600 },
  );
  fixture.set(state);
  await page.getByRole("button", { name: "Operations", exact: true }).click();
  await expect(
    page.getByText("No active review conditions detected", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Receiver not configured", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Events are retained without external delivery.", {
      exact: false,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("row").filter({ hasText: "oms fulfillment" }),
  ).toContainText("rejected");
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  expect(fixture.writes).toEqual([]);
});

test("uncertain requests remain actionable and a failed refresh retains explicitly stale data", async ({
  page,
}) => {
  const fixture = await setup(page);
  const state = snapshot();
  state.native.total =
    state.native.uncertain =
    state.native.requires_review =
      1;
  state.native.groups.find(
    (g) => g.kind === "tms_transport" && g.status === "uncertain",
  )!.count = 1;
  fixture.set(state);
  await page.getByRole("button", { name: "Operations", exact: true }).click();
  await expect(
    page.getByText("Operational review needed", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Download snapshot", exact: true }),
  ).toBeEnabled();
  fixture.fail();
  await page
    .getByRole("button", { name: "Refresh status", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("Database is recovering");
  await expect(
    page.getByText("Status unavailable — retained snapshot", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Download snapshot", exact: true }),
  ).toBeDisabled();
  await expect(
    page.getByRole("row").filter({ hasText: "tms transport" }),
  ).toContainText("uncertain");
  await page
    .getByRole("button", { name: "Open integration recovery", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Integrations", exact: true }),
  ).toBeVisible();
  expect(fixture.writes).toEqual([]);
});

test("warehouse status pagination uses independent bounded snapshots and rejects incomplete status", async ({
  page,
}) => {
  const fixture = await setup(page);
  const state = snapshot();
  state.warehouses.total = 2;
  state.warehouses.next_after = "wh-pdx";
  fixture.set(state);
  await page.getByRole("button", { name: "Operations", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Next warehouses", exact: true }),
  ).toBeEnabled();
  await page
    .getByRole("button", { name: "Next warehouses", exact: true })
    .click();
  await expect(
    page.getByRole("cell", { name: "wh-zulu", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("cell", { name: "wh-pdx", exact: true }),
  ).toHaveCount(0);
  expect(fixture.reads).toEqual(["?limit=25", "?limit=25&after=wh-pdx"]);
  await page
    .getByRole("button", { name: "Previous warehouses", exact: true })
    .click();
  await expect(
    page.getByRole("cell", { name: "wh-pdx", exact: true }),
  ).toBeVisible();
  fixture.invalid();
  await page
    .getByRole("button", { name: "Refresh status", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("snapshot is incomplete");
  await expect(
    page.getByText("Status unavailable — retained snapshot", { exact: true }),
  ).toBeVisible();
});

test("viewer cannot open operational data even through a saved direct URL", async ({
  page,
}) => {
  const fixture = await setup(page, "viewer");
  await expect(
    page.getByRole("button", { name: "Operations", exact: true }),
  ).toHaveCount(0);
  await page.goto("https://warehouse.test/#operations");
  await expect(page.getByRole("alert")).toContainText(
    "operator or administrator",
  );
  expect(fixture.reads).toEqual([]);
});

test("offline and elapsed reads never remain green or exportable, and reconnect refreshes", async ({
  page,
  context,
}) => {
  await page.clock.install({ time: Date.now() });
  const fixture = await setup(page);
  await page.getByRole("button", { name: "Operations", exact: true }).click();
  await expect(
    page.getByText("No active review conditions detected", { exact: true }),
  ).toBeVisible();
  await context.setOffline(true);
  await expect(page.getByRole("alert")).toContainText("browser is offline");
  await expect(
    page.getByRole("button", { name: "Download snapshot", exact: true }),
  ).toBeDisabled();
  await context.setOffline(false);
  await expect(
    page.getByText("No active review conditions detected", { exact: true }),
  ).toBeVisible();
  await page.evaluate(() =>
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "hidden",
    }),
  );
  await page.clock.runFor(61000);
  await expect(page.getByRole("alert")).toContainText("over 60 seconds old");
  await expect(
    page.getByRole("button", { name: "Download snapshot", exact: true }),
  ).toBeDisabled();
  expect(fixture.writes).toEqual([]);
});

test("old or future server timestamps cannot become a healthy snapshot", async ({
  page,
}) => {
  const fixture = await setup(page);
  const old = snapshot();
  old.snapshot_at = new Date(Date.now() - 300000).toISOString();
  fixture.set(old);
  await page.getByRole("button", { name: "Operations", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("clock window");
  await expect(
    page.getByText("No active review conditions detected", { exact: true }),
  ).toHaveCount(0);
  const future = snapshot();
  future.snapshot_at = new Date(Date.now() + 300000).toISOString();
  fixture.set(future);
  await page
    .getByRole("button", { name: "Refresh status", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("clock window");
  await expect(
    page.getByRole("button", { name: "Download snapshot", exact: true }),
  ).toBeDisabled();
});
