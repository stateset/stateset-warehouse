// Browser contract checks for operator review and recovery. Native HTTP/SQL
// behavior is separately exercised by tests/native_api.rs against PostgreSQL.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), "..");
async function setup(page: Page, mode: "import" | "transport") {
  const fixture = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    { encoding: "utf8" },
  );
  if (fixture.status !== 0) throw new Error(fixture.stderr);
  const state = JSON.parse(fixture.stdout);
  const packed = state.orders.find((order: any) => order.status === "packed");
  packed.source_order_id = "oms:acme:00000000-0000-4000-8000-000000000999";
  const source = {
    id: "00000000-0000-4000-8000-000000000999",
    orderNumber: "OMS-501",
    status: "processing",
    updatedAt: "2026-10-17T10:00:00Z",
    lineItems: [{ title: "Verified goods", sku: "SS-TEE-001", quantity: 2 }],
  };
  const submissions: any[] = [];
  let receipt: any = { found: false };
  let rejectRetries = false;
  await page.route("https://warehouse.test/**", async (route) => {
    const url = new URL(route.request().url());
    const send = (value: unknown) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(value),
      });
    if (url.pathname === "/v1/session")
      return send({
        identity: { tenant: "acme", actor: "operator:alice", role: "admin" },
        demo: false,
        integrations: [],
        native_integrations: {
          oms: { configured: true },
          tms: { configured: true },
        },
      });
    if (url.pathname === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
      });
    if (url.pathname === "/v1/warehouses/wh-pdx") return send(state);
    if (url.pathname.endsWith("/outbox")) return send({ items: [] });
    if (url.pathname === "/v1/integrations/oms/orders")
      return send({ items: [source], pagination: { page: 1, hasNext: false } });
    if (url.pathname.includes("/native-requests/")) return send(receipt);
    if (url.pathname.endsWith("/readiness"))
      return send({
        readiness: {
          order_id: packed.id,
          state: "packed",
          lines: packed.lines,
        },
        oms_writeback: { state: "held" },
      });
    if (url.pathname.endsWith("/imports/oms")) {
      submissions.push(route.request().postDataJSON());
      receipt = {
        found: true,
        request_id: submissions[0].request_id,
        status: "completed",
        attempts: 0,
      };
      if (mode === "import" && submissions.length === 1)
        return route.abort("failed");
      return send(receipt);
    }
    if (url.pathname.endsWith("/transport")) {
      submissions.push(route.request().postDataJSON());
      if (rejectRetries && submissions.length > 1)
        return route.fulfill({
          status: 403,
          contentType: "application/json",
          body: '{"error":"Permissions changed; original outcome remains uncertain"}',
        });
      receipt = {
        found: true,
        request_id: submissions[0].request_id,
        status: submissions.length === 1 ? "uncertain" : "completed",
        attempts: submissions.length,
        last_error: submissions.length === 1 ? "Remote response lost" : "",
      };
      return send(receipt);
    }
    if (url.pathname.startsWith("/v1/"))
      return route.fulfill({
        status: 404,
        contentType: "application/json",
        body: '{"error":"Unhandled contract route"}',
      });
    const file = resolve(
      root,
      "dashboard/dist",
      url.pathname === "/" ? "index.html" : url.pathname.slice(1),
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
  await page
    .getByLabel("API credential", { exact: true })
    .fill("fixture-credential");
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await page.getByRole("button", { name: "Integrations", exact: true }).click();
  return {
    submissions,
    source,
    packed,
    version: state.version,
    advanceVersion: () => ++state.version,
    denyRetries: () => {
      rejectRetries = true;
    },
  };
}

