// Built-browser/domain contracts with intercepted HTTP. Actual database and
// browser/API checks separately qualify facility creation and scope isolation.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), "..");
const identity = {
  tenant: "facility-tenant",
  actor: "operator:facility",
  role: "admin",
};
const selectionKey = `wms-warehouse:${identity.tenant}:${identity.actor}`;
const journalKey = `wms-pending:${identity.tenant}:${identity.actor}`;
const nativeKey = `wms-native:${identity.tenant}:${identity.actor}`;
function domain(input?: unknown) {
  const output = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    {
      input: input ? JSON.stringify(input) : "",
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    },
  );
  if (output.status !== 0) throw new Error(output.stderr);
  return JSON.parse(output.stdout);
}
async function fixture(
  page: Page,
  options: {
    role?: string;
    loss?: "before" | "after";
    pages?: boolean | number;
    fresh?: boolean;
    pending?: "physical" | "native";
    wrongSnapshot?: boolean;
  } = {},
) {
  const first = domain();
  const blank = (id: string, code: string) => ({
    ...first,
    warehouse: {
      id,
      code,
      name: code + " facility",
      city: "Test city",
      timezone: "UTC",
    },
    version: 0,
    products: [],
    stocks: [],
    locations: first.locations.filter((l: any) => l.kind !== "storage"),
    inventory_baseline: [],
    receipt_baseline: {},
    receipts: [],
    orders: [],
    movements: [],
    exceptions: [],
    activity: [],
    resource_versions: {},
  });
  const states = new Map<string, any>([
    [first.warehouse.id, first],
    ["wh-zulu", blank("wh-zulu", "ZULU")],
  ]);
  if (options.pages)
    for (
      let i = 0;
      i < (typeof options.pages === "number" ? options.pages : 52);
      i++
    )
      states.set(
        `wh-a${String(i).padStart(2, "0")}`,
        blank(`wh-a${String(i).padStart(2, "0")}`, `A${i}`),
      );
  const posts: { path: string; body: any }[] = [];
  const receipts = new Map<string, any>();
  const pages: string[] = [];
  let lost = false;
  let hold = false;
  let heldStarted = false;
  let release: (() => void) | undefined;
  await page.addInitScript(
    ({ selectionKey, journalKey, nativeKey, pending, fresh }) => {
      if (!fresh && !sessionStorage.getItem(selectionKey))
        sessionStorage.setItem(selectionKey, "wh-pdx");
      if (pending === "physical" && !sessionStorage.getItem(journalKey))
        sessionStorage.setItem(
          journalKey,
          JSON.stringify({
            tenant: "facility-tenant",
            actor: "operator:facility",
            warehouse: "wh-zulu",
            request: {
              request_id: "retained-other-warehouse",
              expected_version: 0,
              command: {
                type: "report_exception",
                order_id: null,
                category: "Other",
                detail: "Original retained facility observation",
              },
            },
          }),
        );
      if (pending === "native" && !sessionStorage.getItem(nativeKey))
        sessionStorage.setItem(
          nativeKey,
          JSON.stringify({
            warehouse: "wh-zulu",
            path: "/v1/warehouses/wh-zulu/imports/oms",
            body: {
              request_id: "retained-native-warehouse",
              oms_order_id: "source-original",
            },
          }),
        );
    },
    {
      selectionKey,
      journalKey,
      nativeKey,
      pending: options.pending,
      fresh: options.fresh,
    },
  );
  await page.route("https://warehouse.test/**", async (route) => {
    const url = new URL(route.request().url());
    const send = (value: unknown, status = 200) =>
      route.fulfill({
        status,
        contentType: "application/json",
        body: JSON.stringify(value),
      });
    if (url.pathname === "/auth/config") return send({ provider: "api_key" });
    if (url.pathname === "/v1/session")
      return send({
        identity: { ...identity, role: options.role ?? "admin" },
        default_warehouse_id: "wh-pdx",
        demo: false,
        integrations: [],
        native_integrations: {
          oms: { configured: false },
          tms: { configured: false },
        },
      });
    if (url.pathname === "/v1/warehouses") {
      pages.push(url.search);
      const ids = [...states.keys()]
        .sort()
        .filter((id) => id > (url.searchParams.get("after") ?? ""));
      const limit = Number(url.searchParams.get("limit") ?? 100);
      const visible = ids.slice(0, limit);
      return send({
        warehouses: visible.map((id) => ({
          warehouse: states.get(id).warehouse,
          version: states.get(id).version,
        })),
        next_after: ids.length > limit ? visible.at(-1) : null,
      });
    }
    const command = url.pathname.match(
      /^\/v1\/warehouses\/([^/]+)\/commands(?:\/([^/]+))?$/,
    );
    if (command) {
      const warehouse = decodeURIComponent(command[1]);
      if (command[2]) {
        const saved = receipts.get(`${warehouse}:${command[2]}`);
        return send(
          saved ? { found: true, result: saved.result } : { found: false },
        );
      }
      const body = route.request().postDataJSON();
      posts.push({ path: url.pathname, body });
      if (
        options.loss === "before" &&
        body.command.type === "create_warehouse" &&
        !lost
      ) {
        lost = true;
        return route.abort("failed");
      }
      const key = `${warehouse}:${body.request_id}`;
      const saved = receipts.get(key);
      if (saved)
        return send(
          JSON.stringify(saved.request) === JSON.stringify(body)
            ? { replayed: true, result: saved.result }
            : { error: "Request changed" },
          JSON.stringify(saved.request) === JSON.stringify(body) ? 200 : 409,
        );
      let state = states.get(warehouse);
      if (body.command.type === "create_warehouse") {
        if (state) return send({ error: "Warehouse exists" }, 409);
        state = blank(warehouse, body.command.code);
      }
      if (!state) return send({ error: "Unknown warehouse" }, 404);
      if (body.expected_version !== state.version)
        return send({ error: "Warehouse changed" }, 409);
      const result = domain({ state, command: body.command });
      if (result.error) return send({ error: result.error }, 422);
      states.set(warehouse, result.state);
      receipts.set(key, { request: body, result: result.result });
      if (
        options.loss === "after" &&
        body.command.type === "create_warehouse" &&
        !lost
      ) {
        lost = true;
        return route.abort("failed");
      }
      return send({ replayed: false, result: result.result });
    }
    const scope = url.pathname.match(/^\/v1\/warehouses\/([^/]+)(.*)$/);
    if (scope) {
      const state = states.get(decodeURIComponent(scope[1]));
      if (scope[2] === "/outbox") return send({ items: [] });
      if (!state) return send({ error: "Warehouse not found" }, 404);
      if (!scope[2]) {
        const displayed =
          options.wrongSnapshot && state.warehouse.id === "wh-zulu"
            ? first
            : state;
        return send({
          ...displayed,
          reconciliation: domain({ state: displayed }),
        });
      }
      const detail = scope[2].match(/^\/(orders|receipts)\/([^/]+)$/);
      if (detail) {
        const record = state[detail[1]].find(
          (row: any) => row.id === decodeURIComponent(detail[2]),
        );
        if (hold) {
          heldStarted = true;
          await new Promise<void>((done) => {
            release = done;
          });
          await send(record).catch(() => {});
          return;
        }
        return record ? send(record) : send({ error: "Record missing" }, 404);
      }
      if (scope[2].startsWith("/history/"))
        return send({ items: [], next_cursor: null });
      if (scope[2] === "/reconciliation") return send(domain({ state }));
    }
    if (url.pathname.startsWith("/v1/"))
      return send({ error: "Unhandled fixture API" }, 404);
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
  await page.route("https://fonts.googleapis.com/**", (route) => route.abort());
  await page.route("https://fonts.gstatic.com/**", (route) => route.abort());
  await page.goto("https://warehouse.test/");
  await page
    .getByLabel("API credential", { exact: true })
    .fill("facility-test-credential");
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Warehouse overview", exact: true }),
  ).toBeVisible();
  return {
    states,
    posts,
    pages,
    first,
    holdDetails: () => {
      hold = true;
    },
    detailsHeld: () => heldStarted,
    releaseDetails: () => {
      hold = false;
      release?.();
    },
  };
}
async function picker(page: Page) {
  await page
    .getByRole("button", { name: "Change warehouse", exact: true })
    .click();
  await expect(
    page.getByLabel("Active warehouse", { exact: true }),
  ).toBeEnabled();
}
async function create(page: Page) {
  await picker(page);
  await page
    .getByRole("button", { name: "New warehouse", exact: true })
    .click();
  await page.getByLabel("Warehouse code", { exact: true }).fill("EAST");
  await page
    .getByLabel("Warehouse name", { exact: true })
    .fill("East facility");
  await page.getByLabel("City", { exact: true }).fill("Toronto");
  await page
    .getByLabel("IANA timezone", { exact: true })
    .fill("America/Toronto");
  await page
    .getByRole("button", { name: "Create warehouse", exact: true })
    .click();
}

test("facility creation switches to empty stock and selected warehouse survives reload", async ({
  page,
}) => {
  const f = await fixture(page);
  const original = JSON.stringify(f.first);
  await create(page);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.locator(".workspace-card strong")).toHaveText("EAST");
  const creation = f.posts[0];
  expect(creation.body.expected_version).toBe(0);
  expect(creation.body.resource_versions).toBeUndefined();
  const id = creation.path.split("/")[3];
  expect(id).not.toBe("wh-pdx");
  expect(f.states.get(id).stocks).toEqual([]);
  expect(JSON.stringify(f.states.get("wh-pdx"))).toBe(original);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.locator(".warehouse-picker-trigger")).toBeVisible();
  await expect(page.locator(".warehouse-picker-trigger")).toContainText("EAST");
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.reload();
  await expect(page.locator(".workspace-card strong")).toHaveText("EAST");
  await picker(page);
  await page
    .getByLabel("Active warehouse", { exact: true })
    .selectOption("wh-pdx");
  await expect(page.locator(".workspace-card strong")).toHaveText(
    f.first.warehouse.code,
  );
  await page.reload();
  await expect(page.locator(".workspace-card strong")).toHaveText(
    f.first.warehouse.code,
  );
});
for (const loss of ["before", "after"] as const)
  test(`lost facility creation ${loss} arrival preserves one original route and envelope`, async ({
    page,
  }) => {
    const f = await fixture(page, { loss });
    await create(page);
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeVisible();
    const original = await page.evaluate(
      (key) => sessionStorage.getItem(key),
      journalKey,
    );
    await expect(
      page.getByRole("button", { name: "Change warehouse", exact: true }),
    ).toBeDisabled();
    await page.reload();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate((key) => sessionStorage.getItem(key), journalKey),
    ).toBe(original);
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(page.locator(".workspace-card strong")).toHaveText("EAST");
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toHaveCount(0);
    expect(f.states.size).toBe(3);
    for (const post of f.posts) expect(post).toEqual(f.posts[0]);
  });
