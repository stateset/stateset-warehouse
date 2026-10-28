// These exercise the built UI and Rust domain through intercepted requests.
// They do not claim to exercise SQLx, HTTP sockets or actual integrations.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync, readdirSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
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
async function setup(
  page: Page,
  loseFirstResponse = false,
  historyDepth = 0,
  fullHistoryMatches = true,
  recovery?: {
    dropBeforeArrival: boolean;
    scoped?: "related" | "unrelated";
    outcome?: (value: Record<string, unknown>) => unknown;
    responseStatus?: number;
  },
  initialise?: (state: any) => any,
) {
  let state = domain();
  if (initialise) state = initialise(state);
  if (recovery?.scoped) state.resource_versions = {};
  const archivedOrders = Array.from({ length: historyDepth }, (_, index) => ({
    ...state.orders.find((order: any) => order.status === "shipped"),
    id: `archived-order-${index}`,
    reference: `ARCHIVE-${String(index).padStart(3, "0")}`,
    source_order_id: `oms-archive-${index}`,
    customer: "Historical customer",
    created_at: new Date(Date.UTC(2020, 0, index + 1)).toISOString(),
  }));
  const archivedReceipts = Array.from({ length: historyDepth }, (_, index) => ({
    ...state.receipts.find((receipt: any) => receipt.status === "received"),
    id: `archived-receipt-${index}`,
    reference: `ARCHIVE-RCV-${String(index).padStart(3, "0")}`,
    created_at: new Date(Date.UTC(2020, 0, index + 1)).toISOString(),
  }));
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
    if (url.pathname === "/auth/config") return send({ provider: "api_key" });
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
    if (url.pathname.endsWith("/reconciliation")) {
      const proof = domain({ state });
      const receiptProofs = state.receipts.flatMap((receipt: any) =>
        receipt.lines.map((line: any) => ({
          receipt_id: receipt.id,
          line_id: line.id,
          received: line.received,
          evidence: state.movements
            .filter(
              (movement: any) =>
                movement.kind === "receipt" &&
                movement.receipt_id === receipt.id &&
                movement.receipt_line_id === line.id,
            )
            .reduce(
              (total: number, movement: any) => total + movement.quantity,
              0,
            ),
          ok: true,
        })),
      );
      return send({
        ...proof,
        ok: fullHistoryMatches && proof.ok,
        receipts: receiptProofs,
        method: "full_append_only_ledger_and_order_line_allocations",
      });
    }
    const shipmentPath = url.pathname.match(/\/orders\/([^/]+)\/shipments$/);
    if (shipmentPath) {
      const order = state.orders.find(
        (o: any) => o.id === decodeURIComponent(shipmentPath[1]),
      );
      return order
        ? send({
            warehouse_id: state.warehouse.id,
            order_id: order.id,
            version: state.version,
            ok: true,
            order,
            packages: [],
            progress: order.lines.map((l: any) => {
              const a = order.allocations.filter(
                (a: any) => a.line_id === l.id,
              );
              const allocated = a.reduce(
                (n: number, a: any) => n + a.quantity - a.released,
                0,
              );
              return {
                line_id: l.id,
                sku: l.sku,
                ordered: l.quantity,
                allocated,
                picked: a.reduce((n: number, a: any) => n + a.picked, 0),
                shipped: a.reduce((n: number, a: any) => n + a.shipped, 0),
                unallocated: l.quantity - allocated,
              };
            }),
          })
        : send({ error: "Unknown order" }, 404);
    }
    if (url.pathname.endsWith("/outbox")) return send({ items: [] });
    const detail = url.pathname.match(
      /\/warehouses\/wh-pdx\/(orders|receipts)\/([^/]+)$/,
    );
    if (detail) {
      const records =
        detail[1] === "orders"
          ? [...state.orders, ...archivedOrders]
          : [...state.receipts, ...archivedReceipts];
      const record = records.find(
        (item: any) => item.id === decodeURIComponent(detail[2]),
      );
      return record
        ? send(
            record.id.startsWith("archived-order-")
              ? { ...record, customer: "Canonical historical customer" }
              : record,
          )
        : send({ error: "Record not found" }, 404);
    }
    const history = url.pathname.match(
      /\/history\/(orders|receipts|movements|activity|exceptions)$/,
    );
    if (history) {
      const kind = history[1];
      const older =
        kind === "orders"
          ? archivedOrders
          : kind === "receipts"
            ? archivedReceipts
            : [];
      const all = [...older, ...state[kind]]
        .map((data: unknown, index: number) => ({ cursor: index + 1, data }))
        .reverse();
      const before = Number(
        url.searchParams.get("before") ?? Number.MAX_SAFE_INTEGER,
      );
      const remaining = all.filter((item) => item.cursor < before);
      const items = remaining.slice(
        0,
        Number(url.searchParams.get("limit") ?? 50),
      );
      return send({
        items,
        next_cursor:
          remaining.length > items.length ? items.at(-1)!.cursor : null,
      });
    }
    if (url.pathname.match(/\/commands\/.+$/)) {
      const result = receipts.get(url.pathname.split("/").pop()!);
      return send(result ? { found: true, result } : { found: false });
    }
    if (url.pathname.endsWith("/commands")) {
      if (
        Buffer.byteLength(route.request().postData() ?? "", "utf8") >
        256 * 1024
      )
        return send({ error: "Command body exceeds 256 KiB" }, 413);
      const req = route.request().postDataJSON();
      if (recovery?.dropBeforeArrival && !dropped) {
        // Another operator commits after the original request is lost before
        // arrival. The missing command has never been applied.
        state = domain({
          state,
          command: {
            type: "report_exception",
            order_id: null,
            category: "Other",
            detail: "Concurrent operator observed a warehouse issue",
          },
        }).state;
        if (recovery.scoped) {
          state.resource_versions = {};
          if (recovery.scoped === "related")
            state.resource_versions[
              JSON.stringify(["receipt", req.command.receipt_id])
            ] = 1;
        }
        dropped = true;
        return route.abort("failed");
      }
      const currentResources = req.resource_versions
        ? Object.fromEntries(
            Object.keys(req.resource_versions).map((key) => [
              key,
              state.resource_versions?.[key] ?? 0,
            ]),
          )
        : null;
      const resourcesMatch =
        currentResources &&
        Object.entries(req.resource_versions).every(
          ([key, value]) => currentResources[key] === value,
        );
      if (req.expected_version !== state.version && !resourcesMatch) {
        const outcome = {
          status: "not_committed",
          reason: req.resource_versions ? "changed_resources" : "stale_version",
          tenant: "demo",
          actor: "operator:demo",
          warehouse_id: state.warehouse.id,
          request_id: req.request_id,
          expected_version: req.expected_version,
          current_version: state.version,
          ...(req.resource_versions
            ? {
                expected_resource_versions: req.resource_versions,
                current_resource_versions: currentResources,
              }
            : {}),
        };
        return send(
          {
            error: "Warehouse changed. Refresh and review.",
            ...(recovery
              ? {
                  command_outcome: recovery.outcome
                    ? recovery.outcome(outcome)
                    : outcome,
                }
              : {}),
          },
          recovery?.responseStatus ?? 409,
        );
      }
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
  await page
    .getByRole("button", { name: "Confirm packed", exact: true })
    .click();
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

async function loseReceiptBeforeArrival(page: Page) {
  await page.getByRole("button", { name: "Receiving", exact: true }).click();
  await page
    .getByRole("row")
    .filter({ hasText: "RCV-207" })
    .getByRole("button", { name: "Receive goods", exact: true })
    .click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Receive goods", exact: true })
    .click();
  await page.getByLabel("Received quantity (each)").fill("1");
  await page
    .getByRole("button", { name: "Confirm receipt", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Check original save", exact: true }),
  ).toBeEnabled();
}

test("a lost-before-arrival save unblocks only after exact definitive stale evidence", async ({
  page,
}) => {
  const state = await setup(page, false, 0, true, {
    dropBeforeArrival: true,
  });
  const received = () =>
    state().receipts.find((receipt: any) => receipt.reference === "RCV-207")
      .lines[0].received;
  const before = received();
  await loseReceiptBeforeArrival(page);
  expect(received()).toBe(before);
  const saved = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("wms-pending:demo:operator:demo")!),
  );
  expect(state().version).toBeGreaterThan(saved.request.expected_version);
  await page.reload();
  await page
    .getByRole("button", { name: "Check original save", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Check original save", exact: true }),
  ).toHaveCount(0);
  await expect(page.getByRole("alert")).toContainText(
    "Review current work and physical stock before starting a new save",
  );
  await expect(
    page.getByRole("button", { name: "New receipt", exact: true }),
  ).toBeEnabled();
  expect(received()).toBe(before);
  expect(domain({ state: state() }).ok).toBe(true);
  expect(
    await page.evaluate(() =>
      sessionStorage.getItem("wms-pending:demo:operator:demo"),
    ),
  ).toBeNull();
  await page.reload();
  await expect(
    page.getByRole("button", { name: "New receipt", exact: true }),
  ).toBeEnabled();
});

const rejectionCases: {
  label: string;
  outcome: (value: Record<string, unknown>) => unknown;
  responseStatus?: number;
}[] = [
  { label: "generic conflict", outcome: () => undefined },
  { label: "non-object proof", outcome: () => "not_committed" },
  ...["tenant", "actor", "warehouse_id", "request_id"].map((field) => ({
    label: `another ${field}`,
    outcome: (value: Record<string, unknown>) => ({
      ...value,
      [field]: "different-identity",
    }),
  })),
  {
    label: "another expected version",
    outcome: (value) => ({
      ...value,
      expected_version: Number(value.expected_version) + 1,
    }),
  },
  {
    label: "version did not advance",
    outcome: (value) => ({
      ...value,
      current_version: value.expected_version,
    }),
  },
  {
    label: "unsafe version",
    outcome: (value) => ({
      ...value,
      current_version: Number.MAX_SAFE_INTEGER + 1,
    }),
  },
  {
    label: "unrecognized proof schema",
    outcome: (value) => ({ ...value, status: "unknown" }),
  },
  {
    label: "forbidden response cannot prove rejection",
    outcome: (value) => value,
    responseStatus: 403,
  },
];
for (const scenario of rejectionCases) {
  test(`recovery retains original request: ${scenario.label}`, async ({
    page,
  }) => {
    await setup(page, false, 0, true, {
      dropBeforeArrival: true,
      ...scenario,
    });
    await loseReceiptBeforeArrival(page);
    const saved = await page.evaluate(() =>
      sessionStorage.getItem("wms-pending:demo:operator:demo"),
    );
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
      scenario.label,
    ).toBeEnabled();
    await expect(
      page.getByRole("button", { name: "New receipt", exact: true }),
      scenario.label,
    ).toBeDisabled();
    expect(
      await page.evaluate(() =>
        sessionStorage.getItem("wms-pending:demo:operator:demo"),
      ),
      scenario.label,
    ).toBe(saved);
  });
}

test("an in-flight warehouse refresh replaces stale canonical receipt detail before action", async ({
  page,
}) => {
  const state = await setup(page);
  await page.getByRole("button", { name: "Receiving", exact: true }).click();
  let releaseRefresh!: () => void;
  const refreshHeld = new Promise<void>((resolve) => {
    releaseRefresh = resolve;
  });
  let refreshStarted!: () => void;
  const started = new Promise<void>((resolve) => {
    refreshStarted = resolve;
  });
  await page.route(
    "https://warehouse.test/v1/warehouses/wh-pdx",
    async (route) => {
      refreshStarted();
      await refreshHeld;
      return route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          ...state(),
          reconciliation: domain({ state: state() }),
        }),
      });
    },
  );
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await started;
  await page.getByRole("button", { name: "RCV-207", exact: true }).click();
  const receipt = state().receipts.find(
    (value: any) => value.reference === "RCV-207",
  );
  const line = receipt.lines[0];
  await expect(page.getByRole("dialog")).toContainText(
    `${line.received} received / ${line.quantity} expected`,
  );
  const changed = domain({
    state: state(),
    command: {
      type: "receive",
      receipt_id: receipt.id,
      line_id: line.id,
      quantity: 4,
      location: "RECV-01",
    },
  });
  expect(changed.error).toBeUndefined();
  Object.assign(state(), changed.state);
  releaseRefresh();
  await expect(page.getByRole("dialog")).toContainText(
    `${line.received + 4} received / ${line.quantity} expected`,
  );
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Receive goods", exact: true })
    .click();
  await expect(page.getByLabel("Received quantity (each)")).toHaveValue(
    String(line.quantity - line.received - 4),
  );
});

