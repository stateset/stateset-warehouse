// Intercepted read-side UI contracts; units-live.spec.ts covers actual HTTP/PG.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), "..");
async function setup(
  page: Page,
  options: {
    role?: string;
    fault?: "source" | "scope" | "incomplete" | "stale";
  } = {},
) {
  const r = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    { encoding: "utf8" },
  );
  if (r.status !== 0) throw Error(r.stderr);
  const state = JSON.parse(r.stdout),
    sku = state.products[0].sku,
    submissions: any[] = [];
  await page.route("https://warehouse.test/**", async (route) => {
    const url = new URL(route.request().url()),
      path = url.pathname;
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
          tenant: "demo",
          actor: "unit:reader",
          role: options.role ?? "admin",
        },
        demo: true,
        integrations: [],
      });
    if (path === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
      });
    if (path.endsWith("/outbox")) return send({ items: [] });
    if (path.endsWith("/unit-packs")) {
      if (options.fault === "stale")
        return send(
          { error: "Warehouse changed; explicitly refresh the review" },
          409,
        );
      const history = url.searchParams.get("history") === "true";
      const items = [
        {
          id: "original",
          sku,
          code: "CASE",
          label: "Original case",
          each_per_pack: 12,
          enabled: true,
          actor: "synthetic:controller",
          version: 1,
          source_ok: options.fault !== "source",
        },
      ];
      return send({
        warehouse_id: state.warehouse.id,
        sku,
        code: history ? null : url.searchParams.get("code"),
        version: state.version,
        view: history ? "history" : "current",
        items:
          options.fault === "scope"
            ? [{ ...items[0], sku: "OTHER-SKU" }]
            : items,
        total_records: options.fault === "incomplete" ? 2 : 1,
        next_cursor: null,
        page_source_ok: options.fault !== "source",
      });
    }
    if (path.endsWith("/commands")) {
      submissions.push(route.request().postDataJSON());
      return send({ error: "Unexpected write" }, 422);
    }
    if (path === `/v1/warehouses/${state.warehouse.id}`)
      return send({ ...state, reconciliation: { ok: true, rows: [] } });
    if (path.startsWith("/v1/")) return send({ error: "Unhandled read" }, 404);
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
  await page
    .getByRole("button", { name: "Open synthetic demo", exact: true })
    .click();
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  await page
    .locator("details")
    .filter({ has: page.locator("summary", { hasText: /^Pack units$/ }) })
    .locator("summary")
    .click();
  return {
    panel: page.getByRole("region", { name: "Pack units", exact: true }),
    submissions,
  };
}
for (const fault of ["source", "scope", "incomplete", "stale"] as const)
  test(`pack change holds an unverified ${fault} definition review`, async ({
    page,
  }) => {
    const { panel, submissions } = await setup(page, { fault });
    await panel
      .getByLabel("Pack evidence / reason", { exact: true })
      .fill("Reviewed physical case label");
    await panel
      .getByRole("button", { name: "Review pack change", exact: true })
      .click();
    await expect(panel.getByRole("alert")).toBeVisible();
    await expect(
      panel.getByRole("button", {
        name: "Confirm pack definition",
        exact: true,
      }),
    ).toHaveCount(0);
    expect(submissions).toHaveLength(0);
  });
test("viewers can inspect original pack history without metadata controls", async ({
  page,
}) => {
  const { panel, submissions } = await setup(page, { role: "viewer" });
  await expect(
    panel.getByRole("button", { name: "Review pack change", exact: true }),
  ).toHaveCount(0);
  await panel
    .getByRole("button", { name: "Review pack history", exact: true })
    .click();
  await expect(
    panel.getByText(/1 of 1 original history records/),
  ).toBeVisible();
  await expect(panel.getByRole("listitem")).toContainText("12 each");
  expect(submissions).toHaveLength(0);
});

test("pack code form validation uses Chrome's current pattern rules and reserves base each", async ({
  page,
}) => {
  const { panel, submissions } = await setup(page);
  const code = panel.getByLabel("Pack code", { exact: true });
  for (const value of ["lowercase", "CASE/12", "CASE SPACE"]) {
    await code.fill(value);
    expect(
      await code.evaluate((e) => (e as HTMLInputElement).checkValidity()),
    ).toBe(false);
  }
  await code.fill("EA");
  await expect(
    panel.getByRole("button", { name: "Review pack change", exact: true }),
  ).toBeDisabled();
  await code.fill("CASE_12-OUTER");
  expect(
    await code.evaluate((e) => (e as HTMLInputElement).checkValidity()),
  ).toBe(true);
  await expect(
    panel.getByRole("button", { name: "Review pack change", exact: true }),
  ).toBeEnabled();
  expect(submissions).toHaveLength(0);
});
