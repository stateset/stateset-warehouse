// Intercepted browser contracts; transfer-loss-live.spec.ts covers actual HTTP/PG.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), "..");
function domain(input?: unknown) {
  const r = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    {
      input: input ? JSON.stringify(input) : "",
      encoding: "utf8",
      maxBuffer: 16 * 1024 * 1024,
    },
  );
  if (r.status !== 0) throw Error(r.stderr);
  return JSON.parse(r.stdout);
}
async function setup(
  page: Page,
  options: {
    role?: string;
    loss?: "before" | "after";
    fault?: "source" | "changed_page";
  } = {},
) {
  const state = domain();
  state.version += 100;
  const requests: any[] = [];
  let committed = 0,
    dropped = false,
    changed = false;
  const records: any[] = [];
  const receipts = new Map<string, any>();
  const transfer = {
    id: "original-transfer",
    reference: "LOSS-TRANSFER",
    source_warehouse_id: "wh-source",
    destination_warehouse_id: state.warehouse.id,
    carrier: "Synthetic carrier",
    handoff_reference: "ORIGINAL-HANDOFF",
    ok: true,
    lines: [
      {
        id: "a",
        sku: state.products[0].sku,
        lot: "ORIGINAL-LOT",
        location: "SOURCE-A",
        quantity: 3,
        received: 0,
        lost: 0,
        loss_record_count: 0,
      },
    ],
  };
  const record = (version: number) => ({
    id: `loss-${version}`,
    transfer_id: transfer.id,
    line_id: "a",
    warehouse_id: state.warehouse.id,
    kind: "loss",
    quantity: 1,
    reversed: 1,
    original_loss_id: null,
    incident_reference: `CASE-${version}`,
    reason: "Original reviewed incident",
    actor: "review:admin",
    recorded_at: "2026-10-18T20:00:00Z",
    version,
    event_id: `event-${version}`,
    source_ok: true,
  });
  if (options.fault === "changed_page") {
    for (let n = 1; n <= 26; n++) records.push(record(n));
    transfer.lines[0].loss_record_count = 26;
  }
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
          tenant: "loss-contract",
          actor: "operator:demo",
          role: options.role ?? "admin",
        },
        demo: true,
        integrations: [],
      });
    if (path === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
        next_after: null,
      });
    if (path === `/v1/warehouses/${state.warehouse.id}`)
      return send({ ...state, reconciliation: domain({ state }) });
    if (path.endsWith("/outbox")) return send({ items: [] });
    if (path.endsWith("/transfers"))
      return send({
        warehouse_id: state.warehouse.id,
        version: state.version,
        items: [{ cursor: 1, data: transfer }],
        next_cursor: null,
      });
    if (path.endsWith("/losses")) {
      if (
        options.fault === "changed_page" &&
        url.searchParams.has("after") &&
        !changed
      ) {
        changed = true;
        ++state.version;
        return send(
          { error: "Destination changed; discard original loss pages" },
          409,
        );
      }
      const after = Number(url.searchParams.get("after") ?? 0),
        items = records.filter((r) => r.version > after).slice(0, 25);
      return send({
        warehouse_id: state.warehouse.id,
        version: state.version,
        destination_warehouse_id: state.warehouse.id,
        transfer_id: transfer.id,
        review: state.version,
        items,
        next_cursor:
          records.filter((r) => r.version > after).length > 25
            ? items.at(-1).version
            : null,
        page_source_ok: options.fault !== "source",
      });
    }
    if (path.includes("/commands/")) {
      const result = receipts.get(path.split("/").at(-1)!);
      return send(result ? { found: true, result } : { found: false });
    }
    if (path.endsWith("/commands") && route.request().method() === "POST") {
      const body = route.request().postDataJSON();
      requests.push(body);
      if (!dropped && options.loss === "before") {
        dropped = true;
        return route.abort("connectionreset");
      }
      if (receipts.has(body.request_id))
        return send({ replayed: true, result: receipts.get(body.request_id) });
      expect(body.expected_version).toBe(state.version);
      const cmd = body.command;
      const r = {
        ...record(++state.version),
        id: `loss-${state.version}`,
        quantity: cmd.quantity,
        reversed: 0,
        reason: cmd.reason,
        incident_reference: cmd.incident_reference ?? null,
        original_loss_id: cmd.loss_id ?? null,
        kind: cmd.type === "record_transfer_loss" ? "loss" : "reversal",
      };
      if (r.kind === "loss") transfer.lines[0].lost += r.quantity;
      else {
        records.find((r) => r.id === cmd.loss_id).reversed += r.quantity;
        transfer.lines[0].lost -= r.quantity;
      }
      records.push(r);
      transfer.lines[0].loss_record_count++;
      committed++;
      const result = {
        entity_id: r.id,
        version: state.version,
        type:
          r.kind === "loss"
            ? "transfer.loss_recorded"
            : "transfer.loss_reversed",
      };
      receipts.set(body.request_id, result);
      if (!dropped && options.loss === "after") {
        dropped = true;
        return route.abort("connectionreset");
      }
      return send({ replayed: false, result });
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
  await page
    .getByRole("button", { name: "Facility transfers", exact: true })
    .click();
  const panel = page.getByRole("region", {
    name: "Loss records for LOSS-TRANSFER",
  });
  await panel.locator("summary").click();
  await panel
    .getByRole("button", { name: "Review loss journal", exact: true })
    .click();
  return {
    panel,
    requests,
    state,
    records,
    transfer,
    committed: () => committed,
  };
}
async function draft(panel: ReturnType<Page["getByRole"]>) {
  await panel
    .getByRole("button", { name: "Review loss for line a", exact: true })
    .click();
  await panel.getByLabel("Loss quantity each", { exact: true }).fill("2");
  await panel
    .getByLabel("Carrier incident reference", { exact: true })
    .fill("CARRIER-CASE");
  await panel
    .getByLabel("Loss review reason", { exact: true })
    .fill("Carrier and controller confirmed two missing each units");
  await panel.getByRole("checkbox").check();
}
test("loss settlement and exact reversal preserve stock and display pending transit honestly", async ({
  page,
}) => {
  const f = await setup(page);
  const before = structuredClone(f.state.stocks);
  await draft(f.panel);
  await f.panel
    .getByRole("button", { name: "Record confirmed loss", exact: true })
    .click();
  await expect.poll(f.committed).toBe(1);
  expect(f.state.stocks).toEqual(before);
  expect(f.transfer.lines[0].lost).toBe(2);
  await f.panel
    .getByRole("button", { name: "Review loss journal", exact: true })
    .click();
  await expect(
    f.panel.getByText("Recorded loss · 2 each · Line a", { exact: true }),
  ).toBeVisible();
  await f.panel
    .getByRole("button", { name: /^Review reversal of loss / })
    .click();
  await f.panel
    .getByLabel("Loss review reason", { exact: true })
    .fill("One original missing unit located; receipt remains separate");
  await f.panel.getByRole("checkbox").check();
  await f.panel
    .getByRole("button", { name: "Record loss reversal", exact: true })
    .click();
  await expect.poll(f.committed).toBe(2);
  expect(f.state.stocks).toEqual(before);
  expect(f.transfer.lines[0].lost).toBe(1);
  expect(f.requests[1].command.loss_id).toBe(f.records[0].id);
  expect(f.requests[1].command.quantity).toBe(1);
});
for (const loss of ["before", "after"] as const)
  test(`loss save lost ${loss} arrival recovers only its original journal after reload`, async ({
    page,
  }) => {
    const f = await setup(page, { loss });
    await draft(f.panel);
    await f.panel
      .getByRole("button", { name: "Record confirmed loss", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeEnabled();
    const original = structuredClone(f.requests[0]);
    await expect(page.locator(".alert.warning")).toContainText(
      "Incident CARRIER-CASE",
    );
    await expect(page.locator(".alert.warning")).toContainText("Line a");
    const saved = await page.evaluate(() => {
      const keys = Object.keys(sessionStorage).filter((k) =>
        k.startsWith("wms-pending:"),
      );
      return JSON.parse(sessionStorage.getItem(keys[0])!);
    });
    expect(saved.request).toEqual(original);
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toHaveCount(0);
    expect(f.committed()).toBe(1);
    expect(f.transfer.lines[0].lost).toBe(2);
    expect(
      f.requests.every((r) => JSON.stringify(r) === JSON.stringify(original)),
    ).toBe(true);
  });
test("failed source proof holds loss changes while a viewer has no write controls", async ({
  page,
}) => {
  const f = await setup(page, { fault: "source" });
  await expect(f.panel.getByRole("alert")).toContainText(
    "needs reconciliation",
  );
  await expect(
    f.panel.getByRole("button", {
      name: "Review loss for line a",
      exact: true,
    }),
  ).toBeDisabled();
  expect(f.requests).toEqual([]);
});
test("viewers can inspect the loss journal without loss or reversal authority", async ({
  page,
}) => {
  const f = await setup(page, { role: "viewer" });
  await expect(
    f.panel.getByText("No loss records in this review.", { exact: true }),
  ).toBeVisible();
  await expect(
    f.panel.getByRole("button", { name: /Review loss for line/ }),
  ).toHaveCount(0);
  expect(f.requests).toEqual([]);
});
test("destination changes discard all retained loss pages before a fresh explicit review", async ({
  page,
}) => {
  const f = await setup(page, { fault: "changed_page" });
  await expect(f.panel.getByText(/Recorded loss · 1 each/)).toHaveCount(25);
  await f.panel
    .getByRole("button", { name: "Load older loss records", exact: true })
    .click();
  await expect(f.panel.getByRole("alert")).toContainText("Destination changed");
  await expect(f.panel.getByText(/Recorded loss · 1 each/)).toHaveCount(0);
  await expect(
    f.panel.getByRole("button", { name: /Review loss for line/ }),
  ).toHaveCount(0);
  expect(f.requests).toEqual([]);
});