async function reviewTransportPlan(page: Page, orderId: string) {
  await page
    .getByRole("combobox", { name: "Packed order", exact: true })
    .selectOption(orderId);
  for (const [label, value] of [
    ["Transport reference", "FR-501"],
    ["Freight customer ID", "customer-real"],
    ["Commodity", "Reviewed goods"],
    ["Weight (lb)", "1200"],
    ["Miles", "180"],
    ["Buy (USD cents)", "100000"],
    ["Sell (USD cents)", "125000"],
  ])
    await page.getByLabel(label, { exact: true }).fill(value);
  for (const [name, city, region, date, postal] of [
    ["Pickup", "Portland", "OR", "2026-10-20", "97201"],
    ["Delivery", "Seattle", "WA", "2026-10-21", "98101"],
  ]) {
    const stop = page.getByRole("group", { name, exact: true });
    for (const [label, value] of [
      ["Facility", `${name} facility`],
      ["City", city],
      ["State / province code", region],
      ["Postal code", postal],
      ["Country code", "US"],
      ["Date", date],
      ["Street address (one or two lines)", "123 Verified Avenue"],
    ])
      await stop.getByLabel(label, { exact: true }).fill(value);
  }
  await expect(
    page.getByRole("button", {
      name: "Create reviewed planned load",
      exact: true,
    }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Review transport plan", exact: true })
    .click();
  await expect(
    page.getByText("Confirm planned load", { exact: true }),
  ).toBeVisible();
}
test("native import preserves reviewed source revision and recovers a lost response without resubmission", async ({
  page,
}) => {
  const fixture = await setup(page, "import");
  await page
    .getByRole("button", { name: "Load OMS orders", exact: true })
    .click();
  await page.getByRole("button", { name: /OMS-501/ }).click();
  await page.getByLabel("Customer", { exact: true }).fill("Verified customer");
  await page
    .getByLabel("Destination", { exact: true })
    .fill("Reviewed destination");
  await page
    .getByRole("button", { name: "Import reviewed OMS order", exact: true })
    .click();
  await expect(
    page.getByText("Original integration request awaiting confirmation"),
  ).toBeVisible();
  expect(fixture.submissions).toHaveLength(1);
  expect(fixture.submissions[0]).toMatchObject({
    expected_version: fixture.version,
    source_updated_at: fixture.source.updatedAt,
    oms_order_id: fixture.source.id,
    details: {
      customer: "Verified customer",
      destination: "Reviewed destination",
      priority: "normal",
    },
  });
  await page.reload();
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "New order", exact: true }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "Integrations", exact: true }).click();
  await page
    .getByRole("button", {
      name: "Check original integration request",
      exact: true,
    })
    .click();
  await expect(
    page.getByText("Original integration request confirmed.", { exact: true }),
  ).toBeVisible();
  expect(fixture.submissions).toHaveLength(1);
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "New order", exact: true }),
  ).toBeEnabled();
});
test("transport requires explicit plan review and retries the original frozen request after reload", async ({
  page,
}) => {
  const fixture = await setup(page, "transport");
  await reviewTransportPlan(page, fixture.packed.id);
  await page
    .getByRole("button", { name: "Create reviewed planned load", exact: true })
    .click();
  await expect(
    page.getByText("Original integration request awaiting confirmation"),
  ).toBeVisible();
  expect(fixture.submissions[0].plan).toMatchObject({
    buy_cents: 100000,
    sell_cents: 125000,
    weight_lbs: 1200,
    origin: { city: "Portland", date: "2026-10-20" },
    destination: { city: "Seattle", date: "2026-10-21" },
  });
  await page.reload();
  await page
    .getByRole("button", {
      name: "Retry original integration request",
      exact: true,
    })
    .click();
  await expect(
    page.getByText(
      "Integration saved. Original request and source evidence are retained.",
      { exact: true },
    ),
  ).toBeVisible();
  expect(fixture.submissions).toHaveLength(2);
  expect(fixture.submissions[1]).toEqual(fixture.submissions[0]);
});

test("reviewing transportation cannot freshen a stale OMS import form", async ({
  page,
}) => {
  const fixture = await setup(page, "import");
  await page
    .getByRole("button", { name: "Load OMS orders", exact: true })
    .click();
  await page.getByRole("button", { name: /OMS-501/ }).click();
  await page
    .getByLabel("Customer", { exact: true })
    .fill("Customer reviewed at the original version");
  await page
    .getByLabel("Destination", { exact: true })
    .fill("Destination reviewed at the original version");
  const changedVersion = fixture.advanceVersion();
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await expect(page.locator(".page-footer")).toContainText(
    `Version ${changedVersion}`,
  );
  await page
    .getByRole("combobox", { name: "Packed order", exact: true })
    .selectOption(fixture.packed.id);
  await page
    .getByRole("button", { name: "Import reviewed OMS order", exact: true })
    .click();
  await expect(
    page.getByText("Original integration request awaiting confirmation"),
  ).toBeVisible();
  expect(fixture.submissions).toHaveLength(1);
  expect(fixture.submissions[0].expected_version).toBe(fixture.version);
  expect(fixture.submissions[0].expected_version).not.toBe(changedVersion);
  expect(fixture.submissions[0].source_updated_at).toBe(
    fixture.source.updatedAt,
  );
});

test("denied recovery retry retains an uncertain transport journal across reload", async ({
  page,
}) => {
  const fixture = await setup(page, "transport");
  await reviewTransportPlan(page, fixture.packed.id);
  await page
    .getByRole("button", { name: "Create reviewed planned load", exact: true })
    .click();
  await expect(
    page.getByText("Original integration request awaiting confirmation"),
  ).toBeVisible();
  const saved = await page.evaluate(() =>
    sessionStorage.getItem("wms-native:acme:operator:alice"),
  );
  expect(saved).not.toBeNull();
  fixture.denyRetries();
  await page
    .getByRole("button", {
      name: "Retry original integration request",
      exact: true,
    })
    .click();
  await expect(page.getByRole("alert")).toContainText(
    "Original request retained",
  );
  expect(fixture.submissions).toHaveLength(2);
  expect(fixture.submissions[1]).toEqual(fixture.submissions[0]);
  expect(
    await page.evaluate(() =>
      sessionStorage.getItem("wms-native:acme:operator:alice"),
    ),
  ).toBe(saved);
  await page.reload();
  await expect(
    page.getByText("Original integration request awaiting confirmation"),
  ).toBeVisible();
  await page
    .getByRole("button", {
      name: "Check original integration request",
      exact: true,
    })
    .click();
  await expect(
    page.getByText(
      "Original intent retained. Retry uses its exact frozen payload.",
      { exact: true },
    ),
  ).toBeVisible();
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "New order", exact: true }),
  ).toBeDisabled();
  expect(fixture.submissions).toHaveLength(2);
});
