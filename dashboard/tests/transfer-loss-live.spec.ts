// Actual built browser → canonical authenticated HTTP → PostgreSQL. No interception.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
test("confirmed transfer loss and exact reversal retain custody until actual recovered arrival", async ({
  page,
}, testInfo) => {
  test.setTimeout(120000);
  const token =
      process.env.WMS_SCANNER_OPERATOR_TOKEN ||
      "scanner-live-operator-credential-0001",
    headers = { Authorization: `Bearer ${token}` };
  const suffix = randomUUID().slice(0, 8),
    source = `loss-src-${suffix}`,
    destination = `loss-dst-${suffix}`,
    sku = `LOSS-${suffix}`,
    reference = `LOSS-XFER-${suffix}`;
  const get = async (path: string) => {
    const r = await page.request.get(path, { headers });
    expect(r.ok(), await r.text()).toBe(true);
    const body = await r.json();
    if (/^\/v1\/warehouses\/[^/]+$/.test(path)) {
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
    const r = await page.request.post(`/v1/warehouses/${w}/commands`, {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: version ?? (await get(`/v1/warehouses/${w}`)).version,
        command,
      },
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
      name: "Synthetic transfer loss product",
      category: "Test",
      reorder_point: 0,
    });
  }
  await submit(source, {
    type: "create_location",
    code: "LOSS-A",
    zone: "Storage",
    kind: "storage",
  });
  await submit(source, {
    type: "count_stock",
    sku,
    lot: "LOSS-LOT",
    location: "LOSS-A",
    counted: 10,
    reason: "Synthetic opening custody",
  });
  const dispatched = await submit(source, {
    type: "dispatch_transfer",
    reference,
    destination_warehouse_id: destination,
    carrier: "Synthetic carrier",
    handoff_reference: `ORIGINAL-${suffix}`,
    lines: [
      {
        id: "original-a",
        sku,
        lot: "LOSS-LOT",
        location: "LOSS-A",
        quantity: 4,
      },
    ],
  });
  const tid = dispatched.result.entity_id;
  await submit(source, {
    type: "quarantine_lot",
    sku,
    lot: "LOSS-LOT",
    reason: "Actual synthetic source restriction after original dispatch",
  });
  await submit(destination, {
    type: "receive_transfer",
    transfer_id: tid,
    line_id: "original-a",
    quantity: 1,
    location: "RECV-01",
    arrival_reference: "One actual initial each",
  });
  const sourceBefore = await get(`/v1/warehouses/${source}`),
    before = await get(`/v1/warehouses/${destination}`);
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
  await page
    .getByRole("button", { name: "Change warehouse", exact: true })
    .click();
  await expect(
    page.getByLabel("Active warehouse", { exact: true }),
  ).toBeEnabled();
  await page
    .getByLabel("Active warehouse", { exact: true })
    .selectOption(destination);
  await expect(page.locator(".workspace-card strong")).toHaveText(
    `DST-${suffix}`,
  );
  await page
    .getByRole("button", { name: "Facility transfers", exact: true })
    .click();
  const panel = page.getByRole("region", {
    name: `Loss records for ${reference}`,
  });
  const saved: { body: any; result: any }[] = [];
  const capture = async (type: string, action: () => Promise<unknown>) => {
    const sent = page.waitForRequest(
      (r) =>
        r.method() === "POST" &&
        r.url().endsWith("/commands") &&
        r.postDataJSON()?.command?.type === type,
    );
    await action();
    const request = await sent;
    const response = await request.response();
    expect(response!.ok(), await response!.text()).toBe(true);
    const result = await response!.json();
    saved.push({ body: request.postDataJSON(), result: result.result });
    return result;
  };
  const openJournal = async () => {
    await expect(panel).toBeVisible();
    const summary = panel.locator("summary");
    if (
      !(await panel
        .locator("details")
        .evaluate((e) => (e as HTMLDetailsElement).open))
    )
      await summary.click();
    await panel
      .getByRole("button", { name: "Review loss journal", exact: true })
      .click();
  };
  const loss = async (q: number, reason: string) => {
    await openJournal();
    await panel
      .getByRole("button", {
        name: "Review loss for line original-a",
        exact: true,
      })
      .click();
    await panel
      .getByLabel("Loss quantity each", { exact: true })
      .fill(String(q));
    await panel
      .getByLabel("Carrier incident reference", { exact: true })
      .fill(`CARRIER-CASE-${suffix}`);
    await panel.getByLabel("Loss review reason", { exact: true }).fill(reason);
    await panel.getByRole("checkbox").check();
    return capture("record_transfer_loss", () =>
      panel
        .getByRole("button", { name: "Record confirmed loss", exact: true })
        .click(),
    );
  };
  const first = await loss(
    2,
    "Carrier and controller confirmed two missing original each units",
  );
  await expect
    .poll(
      async () =>
        (await get(`/v1/warehouses/${destination}/transfers?open_only=false`))
          .items[0].data.lines[0].lost,
    )
    .toBe(2);
  let now = await get(`/v1/warehouses/${destination}`);
  expect(now.stocks).toEqual(before.stocks);
  expect(now.movements).toEqual(before.movements);
  expect(await get(`/v1/warehouses/${source}`)).toEqual(sourceBefore);
  await loss(1, "Final original missing unit confirmed separately");
  await expect
    .poll(
      async () =>
        (await get(`/v1/warehouses/${destination}/transfers?open_only=true`))
          .items.length,
    )
    .toBe(0);
  await page.getByLabel("Include completed transfers", { exact: true }).check();
  await expect(
    page.getByText(
      "Transit settled with recorded loss. This is not a complete physical arrival.",
      { exact: true },
    ),
  ).toBeVisible();
  const over = await page.request.post(
    `/v1/warehouses/${destination}/commands`,
    {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: (await get(`/v1/warehouses/${destination}`)).version,
        command: {
          type: "receive_transfer",
          transfer_id: tid,
          line_id: "original-a",
          quantity: 1,
          location: "RECV-01",
          arrival_reference: "Settled units cannot also arrive",
        },
      },
    },
  );
  expect(over.status()).toBe(422);
  await openJournal();
  await panel
    .getByRole("button", {
      name: `Review reversal of loss ${first.result.entity_id}`,
      exact: true,
    })
    .click();
  await panel
    .getByLabel("Loss review reason", { exact: true })
    .fill(
      "One original missing unit located; actual destination receipt remains separate",
    );
  await panel.getByRole("checkbox").check();
  await capture("reverse_transfer_loss", () =>
    panel
      .getByRole("button", { name: "Record loss reversal", exact: true })
      .click(),
  );
  await expect
    .poll(
      async () =>
        (await get(`/v1/warehouses/${destination}/transfers?open_only=true`))
          .items[0]?.data.lines[0].lost,
    )
    .toBe(2);
  now = await get(`/v1/warehouses/${destination}`);
  expect(now.stocks).toEqual(before.stocks);
  expect(now.movements).toEqual(before.movements);
  await page
    .getByRole("button", {
      name: `Receive transfer ${reference} line original-a`,
      exact: true,
    })
    .click();
  await page.getByLabel("Arrived each units", { exact: true }).fill("1");
  await page
    .getByLabel("Arrival evidence reference", { exact: true })
    .fill("Actual recovered original unit observed at destination");
  await capture("receive_transfer", () =>
    page
      .getByRole("button", { name: "Record transfer arrival", exact: true })
      .click(),
  );
  const final = await get(`/v1/warehouses/${destination}`);
  expect(final.stocks.reduce((n: number, s: any) => n + s.on_hand, 0)).toBe(2);
  expect(await get(`/v1/warehouses/${source}`)).toEqual(sourceBefore);
  const proof = await get(`/v1/warehouses/${destination}/reconciliation`);
  expect(proof.ok).toBe(true);
  expect(proof.transfers[0].lines[0]).toMatchObject({
    dispatched: 4,
    received: 2,
    lost: 2,
    in_transit: 0,
    loss_record_count: 3,
  });
  const journal = await get(
    `/v1/warehouses/${destination}/transfers/${tid}/losses`,
  );
  expect(journal.page_source_ok).toBe(true);
  expect(journal.items).toHaveLength(3);
  expect(journal.items[0]).toMatchObject({
    id: first.result.entity_id,
    quantity: 2,
    reversed: 1,
  });
  expect(journal.items[2].original_loss_id).toBe(first.result.entity_id);
  expect(
    final.lot_controls.some((l: any) => l.sku === sku && l.holds.length > 0),
  ).toBe(true);
  const session = await get("/v1/session");
  for (const original of saved) {
    const replay = await page.request.post(
      `/v1/warehouses/${destination}/commands`,
      { headers, data: original.body },
    );
    expect(replay.ok()).toBe(true);
    expect(await replay.json()).toEqual({
      replayed: true,
      result: original.result,
    });
  }
  for (const original of saved.filter((r) =>
    ["record_transfer_loss", "reverse_transfer_loss"].includes(
      r.body.command.type,
    ),
  )) {
    await page.evaluate(
      ({ key, journal }) =>
        sessionStorage.setItem(key, JSON.stringify(journal)),
      {
        key: `wms-pending:${session.identity.tenant}:${session.identity.actor}`,
        journal: {
          tenant: session.identity.tenant,
          actor: session.identity.actor,
          warehouse: destination,
          request: original.body,
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
  }
  expect(await get(`/v1/warehouses/${destination}`)).toEqual(final);
  await page
    .getByRole("button", { name: "Facility transfers", exact: true })
    .click();
  await page.getByLabel("Include completed transfers", { exact: true }).check();
  await openJournal();
  await expect(
    panel.getByText(/Original loss reversal · 1 each/),
  ).toBeVisible();
  await page.screenshot({
    path: testInfo.outputPath("transfer-loss-desktop.png"),
    fullPage: true,
  });
  await page.setViewportSize({ width: 390, height: 844 });
  const overflowing = await page.locator("body *").evaluateAll((nodes) =>
    nodes
      .filter((n) => n.getBoundingClientRect().right > window.innerWidth + 0.5)
      .map((n) => ({
        tag: n.tagName,
        className: n.className,
        right: n.getBoundingClientRect().right,
        width: n.getBoundingClientRect().width,
      })),
  );
  await testInfo.attach("mobile-overflow.json", {
    body: JSON.stringify(overflowing, null, 2),
    contentType: "application/json",
  });
  await page.screenshot({
    path: testInfo.outputPath("transfer-loss-mobile.png"),
    fullPage: true,
  });
  await expect(page.locator("html")).toHaveJSProperty("scrollWidth", 390);
  expect(errors).toEqual([]);
});
