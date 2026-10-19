// Actual built browser → authenticated API → PostgreSQL, without interception.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
test("facility handoff remains in transit until exact partial destination arrivals and original replays", async ({
  page,
}) => {
  test.setTimeout(90000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` };
  const suffix = randomUUID().slice(0, 8);
  const source = `xfer-src-${suffix}`,
    destination = `xfer-dst-${suffix}`,
    sku = `XFER-${suffix}`;
  const get = async (path: string) => {
    const r = await page.request.get(path, { headers });
    expect(r.ok(), await r.text()).toBe(true);
    const body = await r.json();
    if (/^\/v1\/warehouses\/[^/]+$/.test(path)) {
      // This is a fresh read observation, not persisted warehouse evidence.
      const { availability_checked_at, ...persisted } = body;
      expect(availability_checked_at).toMatch(
        /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6}Z$/,
      );
      expect(Number.isFinite(Date.parse(availability_checked_at))).toBe(true);
      return persisted;
    }
    return body;
  };
  const submit = async (w: string, command: unknown, version?: number) => {
    const v = version ?? (await get(`/v1/warehouses/${w}`)).version;
    const r = await page.request.post(`/v1/warehouses/${w}/commands`, {
      headers,
      data: { request_id: randomUUID(), expected_version: v, command },
    });
    expect(r.ok(), await r.text()).toBe(true);
    return r.json();
  };
  for (const [w, code] of [
    [source, `SRC-${suffix}`],
    [destination, `DST-${suffix}`],
  ]) {
    await submit(
      w,
      {
        type: "create_warehouse",
        code,
        name: code,
        city: "Vancouver",
        timezone: "America/Vancouver",
      },
      0,
    );
    await submit(w, {
      type: "create_product",
      sku,
      name: "Synthetic physical transfer",
      category: "General",
      reorder_point: 0,
    });
  }
  await submit(source, {
    type: "create_location",
    code: "TRANSFER-A",
    zone: "Storage",
    kind: "storage",
  });
  await submit(source, {
    type: "count_stock",
    sku,
    lot: "ORIGINAL-LOT",
    location: "TRANSFER-A",
    counted: 10,
    reason: "Witnessed synthetic opening count",
  });
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");
  await page.getByLabel("API credential", { exact: true }).fill(token);
  await page
    .getByRole("button", { name: "Connect to workspace", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Warehouse overview", exact: true }),
  ).toBeVisible();
  const change = async (w: string) => {
    await page
      .getByRole("button", { name: "Change warehouse", exact: true })
      .click();
    await expect(
      page.getByLabel("Active warehouse", { exact: true }),
    ).toBeEnabled();
    await page.getByLabel("Active warehouse", { exact: true }).selectOption(w);
    await expect(page.locator(".workspace-card strong")).toHaveText(
      w === source ? `SRC-${suffix}` : `DST-${suffix}`,
    );
    await expect(
      page.getByRole("button", { name: "Facility transfers", exact: true }),
    ).toBeEnabled();
    await page
      .getByRole("button", { name: "Facility transfers", exact: true })
      .click();
  };
  await change(source);
  await page.getByText("New facility transfer", { exact: true }).click();
  await expect(
    page.getByLabel("Destination facility", { exact: true }),
  ).toBeEnabled();
  await page
    .getByLabel("Destination facility", { exact: true })
    .selectOption(destination);
  await page
    .getByLabel("Transfer reference", { exact: true })
    .fill(`HANDOFF-${suffix}`);
  await page
    .getByLabel("Transfer carrier", { exact: true })
    .fill("Synthetic carrier");
  await page
    .getByLabel("Carrier handoff reference", { exact: true })
    .fill("ORIGINAL-HANDOFF");
  await page
    .getByLabel("Source stock 1", { exact: true })
    .selectOption(JSON.stringify([sku, "ORIGINAL-LOT", "TRANSFER-A"]));
  await page.getByLabel("Dispatch each units 1", { exact: true }).fill("3");
  await page
    .getByRole("button", { name: "Review transfer dispatch", exact: true })
    .click();
  const sent = page.waitForRequest(
    (r) =>
      r.method() === "POST" &&
      r.url().endsWith("/commands") &&
      r.postDataJSON()?.command?.type === "dispatch_transfer",
  );
  await page
    .getByRole("button", { name: "Record carrier handoff", exact: true })
    .click();
  const original = await sent;
  const dispatch = await (await original.response())!.json();
  expect(dispatch.replayed).toBe(false);
  await expect(
    page.getByRole("status").filter({ hasText: "Dispatch recorded" }),
  ).toBeVisible();
  const tid = dispatch.result.entity_id,
    line = original.postDataJSON().command.lines[0].id;
  expect((await get(`/v1/warehouses/${source}`)).stocks[0].on_hand).toBe(7);
  expect((await get(`/v1/warehouses/${destination}`)).stocks).toEqual([]);
  const inTransit = await get(`/v1/warehouses/${source}/reconciliation`);
  expect(inTransit.ok).toBe(true);
  expect(inTransit.transfers[0].lines[0].in_transit).toBe(3);
  await page
    .getByRole("button", { name: "Movement ledger", exact: true })
    .click();
  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export CSV", exact: true }).click();
  const csv = readFileSync((await (await download).path())!, "utf8");
  for (const field of ["transfer_id", "transfer_line_id", "transfer_event_id"])
    expect(csv.split("\r\n")[0]).toContain(`"${field}"`);
  for (const value of [tid, line, inTransit.transfers[0].source_event_id])
    expect(csv).toContain(`"${value}"`);
  await change(destination);
  await page
    .getByRole("button", {
      name: `Receive transfer HANDOFF-${suffix} line ${line}`,
      exact: true,
    })
    .click();
  await page.getByLabel("Arrived each units", { exact: true }).fill("2");
  await page
    .getByLabel("Arrival evidence reference", { exact: true })
    .fill("Witnessed two arrived each");
  const arrived = page.waitForRequest(
    (r) =>
      r.method() === "POST" &&
      r.postDataJSON()?.command?.type === "receive_transfer",
  );
  await page
    .getByRole("button", { name: "Record transfer arrival", exact: true })
    .click();
  const arrival = await arrived;
  const arrivalResult = await (await arrival.response())!.json();
  await expect(
    page.getByRole("status").filter({ hasText: "Arrival recorded" }),
  ).toBeVisible();
  const after = await get(`/v1/warehouses/${destination}`);
  expect(after.stocks[0]).toMatchObject({
    sku,
    lot: "ORIGINAL-LOT",
    location: "RECV-01",
    on_hand: 2,
    reserved: 0,
  });
  const proof = await get(`/v1/warehouses/${destination}/reconciliation`);
  expect(proof.ok).toBe(true);
  expect(proof.transfers[0].lines[0]).toMatchObject({
    dispatched: 3,
    received: 2,
    in_transit: 1,
  });
  for (const [r, result] of [
    [original, dispatch],
    [arrival, arrivalResult],
  ] as const) {
    const replay = await page.request.post(r.url(), {
      headers,
      data: r.postDataJSON(),
    });
    expect(replay.ok()).toBe(true);
    expect(await replay.json()).toEqual({
      replayed: true,
      result: result.result,
    });
  }
  expect(await get(`/v1/warehouses/${destination}`)).toEqual(after);
  const over = await page.request.post(
    `/v1/warehouses/${destination}/commands`,
    {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: after.version,
        command: {
          type: "receive_transfer",
          transfer_id: tid,
          line_id: line,
          quantity: 2,
          location: "RECV-01",
          arrival_reference: "Excess must be held",
        },
      },
    },
  );
  expect(over.status()).toBe(422);
  // Restore a journal after the server committed but before a browser could
  // clear it. Recovery uses real receipt/HTTP routes, without interception.
  const session = await get("/v1/session");
  for (const [originalRequest, warehouse] of [
    [original, source],
    [arrival, destination],
  ] as const) {
    await page.evaluate(
      ({ key, journal }) =>
        sessionStorage.setItem(key, JSON.stringify(journal)),
      {
        key: `wms-pending:${session.identity.tenant}:${session.identity.actor}`,
        journal: {
          tenant: session.identity.tenant,
          actor: session.identity.actor,
          warehouse,
          request: originalRequest.postDataJSON(),
        },
      },
    );
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toHaveCount(0);
    await expect(page.locator(".workspace-card strong")).toHaveText(
      warehouse === source ? `SRC-${suffix}` : `DST-${suffix}`,
    );
  }
  expect(await get(`/v1/warehouses/${destination}`)).toEqual(after);
  await page.reload();
  await page
    .getByRole("button", { name: "Facility transfers", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: `HANDOFF-${suffix}`, exact: true }),
  ).toBeVisible();
  expect((await get(`/v1/warehouses/${source}`)).stocks[0].on_hand).toBe(7);
  expect(errors).toEqual([]);
});
