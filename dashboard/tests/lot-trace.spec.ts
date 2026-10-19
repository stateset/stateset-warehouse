// Intercepted HTTP contracts for trace review, pagination and exports. These do
// not prove SQL/HTTP integration; lot-trace-live.spec.ts exercises that boundary.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), "..");
const sku = "TRACE-SKU",
  lot = "TRACE-LOT";
function domain(input?: unknown) {
  const result = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    {
      input: input ? JSON.stringify(input) : "",
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    },
  );
  if (result.status !== 0) throw Error(result.stderr);
  return JSON.parse(result.stdout);
}
type Fault =
  | "wrong_scope"
  | "missing_page_proof"
  | "false_page_proof"
  | "broken_source"
  | "negative_ledger"
  | "changed_page";
async function setup(page: Page, fault?: Fault) {
  const state = domain();
  const requests: URL[] = [];
  let revision = "a".repeat(64),
    changed = false;
  const counts = {
    facilities: 26,
    positions: 2,
    allocations: 0,
    receipts: 0,
    shipments: 2,
    transfers: 0,
    movements: 0,
    policies: 0,
    decisions: 0,
  };
  const item = (warehouse_id: string, key: string[], document: object) => ({
    warehouse_id,
    key,
    document,
    source_ok: true,
  });
  const facilities = Array.from({ length: 26 }, (_, n) =>
    item(`wh-${String(n).padStart(2, "0")}`, [], {
      warehouse: {
        id: `wh-${String(n).padStart(2, "0")}`,
        code: `FAC-${n}`,
        name: `Trace facility ${n}`,
        city: "Vancouver",
      },
      version: 5,
      expiry_required: false,
      control: null,
      availability: "available",
      historic_eligibility_unknown_through_version: 2,
      balance: {
        physical: n === 0 ? 2 : 0,
        reserved: n === 0 ? 1 : 0,
        usable_storage: n === 0 ? 1 : 0,
        physical_reconciliation_ok: true,
      },
    }),
  );
  const records: Record<string, any[]> = {
    facilities,
    positions: [
      item("wh-00", ["A-01"], {
        location: "A-01",
        kind: "storage",
        physical: 1,
        reserved: 0,
        ledger: 1,
        allocation_reserved: 0,
        usable_storage: 1,
        availability: "available",
      }),
      item("wh-00", ["STAGE-01"], {
        location: "STAGE-01",
        kind: "staging",
        physical: 1,
        reserved: 1,
        ledger: 1,
        allocation_reserved: 1,
        usable_storage: 0,
        availability: "available",
      }),
    ],
    shipments: [
      item("wh-00", ["original", "legacy-order", "legacy-line", "A-01"], {
        order_id: "legacy-order",
        line_id: "legacy-line",
        source_location: "A-01",
        quantity: 1,
        state: "handed_off",
        source_kind: "legacy_snapshot",
        event_id: null,
        source_version: 0,
        carrier: "Retained legacy carrier",
        handoff_reference: "LEGACY",
        shipped_at: "2025-01-01T00:00:00Z",
      }),
      item("wh-00", ["original", "actual-order", "actual-line", "A-01"], {
        order_id: "actual-order",
        line_id: "actual-line",
        source_location: "A-01",
        quantity: 1,
        state: "handed_off",
        source_kind: "event",
        event_id: "actual-original-event",
        source_version: 4,
        carrier: "Actual carrier",
        handoff_reference: "ACTUAL",
        shipped_at: "2026-01-01T00:00:00Z",
      }),
    ],
  };
  await page.route("**/*", async (route) => {
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
          tenant: "trace-test",
          actor: "trace:viewer",
          role: "viewer",
        },
        demo: true,
        integrations: [],
      });
    if (path === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
      });
    if (path.endsWith("/outbox")) return send({ items: [] });
    if (path === `/v1/warehouses/${state.warehouse.id}`)
      return send({ ...state, reconciliation: domain({ state }) });
    if (path === "/v1/lot-trace") {
      requests.push(url);
      if (
        fault === "changed_page" &&
        url.searchParams.has("after") &&
        !changed
      ) {
        changed = true;
        revision = "b".repeat(64);
        return send(
          { error: "Lot trace source changed; refresh the complete review" },
          409,
        );
      }
      const section = url.searchParams.get("section")!,
        start = url.searchParams.has("after") ? 25 : 0,
        all = records[section] ?? [];
      const items = structuredClone(all.slice(start, start + 25));
      if (fault === "broken_source" && section === "shipments")
        items[0].source_ok = false;
      if (fault === "negative_ledger" && section === "positions") {
        items[0].document.ledger = -1;
        items[0].document.usable_storage = 0;
        items[0].source_ok = false;
      }
      const value: any = {
        schema_version: 1,
        sku: fault === "wrong_scope" ? "OTHER-SKU" : sku,
        lot,
        review: revision,
        checked_at: "2026-10-18T20:00:00.000001Z",
        section,
        items,
        next_cursor: all.length > start + 25 ? "original-trace-cursor" : null,
        page_source_ok: items.every((i) => i.source_ok),
        source_checks:
          "physical_and_reservation_reconstruction_plus_selected_page_sources",
        limits: ["Explicit test fixture; not a physical trial"],
        summary: {
          physical: 2,
          reserved: 1,
          usable_storage: 1,
          receiving: 0,
          staged: 1,
          physical_reconciliation_ok: true,
          sections: counts,
        },
      };
      if (fault === "missing_page_proof") delete value.page_source_ok;
      if (fault === "negative_ledger" && section === "positions") {
        value.summary.physical_reconciliation_ok = false;
        value.summary.usable_storage = 0;
      }
      if (fault === "false_page_proof") {
        items[0].source_ok = false;
        value.page_source_ok = true;
      }
      return send(value);
    }
    if (path.startsWith("/v1/"))
      return send({ error: "Unhandled intercepted route" }, 404);
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
  const panel = page.locator("details").filter({
    has: page.locator("summary", {
      hasText: "Trace a lot across facilities",
    }),
  });
  await panel.locator("summary").click();
  await panel.getByLabel("Trace SKU", { exact: true }).fill(sku);
  await panel.getByLabel("Trace lot", { exact: true }).fill(lot);
  await panel
    .getByRole("button", { name: "Review lot trace", exact: true })
    .click();
  return { panel, requests };
}
async function exported(page: Page, panel: ReturnType<Page["locator"]>) {
  const download = page.waitForEvent("download");
  await panel
    .getByRole("button", { name: "Export loaded trace records", exact: true })
    .click();
  return JSON.parse(readFileSync((await (await download).path())!, "utf8"));
}
test("viewer traces pages without claiming a partial section is a complete cross-facility report", async ({
  page,
}) => {
  const { panel, requests } = await setup(page);
  await expect(panel.getByRole("status")).toContainText("Showing 25 of 26");
  const partial = await exported(page, panel);
  expect(partial.section_complete).toBe(false);
  expect(partial.global_trace_complete).toBe(false);
  expect(partial.loaded_records).toBe(25);
  await panel
    .getByRole("button", { name: "Load more trace records", exact: true })
    .click();
  await expect(panel.getByRole("status")).toContainText("Showing 26 of 26");
  const full = await exported(page, panel);
  expect(full.section_complete).toBe(true);
  expect(full.global_trace_complete).toBe(false);
  expect(full.items).toHaveLength(26);
  expect(full.pages).toHaveLength(2);
  expect(requests[1].searchParams.get("review")).toBe(
    requests[0].searchParams.get("review") ?? "a".repeat(64),
  );
  expect(requests[1].searchParams.get("after")).toBe("original-trace-cursor");
});
test("a changed source discards old trace pages and requires an explicit fresh review", async ({
  page,
}) => {
  const { panel, requests } = await setup(page, "changed_page");
  await expect(
    panel.getByRole("table", { name: "Lot trace records" }),
  ).toBeVisible();
  await panel
    .getByRole("button", { name: "Load more trace records", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText(
    "Old pages have been discarded",
  );
  await expect(
    panel.getByRole("table", { name: "Lot trace records" }),
  ).toHaveCount(0);
  expect(requests).toHaveLength(2);
  await panel
    .getByRole("button", { name: "Review lot trace", exact: true })
    .click();
  const fresh = await exported(page, panel);
  expect(fresh.review).toBe("b".repeat(64));
  expect(fresh.items).toHaveLength(25);
  expect(fresh.pages).toHaveLength(1);
});
for (const fault of [
  "wrong_scope",
  "missing_page_proof",
  "false_page_proof",
] as const)
  test(`lot trace rejects unverifiable page: ${fault}`, async ({ page }) => {
    const { panel } = await setup(page, fault);
    await expect(panel.getByRole("alert")).toContainText(
      "could not be verified",
    );
    await expect(
      panel.getByRole("table", { name: "Lot trace records" }),
    ).toHaveCount(0);
    await expect(
      panel.getByRole("button", {
        name: "Export loaded trace records",
        exact: true,
      }),
    ).toHaveCount(0);
  });
test("trace labels retained legacy shipment baselines separately from original recorded handoffs", async ({
  page,
}) => {
  const { panel } = await setup(page);
  await panel
    .getByRole("combobox", { name: "Trace records", exact: true })
    .selectOption("shipments");
  await expect(
    panel.getByText("Legacy shipment baseline", { exact: true }),
  ).toBeVisible();
  await expect(
    panel.getByText("Recorded carrier handoff", { exact: true }),
  ).toBeVisible();
  await expect(
    panel.getByText("Retained baseline; no newly verified physical handoff.", {
      exact: true,
    }),
  ).toBeVisible();
  const report = await exported(page, panel);
  expect(report.section).toBe("shipments");
  expect(
    report.items.find((i: any) => i.document.source_kind === "legacy_snapshot")
      .document.event_id,
  ).toBeNull();
});
test("balanced quantities do not hide a failed original shipment source in trace or its export", async ({
  page,
}) => {
  const { panel } = await setup(page, "broken_source");
  await panel
    .getByRole("combobox", { name: "Trace records", exact: true })
    .selectOption("shipments");
  await expect(panel.getByRole("alert")).toContainText(
    "original source records need reconciliation",
  );
  const report = await exported(page, panel);
  expect(report.summary.physical_reconciliation_ok).toBe(true);
  expect(report.items[0].source_ok).toBe(false);
  expect(report.pages[0].page_source_ok).toBe(false);
  expect(report.global_trace_complete).toBe(false);
});

test("trace retains signed divergent ledger evidence as an explicit reconciliation incident", async ({
  page,
}) => {
  const { panel } = await setup(page, "negative_ledger");
  await panel
    .getByRole("combobox", { name: "Trace records", exact: true })
    .selectOption("positions");
  await expect(panel.getByRole("alert").first()).toContainText(
    "Physical custody or reservation evidence disagrees",
  );
  await expect(
    panel.getByRole("table", { name: "Lot trace records" }),
  ).toContainText("-1 / 0");
  const report = await exported(page, panel);
  expect(report.items[0].document.ledger).toBe(-1);
  expect(report.items[0].source_ok).toBe(false);
  expect(report.summary.physical_reconciliation_ok).toBe(false);
});