test("warehouse picker loads later pages and clears old searches and record responses", async ({
  page,
}) => {
  const f = await fixture(page, { pages: true });
  await page.getByRole("button", { name: "Receiving", exact: true }).click();
  await page.getByLabel("Search current view", { exact: true }).fill("RCV");
  f.holdDetails();
  await page
    .getByRole("button", { name: f.first.receipts[0].reference, exact: true })
    .click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await expect.poll(() => f.detailsHeld()).toBe(true);
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  await picker(page);
  await expect(
    page.getByLabel("Active warehouse").locator('option[value="wh-zulu"]'),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Load more warehouses", exact: true })
    .click();
  await expect(
    page.getByLabel("Active warehouse").locator('option[value="wh-zulu"]'),
  ).toHaveCount(1);
  await page
    .getByLabel("Active warehouse", { exact: true })
    .selectOption("wh-zulu");
  await expect(page.locator(".workspace-card strong")).toHaveText("ZULU");
  f.releaseDetails();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "Warehouse overview", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Receiving", exact: true }).click();
  await expect(
    page.getByLabel("Search current view", { exact: true }),
  ).toHaveValue("");
  expect(f.pages.some((p) => p.includes("after="))).toBe(true);
});
for (const pending of ["physical", "native"] as const)
  test(`reload restores the ${pending} request warehouse and prevents switching`, async ({
    page,
  }) => {
    const f = await fixture(page, { pending });
    await expect(page.locator(".workspace-card strong")).toHaveText("ZULU");
    await expect(
      page.getByRole("button", { name: "Change warehouse", exact: true }),
    ).toBeDisabled();
    if (pending === "physical") {
      await page
        .getByRole("button", { name: "Check original save", exact: true })
        .click();
      await expect(
        page.getByRole("button", { name: "Change warehouse", exact: true }),
      ).toBeEnabled();
      expect(f.posts[0].path).toBe("/v1/warehouses/wh-zulu/commands");
      expect(f.posts[0].body.request_id).toBe("retained-other-warehouse");
    } else
      expect(
        await page.evaluate(
          (key) => JSON.parse(sessionStorage.getItem(key)!).warehouse,
          nativeKey,
        ),
      ).toBe("wh-zulu");
  });
for (const role of ["viewer", "operator"])
  test(`${role} can choose a warehouse but cannot create one`, async ({
    page,
  }) => {
    await fixture(page, { role });
    await picker(page);
    await expect(
      page.getByRole("button", { name: "New warehouse", exact: true }),
    ).toHaveCount(0);
    await page
      .getByLabel("Active warehouse", { exact: true })
      .selectOption("wh-zulu");
    await expect(page.locator(".workspace-card strong")).toHaveText("ZULU");
  });
test("a snapshot for the wrong warehouse cannot complete a context switch", async ({
  page,
}) => {
  const f = await fixture(page, { wrongSnapshot: true });
  await picker(page);
  await page
    .getByLabel("Active warehouse", { exact: true })
    .selectOption("wh-zulu");
  await expect(page.getByRole("alert")).toContainText(
    "did not match the selected facility",
  );
  await expect(page.locator(".workspace-card strong")).toHaveText(
    f.first.warehouse.code,
  );
  expect(
    await page.evaluate((key) => sessionStorage.getItem(key), selectionKey),
  ).toBe("wh-pdx");
});

test("first login opens the server default even beyond the directory's first hundred facilities", async ({
  page,
}) => {
  const f = await fixture(page, { pages: 120, fresh: true });
  expect(f.states.size).toBe(122);
  await expect(page.locator(".workspace-card strong")).toHaveText(
    f.first.warehouse.code,
  );
  expect(
    await page.evaluate((key) => sessionStorage.getItem(key), selectionKey),
  ).toBe("wh-pdx");
  expect(f.pages).toEqual([""]);
});

test("native recovery cannot change facility while another original warehouse save remains pending", async ({
  page,
}) => {
  const f = await fixture(page, { pending: "physical" });
  await page.evaluate(
    (key) =>
      sessionStorage.setItem(
        key,
        JSON.stringify({
          warehouse: "wh-pdx",
          path: "/v1/warehouses/wh-pdx/imports/oms",
          body: { request_id: "overlap-native-original" },
        }),
      ),
    nativeKey,
  );
  await page.route(
    "https://warehouse.test/v1/warehouses/wh-pdx/native-requests/overlap-native-original",
    (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          found: true,
          status: "completed",
          request_id: "overlap-native-original",
        }),
      }),
  );
  await page.reload();
  await expect(page.locator(".workspace-card strong")).toHaveText("ZULU");
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
  await expect(page.locator(".workspace-card strong")).toHaveText("ZULU");
  expect(
    await page.evaluate((key) => sessionStorage.getItem(key), journalKey),
  ).not.toBeNull();
  expect(
    await page.evaluate((key) => sessionStorage.getItem(key), nativeKey),
  ).toBeNull();
  await page
    .getByRole("button", { name: "Check original save", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Change warehouse", exact: true }),
  ).toBeEnabled();
  await picker(page);
  await page
    .getByLabel("Active warehouse", { exact: true })
    .selectOption("wh-pdx");
  await expect(page.locator(".workspace-card strong")).toHaveText(
    f.first.warehouse.code,
  );
});