test("blocked short picks can be physically returned, released and recounted", async ({
  page,
}) => {
  const state = await setup(page);
  const openOrder = async () => {
    await page
      .getByRole("button", { name: "Fulfillment", exact: true })
      .click();
    await page.getByRole("button", { name: "SO-1041", exact: true }).click();
  };
  await openOrder();
  await page
    .getByRole("button", { name: "Allocate stock", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await openOrder();
  await page.getByRole("button", { name: "Pick units", exact: true }).click();
  await page.getByLabel("Picked quantity (each)").fill("3");
  await page.getByRole("button", { name: "Confirm pick", exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await openOrder();
  await page
    .getByRole("button", { name: "Report exception", exact: true })
    .click();
  await page
    .getByLabel("Observed issue")
    .fill("Only three physical units found after partial pick");
  await page
    .getByRole("button", { name: "Open exception", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await openOrder();
  await expect(page.getByRole("dialog")).toContainText(
    "Only three physical units",
  );
  await expect(
    page.getByRole("button", { name: "Pick units", exact: true }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "Return staged units", exact: true }),
  ).toBeEnabled();
  await page
    .getByRole("button", { name: "Return staged units", exact: true })
    .click();
  await page
    .getByLabel("Return evidence / reason")
    .fill("Three verified staged units returned to original source");
  await page
    .getByRole("button", { name: "Confirm physical return", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await openOrder();
  await page
    .getByRole("button", { name: "Release allocation", exact: true })
    .click();
  await page
    .getByLabel("Allocation release reason")
    .fill("Release for physical shortage recount");
  await page
    .getByRole("button", { name: "Release reservations", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("button", { name: "Inventory", exact: true }).click();
  await page
    .getByRole("row")
    .filter({ hasText: "SS-TEE-001" })
    .filter({ hasText: "A-01-01" })
    .getByRole("button", { name: "Count", exact: true })
    .click();
  await page.getByLabel(/Counted each units/).fill("3");
  await page
    .getByLabel("Count evidence / reason")
    .fill("Physical recount confirms only the three returned units");
  await page
    .getByRole("button", { name: "Apply count adjustment", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const recovered = state().orders.find(
    (order: any) => order.reference === "SO-1041",
  );
  expect(recovered.status).toBe("pending");
  expect(recovered.allocations).toEqual([]);
  expect(
    state().stocks.find(
      (stock: any) =>
        stock.sku === "SS-TEE-001" && stock.location === "A-01-01",
    ).on_hand,
  ).toBe(3);
  expect(domain({ state: state() }).ok).toBe(true);
});

test("keyset history exposes archived records and opens authoritative details", async ({
  page,
}) => {
  await setup(page, false, 65);
  const detailRequests: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("/orders/archived-order-"))
      detailRequests.push(request.url());
  });
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "ARCHIVE-000", exact: true }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Browse orders history", exact: true })
    .click();
  await expect(
    page.getByRole("button", {
      name: "Load older orders history",
      exact: true,
    }),
  ).toBeEnabled();
  await page
    .getByRole("button", { name: "Load older orders history", exact: true })
    .click();
  await expect(
    page.getByText("All orders loaded", { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "ARCHIVE-000", exact: true }).click();
  await expect(page.getByRole("dialog")).toContainText(
    "Canonical historical customer",
  );
  expect(detailRequests).toHaveLength(1);
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  await page.getByRole("button", { name: "Receiving", exact: true }).click();
  await page
    .getByRole("button", { name: "Browse receipts history", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Load older receipts history", exact: true })
    .click();
  await page
    .getByRole("button", { name: "ARCHIVE-RCV-000", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toContainText("ARCHIVE-RCV-000");
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Browse movements history", exact: true })
    .click();
  await expect(
    page.getByText("All movements loaded", { exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Browse audit records history", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Command audit", exact: true }),
  ).toBeVisible();
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

test("ledger uses the independent history proof even when operational balances agree", async ({
  page,
}) => {
  const fullChecks: string[] = [];
  page.on("request", (request) => {
    if (request.url().endsWith("/reconciliation"))
      fullChecks.push(request.url());
  });
  await setup(page, false, 0, false);
  await expect(
    page.getByText("Operational balances checked", { exact: true }),
  ).toBeVisible();
  expect(fullChecks).toHaveLength(0);
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  await expect(
    page.getByText("Full reconciliation needs review", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Inventory ledger reconciles", { exact: true }),
  ).toHaveCount(0);
  expect(fullChecks).toHaveLength(1);
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export proof", exact: true }).click();
  const result = await download;
  const proof = JSON.parse(readFileSync((await result.path())!, "utf8"));
  expect(proof.ok).toBe(false);
  expect(proof.method).toBe(
    "full_append_only_ledger_and_order_line_allocations",
  );
  expect(proof.receipts.length).toBeGreaterThan(0);
  await page
    .getByRole("button", { name: "Run full reconciliation", exact: true })
    .click();
  await expect(
    page.getByText("Full reconciliation needs review", { exact: true }),
  ).toBeVisible();
  expect(fullChecks).toHaveLength(2);
});

test("unrelated work permits exact scoped recovery under the original request", async ({
  page,
}) => {
  const state = await setup(page, false, 0, true, {
    dropBeforeArrival: true,
    scoped: "unrelated",
  });
  const received = () =>
    state().receipts.find((r: any) => r.reference === "RCV-207").lines[0]
      .received;
  const before = received();
  await loseReceiptBeforeArrival(page);
  const original = await page.evaluate(() =>
    JSON.parse(sessionStorage.getItem("wms-pending:demo:operator:demo")!),
  );
  expect(Object.keys(original.request.resource_versions)).toHaveLength(4);
  expect(received()).toBe(before);
  await page.reload();
  await page
    .getByRole("button", { name: "Check original save", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Check original save", exact: true }),
  ).toHaveCount(0);
  expect(received()).toBe(before + 1);
  expect(
    await page.evaluate(() =>
      sessionStorage.getItem("wms-pending:demo:operator:demo"),
    ),
  ).toBeNull();
  expect(domain({ state: state() }).ok).toBe(true);
});

test("changed resources retire only the matching permanently fenced original", async ({
  page,
}) => {
  const state = await setup(page, false, 0, true, {
    dropBeforeArrival: true,
    scoped: "related",
  });
  const received = () =>
    state().receipts.find((r: any) => r.reference === "RCV-207").lines[0]
      .received;
  const before = received();
  await loseReceiptBeforeArrival(page);
  await page.reload();
  await page
    .getByRole("button", { name: "Check original save", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Check original save", exact: true }),
  ).toHaveCount(0);
  expect(received()).toBe(before);
  await expect(page.getByRole("alert")).toContainText(
    "Review current work and physical stock",
  );
});

for (const scenario of [
  {
    label: "different expected resources",
    change: (proof: any) => ({ ...proof, expected_resource_versions: {} }),
  },
  {
    label: "missing current resources",
    change: (proof: any) => ({ ...proof, current_resource_versions: {} }),
  },
  {
    label: "unchanged resources",
    change: (proof: any) => ({
      ...proof,
      current_resource_versions: proof.expected_resource_versions,
    }),
  },
  {
    label: "negative resource revision",
    change: (proof: any) => ({
      ...proof,
      current_resource_versions: Object.fromEntries(
        Object.keys(proof.current_resource_versions).map((k) => [k, -1]),
      ),
    }),
  },
  {
    label: "unsafe resource revision",
    change: (proof: any) => ({
      ...proof,
      current_resource_versions: Object.fromEntries(
        Object.keys(proof.current_resource_versions).map((k) => [
          k,
          Number.MAX_SAFE_INTEGER + 1,
        ]),
      ),
    }),
  },
  {
    label: "legacy proof for a scoped command",
    change: (proof: any) => {
      const {
        expected_resource_versions,
        current_resource_versions,
        ...legacy
      } = proof;
      return { ...legacy, reason: "stale_version" };
    },
  },
]) {
  test(`scoped recovery retains original: ${scenario.label}`, async ({
    page,
  }) => {
    await setup(page, false, 0, true, {
      dropBeforeArrival: true,
      scoped: "related",
      outcome: scenario.change,
    });
    await loseReceiptBeforeArrival(page);
    const original = await page.evaluate(() =>
      sessionStorage.getItem("wms-pending:demo:operator:demo"),
    );
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeEnabled();
    expect(
      await page.evaluate(() =>
        sessionStorage.getItem("wms-pending:demo:operator:demo"),
      ),
    ).toBe(original);
    await expect(
      page.getByRole("button", { name: "New receipt", exact: true }),
    ).toBeDisabled();
  });
}

test("long-label multi-lot shipments retain the strict fence within the command body limit", async ({
  page,
}) => {
  test.setTimeout(60000);
  const current = await setup(page);
  const s = current();
  const sku = `LONG-SKU-${"S".repeat(231)}`;
  const location = `LONG-BIN-${"B".repeat(231)}`;
  s.products.push({
    sku,
    name: "Long label product",
    category: "Test",
    reorder_point: 0,
  });
  s.locations.push({
    code: location,
    zone: "Long label storage",
    kind: "storage",
  });
  const allocations = [];
  for (let i = 0; i < 220; i++) {
    const lot = `LOT-${String(i).padStart(3, "0")}-${"L".repeat(232)}`;
    s.stocks.push(
      { sku, lot, location, on_hand: 0, reserved: 0 },
      { sku, lot, location: "STAGE-01", on_hand: 1, reserved: 1 },
    );
    allocations.push({
      line_id: "long-line",
      sku,
      lot,
      location,
      quantity: 1,
      picked: 1,
      shipped: 0,
      released: 0,
    });
    s.movements.push(
      {
        id: randomUUID(),
        kind: "count",
        sku,
        lot,
        from: null,
        to: location,
        quantity: 1,
        reference: "LONG-COUNT",
        reason: "Synthetic count",
        actor: "fixture:synthetic",
        at: new Date().toISOString(),
      },
      {
        id: randomUUID(),
        kind: "pick",
        sku,
        lot,
        from: location,
        to: "STAGE-01",
        quantity: 1,
        reference: "LONG-LABEL-SHIPMENT",
        reason: "Synthetic pick",
        actor: "fixture:synthetic",
        at: new Date().toISOString(),
      },
    );
  }
  const order = {
    ...s.orders[0],
    id: randomUUID(),
    reference: "LONG-LABEL-SHIPMENT",
    source_order_id: null,
    status: "packed",
    lines: [{ id: "long-line", sku, quantity: 220 }],
    allocations,
    carrier: "",
    handoff_reference: "",
    shipped_at: null,
  };
  s.orders.unshift(order);
  s.resource_versions = {};
  expect(domain({ state: s }).ok).toBe(true);
  await page
    .getByRole("button", { name: "Refresh warehouse", exact: true })
    .click();
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await page
    .getByRole("button", { name: order.reference, exact: true })
    .click();
  await page.getByRole("button", { name: "Record handoff" }).click();
  await page.getByLabel("Carrier", { exact: true }).fill("UPS");
  await page
    .getByLabel("Carrier pickup / tracking reference")
    .fill("LONG-TEST-PICKUP");
  const posted = page.waitForRequest(
    (r) => r.url().endsWith("/commands") && r.method() === "POST",
  );
  await page.getByRole("button", { name: "Confirm physical handoff" }).click();
  const request = await posted;
  expect(request.postDataJSON().resource_versions).toBeUndefined();
  expect(Buffer.byteLength(request.postData()!, "utf8")).toBeLessThan(
    256 * 1024,
  );
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(current().orders.find((o: any) => o.id === order.id).status).toBe(
    "shipped",
  );
  expect(
    current()
      .stocks.filter((b: any) => b.sku === sku)
      .every((b: any) => b.on_hand === 0 && b.reserved === 0),
  ).toBe(true);
  expect(domain({ state: current() }).ok).toBe(true);
});

function partialCancellationFixture(state: any) {
  const receipt = state.receipts.find((r: any) => r.reference === "RCV-207");
  return domain({
    state,
    command: {
      type: "receive",
      receipt_id: receipt.id,
      line_id: receipt.lines[0].id,
      quantity: 12,
      location: "RECV-01",
    },
  }).state;
}
async function cancelRemainder(page: Page) {
  await page.getByRole("button", { name: "Receiving", exact: true }).click();
  await page
    .getByRole("row")
    .filter({ hasText: "RCV-207" })
    .getByRole("button", { name: "Receive goods", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Cancel unreceived remainder", exact: true })
    .click();
  await page
    .getByLabel("Cancellation reason", { exact: true })
    .fill("Supplier cancelled the remaining delivery");
  await page
    .getByRole("button", {
      name: "Confirm remainder cancellation",
      exact: true,
    })
    .click();
}
test("partial receipt cancellation retains stock and closes all receiving controls including scanner", async ({
  page,
}) => {
  const state = await setup(
    page,
    false,
    0,
    true,
    undefined,
    partialCancellationFixture,
  );
  const before = structuredClone(state());
  await cancelRemainder(page);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const cancelled = state().receipts.find(
    (r: any) => r.reference === "RCV-207",
  );
  expect(cancelled.status).toBe("cancelled");
  expect(cancelled.lines[0].received).toBe(12);
  expect(cancelled.cancellation.lines[cancelled.lines[0].id].cancelled).toBe(
    cancelled.lines[0].quantity - 12,
  );
  expect(state().stocks).toEqual(before.stocks);
  expect(state().movements).toEqual(before.movements);
  await page
    .getByRole("row")
    .filter({ hasText: "RCV-207" })
    .getByRole("button", { name: "View receipt", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toContainText(
    "Supplier cancelled the remaining delivery",
  );
  await expect(
    page
      .getByRole("dialog")
      .getByRole("button", { name: "Receive goods", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", {
      name: "Cancel unreceived remainder",
      exact: true,
    }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  await page.getByRole("button", { name: "Scanner", exact: true }).click();
  await page.getByLabel("Receipt reference", { exact: true }).fill("RCV-207");
  await page.getByRole("button", { name: "Load work", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText(
    "receipt remainder was cancelled",
  );
  expect(domain({ state: state() }).ok).toBe(true);
});
for (const loss of ["after", "before"] as const) {
  test(`receipt cancellation lost ${loss} arrival preserves the original journal and stock`, async ({
    page,
  }) => {
    const state = await setup(
      page,
      loss === "after",
      0,
      true,
      loss === "before" ? { dropBeforeArrival: true } : undefined,
      partialCancellationFixture,
    );
    const stocks = structuredClone(state().stocks);
    await cancelRemainder(page);
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeVisible();
    const saved = await page.evaluate(() =>
      JSON.parse(sessionStorage.getItem("wms-pending:demo:operator:demo")!),
    );
    expect(saved.request.command.type).toBe("cancel_receipt");
    const version = state().version;
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toHaveCount(0);
    const receipt = state().receipts.find(
      (r: any) => r.reference === "RCV-207",
    );
    expect(receipt.status).toBe(loss === "after" ? "cancelled" : "partial");
    expect(state().stocks).toEqual(stocks);
    expect(state().version).toBe(version);
    expect(
      await page.evaluate(() =>
        sessionStorage.getItem("wms-pending:demo:operator:demo"),
      ),
    ).toBeNull();
    if (loss === "before") {
      await expect(page.getByRole("alert")).toContainText(
        "Review current work",
      );
      await cancelRemainder(page);
      await expect(page.getByRole("dialog")).toHaveCount(0);
      expect(
        state().receipts.find((r: any) => r.reference === "RCV-207").status,
      ).toBe("cancelled");
    }
  });
}

for (const kind of [
  "synthetic_fixture",
  "legacy_snapshot",
  "unknown_source",
] as const) {
  test(`return allowance identifies ${kind} provenance without presenting it as a physical event`, async ({
    page,
  }) => {
    const state = await setup(page);
    const order = state().orders.find((o: any) => o.status === "shipped");
    await page.route(
      `**/orders/${order.id}/return-eligibility`,
      async (route) =>
        route.fulfill({
          contentType: "application/json",
          body: JSON.stringify({
            order_id: order.id,
            version: state().version,
            shipment_source: { kind, event_id: null, version: state().version },
            lines: order.allocations.map((a: any) => ({
              order_line_id: a.line_id,
              sku: a.sku,
              lot: a.lot,
              remaining: a.shipped,
            })),
          }),
        }),
    );
    await page
      .getByRole("button", { name: "Fulfillment", exact: true })
      .click();
    await page
      .getByRole("button", { name: order.reference, exact: true })
      .click();
    await page
      .getByRole("button", { name: "Review return allowance", exact: true })
      .click();
    if (kind === "unknown_source") {
      await expect(page.getByRole("alert")).toContainText(
        "Return allowance source could not be verified",
      );
      await expect(
        page.getByLabel("RMA reference", { exact: true }),
      ).toHaveCount(0);
    } else {
      await expect(
        page.getByText(
          kind === "synthetic_fixture"
            ? "Synthetic shipment fixture — use for testing only."
            : "Allowance comes from a retained legacy snapshot. Verify the original handoff before receiving returned goods.",
          { exact: true },
        ),
      ).toBeVisible();
      await expect(
        page.getByLabel("RMA reference", { exact: true }),
      ).toBeVisible();
    }
    await expect(
      page.getByText(
        "Allowance checked against the original recorded handoff.",
        { exact: true },
      ),
    ).toHaveCount(0);
  });
}

test("ledger identifies original shipment disagreement even when every stock balance matches", async ({
  page,
}) => {
  const state = await setup(page);
  const order = state().orders.find((o: any) => o.status === "shipped");
  const shipment = {
    order_id: order.id,
    source_kind: "event",
    event_id: "original-shipment-evidence",
    source_version: state().version,
    ok: false,
  };
  await page.route("**/v1/warehouses/wh-pdx/reconciliation", (route) =>
    route.fulfill({
      contentType: "application/json",
      body: JSON.stringify({
        ...domain({ state: state() }),
        version: state().version,
        ok: false,
        shipments: [shipment],
      }),
    }),
  );
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  await expect(
    page.getByText("Full reconciliation needs review", { exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("alert")).toContainText(
    "Shipment evidence needs review for 1 order",
  );
  const exported = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export proof", exact: true }).click();
  const file = await (await exported).path();
  const proof = JSON.parse(readFileSync(file!, "utf8"));
  expect(proof.shipments).toEqual([shipment]);
  expect(proof.rows.every((r: any) => r.ok)).toBe(true);
});

for (const loss of ["before", "after"] as const) {
  test(`facility dispatch lost ${loss} arrival retains its exact original journal and source custody`, async ({
    page,
  }) => {
    const state = await setup(
      page,
      loss === "after",
      0,
      true,
      loss === "before" ? { dropBeforeArrival: true } : undefined,
    );
    const target = structuredClone(state());
    target.warehouse = {
      ...target.warehouse,
      id: "wh-east",
      code: "EAST",
      name: "Synthetic destination",
    };
    await page.route(
      (url) => url.pathname === "/v1/warehouses",
      (route) =>
        route.fulfill({
          json: {
            warehouses: [
              { warehouse: state().warehouse },
              { warehouse: target.warehouse },
            ],
            next_after: null,
          },
        }),
    );
    await page.route("**/v1/warehouses/wh-east", (route) =>
      route.fulfill({ json: target }),
    );
    await page.route(
      (url) => url.pathname.endsWith("/transfers"),
      (route) =>
        route.fulfill({
          json: {
            warehouse_id: state().warehouse.id,
            version: state().version,
            items: [],
            next_cursor: null,
          },
        }),
    );
    const stock = state().stocks.find(
      (b: any) =>
        b.on_hand - b.reserved >= 2 &&
        state().locations.some(
          (l: any) => l.code === b.location && l.kind === "storage",
        ),
    );
    const before = state().stocks.reduce(
      (n: number, b: any) => n + b.on_hand,
      0,
    );
    await page
      .getByRole("button", { name: "Facility transfers", exact: true })
      .click();
    await page.getByText("New facility transfer", { exact: true }).click();
    await expect(
      page.getByLabel("Destination facility", { exact: true }),
    ).toBeEnabled();
    await page
      .getByLabel("Destination facility", { exact: true })
      .selectOption("wh-east");
    await page
      .getByLabel("Transfer reference", { exact: true })
      .fill("ORIGINAL-TRANSFER");
    await page
      .getByLabel("Transfer carrier", { exact: true })
      .fill("Synthetic carrier");
    await page
      .getByLabel("Carrier handoff reference", { exact: true })
      .fill("ORIGINAL-HANDOFF");
    await page
      .getByLabel("Source stock 1", { exact: true })
      .selectOption(JSON.stringify([stock.sku, stock.lot, stock.location]));
    await page.getByLabel("Dispatch each units 1", { exact: true }).fill("2");
    await page
      .getByRole("button", { name: "Review transfer dispatch", exact: true })
      .click();
    await expect(
      page.getByRole("heading", {
        name: "Confirm physical dispatch",
        exact: true,
      }),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "Record carrier handoff", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeVisible();
    const journal = await page.evaluate(() =>
      JSON.parse(sessionStorage.getItem("wms-pending:demo:operator:demo")!),
    );
    expect(journal.warehouse).toBe("wh-pdx");
    expect(journal.request.command.destination_warehouse_id).toBe("wh-east");
    expect(journal.request.command.lines).toEqual([
      {
        id: expect.any(String),
        sku: stock.sku,
        lot: stock.lot,
        location: stock.location,
        quantity: 2,
      },
    ]);
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toHaveCount(0);
    expect(
      await page.evaluate(() =>
        sessionStorage.getItem("wms-pending:demo:operator:demo"),
      ),
    ).toBeNull();
    expect(state().stocks.reduce((n: number, b: any) => n + b.on_hand, 0)).toBe(
      before - (loss === "after" ? 2 : 0),
    );
    expect(
      state().movements.filter((m: any) => m.kind === "transfer_dispatch"),
    ).toHaveLength(loss === "after" ? 1 : 0);
  });
}

test("transfer receiving holds inconsistent original evidence and rejects mixed-version pages", async ({
  page,
}) => {
  const state = await setup(page);
  const transfer = {
    id: "original-transfer",
    reference: "ORIGINAL-TRANSFER",
    source_warehouse_id: "wh-east",
    destination_warehouse_id: "wh-pdx",
    carrier: "Carrier",
    handoff_reference: "ORIGINAL-HANDOFF",
    ok: false,
    lines: [
      {
        id: "a",
        sku: "SS-CAP-004",
        lot: "LOT-2610",
        location: "A-01",
        quantity: 3,
        received: 1,
        lost: 0,
        loss_record_count: 0,
      },
    ],
  };
  await page.route(
    (url) => url.pathname.endsWith("/transfers"),
    (route) =>
      route.fulfill({
        json: {
          warehouse_id: "wh-pdx",
          version:
            state().version +
            (route.request().url().includes("after=") ? 1 : 0),
          items: [
            {
              cursor: route.request().url().includes("after=") ? 2 : 1,
              data: transfer,
            },
          ],
          next_cursor: route.request().url().includes("after=") ? null : 1,
        },
      }),
  );
  await page
    .getByRole("button", { name: "Facility transfers", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText(
    "Original transfer evidence needs reconciliation",
  );
  await expect(
    page.getByRole("button", {
      name: "Receive transfer ORIGINAL-TRANSFER line a",
      exact: true,
    }),
  ).toBeDisabled();
  await page
    .getByRole("button", { name: "Load older transfers", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText(
    "Warehouse changed while paging transfers",
  );
  await expect(
    page.getByRole("button", {
      name: "Receive transfer ORIGINAL-TRANSFER line a",
      exact: true,
    }),
  ).toHaveCount(0);
});

test("mixed movement CSV retains later transfer ownership columns and neutralizes spreadsheet formulas", async ({
  page,
}) => {
  const state = await setup(
    page,
    false,
    0,
    true,
    undefined,
    (original: any) => {
      const bucket = original.stocks.find(
        (b: any) =>
          b.on_hand - b.reserved >= 2 &&
          original.locations.some(
            (l: any) => l.code === b.location && l.kind === "storage",
          ),
      );
      const next = domain({
        state: original,
        command: {
          type: "dispatch_transfer",
          reference: "CSV-TRANSFER",
          destination_warehouse_id: "wh-east",
          carrier: "Synthetic carrier",
          handoff_reference: "=CARRIER-HOOK",
          lines: [
            {
              id: "=original-line",
              sku: bucket.sku,
              lot: bucket.lot,
              location: bucket.location,
              quantity: 2,
            },
          ],
        },
      }).state;
      next.movements.at(-1).transfer_event_id = "original-dispatch-event";
      return next;
    },
  );
  expect(state().movements[0].transfer_id).toBeUndefined();
  const transfer = state().movements.at(-1);
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export CSV", exact: true }).click();
  const file = await (await download).path();
  const csv = readFileSync(file!, "utf8");
  const header = csv.split("\r\n")[0];
  for (const field of ["transfer_id", "transfer_line_id", "transfer_event_id"])
    expect(header).toContain(`"${field}"`);
  expect(csv).toContain(`"${transfer.transfer_id}"`);
  expect(csv).toContain('"\'=original-line"');
  expect(csv).toContain('"original-dispatch-event"');
  expect(csv).toContain('"\'=CARRIER-HOOK"');
});

for (const loss of ["after", "before"] as const) {
  test(`packed shipment lost ${loss} arrival recovers only its original journal`, async ({
    page,
  }) => {
    const state = await setup(
      page,
      loss === "after",
      0,
      true,
      loss === "before" ? { dropBeforeArrival: true } : undefined,
    );
    const order = state().orders.find((o: any) => o.status === "picked");
    const stock = structuredClone(state().stocks);
    const pack = async () => {
      await page
        .getByRole("button", { name: "Fulfillment", exact: true })
        .click();
      await page
        .getByRole("button", { name: order.reference, exact: true })
        .click();
      await page
        .getByLabel("Package reference", { exact: true })
        .fill("REVIEWED-PACKAGE");
      const a = order.allocations[0];
      await page
        .getByLabel(`Package each units ${a.line_id} ${a.location} ${a.lot}`, {
          exact: true,
        })
        .fill("2");
      await page
        .getByRole("button", { name: "Confirm packed shipment", exact: true })
        .click();
    };
    await pack();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeVisible();
    const original = await page.evaluate(() =>
      JSON.parse(sessionStorage.getItem("wms-pending:demo:operator:demo")!),
    );
    expect(original.request.command).toMatchObject({
      type: "pack_shipment",
      reference: "REVIEWED-PACKAGE",
      lines: [{ quantity: 2 }],
    });
    const version = state().version;
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toHaveCount(0);
    expect(state().version).toBe(version);
    expect(state().stocks).toEqual(stock);
    expect(
      state().orders.find((o: any) => o.id === order.id).shipment_mode,
    ).toBe(loss === "after" ? "packages" : undefined);
    expect(
      await page.evaluate(() =>
        sessionStorage.getItem("wms-pending:demo:operator:demo"),
      ),
    ).toBeNull();
    if (loss === "before") {
      await expect(page.getByRole("alert")).toContainText(
        "Review current work",
      );
      await pack();
      await expect(page.getByRole("dialog")).toHaveCount(0);
      expect(
        state().orders.find((o: any) => o.id === order.id).shipment_mode,
      ).toBe("packages");
    }
  });
}

for (const corruption of [
  "warehouse",
  "line ownership",
  "duplicate package",
  "overlapping claims",
] as const) {
  test(`shipment review holds ${corruption} disagreement before any command`, async ({
    page,
  }) => {
    const state = await setup(page);
    const order = state().orders.find((o: any) => o.status === "picked");
    const a = order.allocations[0];
    const packed = {
      id: "original-package",
      order_id: order.id,
      reference: "PACK-REVIEW",
      closure: null,
      lines: [
        {
          line_id: a.line_id,
          sku: a.sku,
          location: a.location,
          lot: a.lot,
          quantity: 2,
        },
      ],
    };
    await page.route(`**/orders/${order.id}/shipments`, async (route) => {
      const packages = structuredClone([packed]);
      if (corruption === "line ownership")
        packages[0].lines[0].line_id = "another-same-sku-line";
      if (corruption === "duplicate package")
        packages.push(structuredClone(packed));
      if (corruption === "overlapping claims")
        packages[0].lines[0].quantity = a.picked + 1;
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          warehouse_id:
            corruption === "warehouse"
              ? "another-warehouse"
              : state().warehouse.id,
          order_id: order.id,
          order,
          version: state().version,
          ok: true,
          packages,
          progress: order.lines.map((l: any) => ({
            line_id: l.id,
            sku: l.sku,
            ordered: l.quantity,
            allocated: a.quantity,
            picked: a.picked,
            shipped: 0,
            unallocated: 0,
          })),
        }),
      });
    });
    const commands: unknown[] = [];
    page.on("request", (r) => {
      if (r.method() === "POST" && r.url().endsWith("/commands"))
        commands.push(r.postDataJSON());
    });
    await page
      .getByRole("button", { name: "Fulfillment", exact: true })
      .click();
    await page
      .getByRole("button", { name: order.reference, exact: true })
      .click();
    await expect(page.getByRole("alert")).toContainText(
      "Shipment ownership review could not be verified",
    );
    await expect(
      page.getByRole("button", {
        name: "Confirm packed shipment",
        exact: true,
      }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Hand off PACK-REVIEW", exact: true }),
    ).toHaveCount(0);
    expect(commands).toEqual([]);
  });
}

for (const event_ids of [
  ["original-first", "original-second"],
  [],
  ["duplicate", "duplicate"],
  undefined,
]) {
  test(`partial return review verifies handoff events ${JSON.stringify(event_ids)}`, async ({
    page,
  }) => {
    const state = await setup(page);
    const order = state().orders.find((o: any) => o.status === "shipped");
    await page.route(`**/orders/${order.id}/return-eligibility`, (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          order_id: order.id,
          version: state().version,
          shipment_source: {
            kind: "handoff_events",
            event_id: null,
            event_ids,
            version: state().version,
          },
          lines: order.allocations.map((a: any) => ({
            order_line_id: a.line_id,
            sku: a.sku,
            lot: a.lot,
            remaining: a.shipped,
          })),
        }),
      }),
    );
    await page
      .getByRole("button", { name: "Fulfillment", exact: true })
      .click();
    await page
      .getByRole("button", { name: order.reference, exact: true })
      .click();
    await page
      .getByRole("button", { name: "Review return allowance", exact: true })
      .click();
    if (event_ids?.length === 2 && event_ids[0] !== event_ids[1]) {
      await expect(
        page.getByText(
          "Allowance checked against the recorded shipment handoffs. Unshipped units are excluded.",
          { exact: true },
        ),
      ).toBeVisible();
      await expect(
        page.getByLabel("RMA reference", { exact: true }),
      ).toBeVisible();
    } else {
      await expect(page.getByRole("alert")).toContainText(
        "Return allowance source could not be verified",
      );
      await expect(
        page.getByLabel("RMA reference", { exact: true }),
      ).toHaveCount(0);
    }
  });
}

// Explicit HTTP contract fixtures for cancellation controls and browser journals.
// Business cancellation/packing source proofs are tested against real PostgreSQL.
function partialOrderClosureFixture(state: any) {
  const order = state.orders.find((o: any) => o.status === "shipped");
  order.status = "partially_shipped";
  order.shipment_mode = "packages";
  order.lines[0].quantity += 1; // One original unallocated unit remains.
  return state;
}
function closureReview(state: any, order: any) {
  return {
    warehouse_id: state.warehouse.id,
    order_id: order.id,
    version: state.version,
    ok: true,
    order,
    packages: [
      {
        id: "original-ui-handoff-package",
        order_id: order.id,
        reference: "ORIGINAL-HANDOFF",
        lines: order.allocations.map((a: any) => ({
          line_id: a.line_id,
          sku: a.sku,
          location: a.location,
          lot: a.lot,
          quantity: a.shipped,
        })),
        closure: {
          kind: "handed_off",
          carrier: order.carrier,
          handoff_reference: order.handoff_reference,
          reason: "",
          closed_at: order.shipped_at,
          actor: "operator:demo",
        },
      },
    ],
    progress: order.lines.map((l: any) => {
      const a = order.allocations.filter((a: any) => a.line_id === l.id);
      const allocated = a.reduce(
        (n: number, a: any) => n + a.quantity - a.released,
        0,
      );
      const cancelled =
        order.remainder_cancellation?.lines[l.id].cancelled ?? 0;
      return {
        line_id: l.id,
        sku: l.sku,
        ordered: l.quantity,
        allocated,
        picked: a.reduce((n: number, a: any) => n + a.picked, 0),
        shipped: a.reduce((n: number, a: any) => n + a.shipped, 0),
        cancelled,
        unallocated: l.quantity - allocated - cancelled,
      };
    }),
  };
}
function closedOrderContract(state: any, order: any, reason: string) {
  const result = structuredClone(state),
    closed = result.orders.find((o: any) => o.id === order.id);
  closed.status = "closed_partial";
  closed.remainder_cancellation = {
    reason,
    actor: "operator:demo",
    cancelled_at: new Date().toISOString(),
    lines: Object.fromEntries(
      closed.lines.map((l: any) => {
        const shipped = closed.allocations
          .filter((a: any) => a.line_id === l.id)
          .reduce((n: number, a: any) => n + a.shipped, 0);
        return [
          l.id,
          {
            sku: l.sku,
            ordered: l.quantity,
            shipped,
            cancelled: l.quantity - shipped,
          },
        ];
      }),
    ),
  };
  result.version += 1;
  return result;
}
async function openPartialClosure(page: Page, reference: string) {
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await page.getByRole("button", { name: reference, exact: true }).click();
  await expect(
    page.getByRole("heading", {
      name: "Cancel the unshipped remainder",
      exact: true,
    }),
  ).toBeVisible();
}
for (const loss of ["before", "after"] as const) {
  test(`commercial remainder cancellation lost ${loss} arrival retains the exact reviewed request through reload`, async ({
    page,
  }) => {
    const state = await setup(
      page,
      false,
      0,
      true,
      undefined,
      partialOrderClosureFixture,
    );
    const order = state().orders.find(
      (o: any) => o.status === "partially_shipped",
    );
    const stock = structuredClone(state().stocks),
      originalLines = structuredClone(order.lines),
      version = state().version;
    const requests: any[] = [];
    let dropped = false,
      receiptsAvailable = false,
      accepted: any;
    await page.route(`**/orders/${order.id}/shipments`, (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(
          closureReview(
            state(),
            state().orders.find((o: any) => o.id === order.id),
          ),
        ),
      }),
    );
    await page.route("**/commands/*", (route) =>
      receiptsAvailable
        ? route.fulfill({
            contentType: "application/json",
            body: JSON.stringify(
              accepted
                ? { found: true, result: accepted.result }
                : { found: false },
            ),
          })
        : route.abort("failed"),
    );
    await page.route("**/commands", async (route) => {
      const request = route.request().postDataJSON();
      if (request.command.type !== "cancel_remainder") return route.fallback();
      requests.push(request);
      if (!dropped && loss === "before") {
        dropped = true;
        return route.abort("failed");
      }
      if (!accepted) {
        Object.assign(
          state(),
          closedOrderContract(state(), order, request.command.reason),
        );
        accepted = {
          replayed: false,
          result: {
            entity_id: order.id,
            version: state().version,
            type: "fulfillment.remainder_cancelled",
          },
        };
      }
      if (!dropped) {
        dropped = true;
        return route.abort("failed");
      }
      return route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(accepted),
      });
    });
    await openPartialClosure(page, order.reference);
    const button = page.getByRole("button", {
      name: "Cancel unshipped remainder",
      exact: true,
    });
    await expect(button).toBeDisabled();
    await page
      .getByLabel("Remainder cancellation reason", { exact: true })
      .fill("Customer withdrew the remaining original unit");
    await expect(button).toBeDisabled();
    await page
      .getByLabel(
        "I confirmed cancellation of all remaining original demand.",
        { exact: true },
      )
      .check();
    await button.click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeVisible();
    const original = await page.evaluate(() =>
      JSON.parse(sessionStorage.getItem("wms-pending:demo:operator:demo")!),
    );
    expect(original.warehouse).toBe(state().warehouse.id);
    expect(original.request.expected_version).toBe(version);
    expect(original.request.command).toEqual({
      type: "cancel_remainder",
      order_id: order.id,
      reason: "Customer withdrew the remaining original unit",
    });
    receiptsAvailable = true;
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toHaveCount(0);
    expect(state().stocks).toEqual(stock);
    expect(state().version).toBe(version + 1);
    expect(requests).toHaveLength(loss === "before" ? 2 : 1);
    for (const request of requests) expect(request).toEqual(original.request);
    const closed = state().orders.find((o: any) => o.id === order.id);
    expect(closed.status).toBe("closed_partial");
    expect(closed.lines).toEqual(originalLines);
    await page
      .getByRole("button", { name: "Fulfillment", exact: true })
      .click();
    await page
      .getByRole("button", { name: order.reference, exact: true })
      .click();
    await expect(
      page.getByText("Unshipped remainder cancelled", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", {
        name: "Cancel unshipped remainder",
        exact: true,
      }),
    ).toHaveCount(0);
    expect(
      await page.evaluate(() =>
        sessionStorage.getItem("wms-pending:demo:operator:demo"),
      ),
    ).toBeNull();
  });
}
for (const corruption of [
  "cancelled quantity",
  "missing closure",
  "altered shipped counters",
] as const) {
  test(`closed partial review holds ${corruption} instead of presenting cancelled units as open demand`, async ({
    page,
  }) => {
    const state = await setup(
      page,
      false,
      0,
      true,
      undefined,
      partialOrderClosureFixture,
    );
    const order = state().orders.find(
      (o: any) => o.status === "partially_shipped",
    );
    const closed = closedOrderContract(
      state(),
      order,
      "Customer cancellation",
    ).orders.find((o: any) => o.id === order.id);
    const review = closureReview(state(), closed);
    if (corruption === "cancelled quantity") review.progress[0].cancelled += 1;
    if (corruption === "missing closure")
      delete review.order.remainder_cancellation;
    if (corruption === "altered shipped counters") {
      review.order.allocations[0].shipped -= 1;
      review.order.allocations[0].picked -= 1;
    }
    await page.route(`**/orders/${order.id}/shipments`, (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(review),
      }),
    );
    await page
      .getByRole("button", { name: "Fulfillment", exact: true })
      .click();
    await page
      .getByRole("button", { name: order.reference, exact: true })
      .click();
    await expect(
      page.getByRole("alert").filter({
        hasText:
          /review could not be verified|cancellation could not be verified/,
      }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", {
        name: "Cancel unshipped remainder",
        exact: true,
      }),
    ).toHaveCount(0);
  });
}

test("ledger runs the proof in the background when it exceeds the request deadline", async ({
  page,
}) => {
  await setup(page);
  // The complete proof the synchronous endpoint would have returned.
  const complete = await page.evaluate(() =>
    fetch("/v1/warehouses/wh-pdx/reconciliation").then((r) => r.json()),
  );
  const calls: string[] = [];
  await page.route("**/v1/warehouses/wh-pdx/reconciliation", (route) => {
    calls.push("direct");
    return route.fulfill({
      status: 503,
      contentType: "application/json",
      body: JSON.stringify({
        error: "Database operation unavailable; retry with the same request ID",
      }),
    });
  });
  await page.route("**/v1/warehouses/wh-pdx/reconciliation/runs", (route) => {
    calls.push(`${route.request().method()} run`);
    return route.fulfill({
      status: 202,
      contentType: "application/json",
      body: JSON.stringify({ id: "run-1", status: "queued" }),
    });
  });
  let polls = 0;
  await page.route(
    "**/v1/warehouses/wh-pdx/reconciliation/runs/run-1",
    (route) => {
      polls += 1;
      return route.fulfill({
        contentType: "application/json",
        body: JSON.stringify(
          polls < 2
            ? { id: "run-1", status: "running" }
            : {
                id: "run-1",
                status: "succeeded",
                version: complete.version,
                result: complete,
              },
        ),
      });
    },
  );
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  await expect(
    page.getByText("Inventory ledger reconciles", { exact: true }),
  ).toBeVisible();
  expect(calls).toEqual(["direct", "POST run"]);
  expect(polls).toBe(2);
});

test("ledger reports a failed background run", async ({ page }) => {
  await setup(page);
  await page.route("**/v1/warehouses/wh-pdx/reconciliation", (route) =>
    route.fulfill({
      status: 503,
      contentType: "application/json",
      body: JSON.stringify({ error: "Database operation unavailable" }),
    }),
  );
  await page.route("**/v1/warehouses/wh-pdx/reconciliation/runs", (route) =>
    route.fulfill({
      status: 202,
      contentType: "application/json",
      body: JSON.stringify({ id: "run-2", status: "queued" }),
    }),
  );
  await page.route(
    "**/v1/warehouses/wh-pdx/reconciliation/runs/run-2",
    (route) =>
      route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          id: "run-2",
          status: "failed",
          error: "Abandoned: the run did not finish before its deadline",
        }),
      }),
  );
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  await expect(
    page.getByText("Full reconciliation unavailable", { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Abandoned: the run did not finish before its deadline"),
  ).toBeVisible();
});
