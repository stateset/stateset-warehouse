// Real browser→HTTP→PostgreSQL. Keyboard-wedge input is simulated; no physical
// scanner/device/facility acceptance is claimed. Only owned synthetic tenants.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
for (const mobile of [false, true])
  test(`serialized receiving and move retain exact labels through lost responses (${mobile ? "mobile" : "desktop"})`, async ({
    page,
  }) => {
    test.setTimeout(120000);
    page.setDefaultTimeout(10000);
    if (mobile) await page.setViewportSize({ width: 390, height: 844 });
    const credential =
      process.env.WMS_SCANNER_OPERATOR_TOKEN ||
      "scanner-live-operator-credential-0001";
    const headers = { Authorization: `Bearer ${credential}` },
      base = "/v1/warehouses/wh-main";
    const suffix = randomUUID().slice(0, 8),
      sku = `SERIAL-UI-${suffix}`,
      lot = `LOT-${suffix}`,
      bin = `BIN-${suffix}`,
      reference = `SERIAL-RCV-${suffix}`;
    const labels = [`Unit-${suffix}`, `unit-${suffix}`];
    const errors: string[] = [];
    page.on("pageerror", (e) => errors.push(e.message));
    const get = async (path = base) => {
      const r = await page.request.get(path, { headers });
      expect(r.ok(), await r.text()).toBe(true);
      return r.json();
    };
    const command = async (command: unknown) => {
      const state = await get();
      const r = await page.request.post(`${base}/commands`, {
        headers,
        data: {
          request_id: randomUUID(),
          expected_version: state.version,
          command,
        },
      });
      expect(r.ok(), await r.text()).toBe(true);
      return r.json();
    };
    const session = await get("/v1/session");
    expect(session.demo).toBe(false);
    expect(session.identity.tenant).toBe("scanner-qualification");
    await page.goto("/");
    await page.getByLabel("API credential", { exact: true }).fill(credential);
    await page
      .getByRole("button", { name: "Connect to workspace", exact: true })
      .click();
    await page.getByRole("button", { name: "Inventory", exact: true }).click();
    await page
      .getByRole("button", { name: "Add product", exact: true })
      .click();
    await page.getByLabel("SKU", { exact: true }).fill(sku);
    await page
      .getByLabel("Product name", { exact: true })
      .fill("Synthetic serial acceptance");
    await page
      .getByLabel("Track individual serials", { exact: true })
      .selectOption("true");
    await page
      .getByRole("button", { name: "Create product", exact: true })
      .click();
    await expect(
      page.getByRole("dialog", { name: "Add product" }),
    ).toBeHidden();
    expect(
      (await get()).products.find((p: any) => p.sku === sku).serial_controlled,
    ).toBe(true);
    await command({
      type: "create_location",
      code: bin,
      zone: "Synthetic",
      kind: "storage",
    });
    await command({
      type: "create_receipt",
      reference,
      supplier: "Synthetic serial supplier",
      lines: [{ id: "LINE", sku, lot, quantity: 2 }],
    });
    await page
      .getByRole("button", { name: "Refresh warehouse", exact: true })
      .click();
    await page.getByRole("button", { name: "Scanner", exact: true }).click();
    await page.getByLabel("Receipt reference", { exact: true }).fill(reference);
    await page.getByRole("button", { name: "Load work", exact: true }).click();
    for (const [label, value] of [
      ["bin", "RECV-01"],
      ["SKU", sku],
      ["lot", lot],
    ]) {
      const f = page.getByRole("textbox", {
        name: `Scan ${label}`,
        exact: true,
      });
      await f.fill(value);
      await f.press("Enter");
    }
    await page
      .getByLabel("Physical quantity (each)", { exact: true })
      .fill("2");
    await page
      .getByRole("button", { name: "Confirm scanned receipt", exact: true })
      .click();
    const scan = async (value: string) => {
      const f = page.getByRole("textbox", {
        name: `Scan serial for ${sku}`,
        exact: true,
      });
      await f.fill(value);
      await f.press("Enter");
    };
    await scan(labels[0]);
    await scan(labels[1]);
    // The API commits and returns its actual response, which this owned browser
    // deliberately loses. Recovery must find its original receipt, without POST.
    const received: any[] = [];
    let dropped = false;
    await page.route(`**${base}/commands`, async (route) => {
      received.push(route.request().postDataJSON());
      if (!dropped) {
        dropped = true;
        const response = await route.fetch();
        expect(response.ok(), await response.text()).toBe(true);
        await route.abort("failed");
      } else await route.continue();
    });
    await page
      .getByRole("button", { name: "Save with observed serials", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeVisible();
    const original = structuredClone(received[0]);
    expect(original.serial_inputs).toEqual([
      { target: [], sku, serials: labels },
    ]);
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeHidden();
    expect(received).toHaveLength(1);
    await page.unroute(`**${base}/commands`);
    let units = await get(`/v1/serials?sku=${encodeURIComponent(sku)}`);
    expect(units.page_source_ok).toBe(true);
    expect(units.items.map((u: any) => u.serial)).toEqual(labels);
    expect(units.items.every((u: any) => u.place.location === "RECV-01")).toBe(
      true,
    );
    await page.getByRole("button", { name: "Inventory", exact: true }).click();
    await page
      .getByRole("row")
      .filter({ hasText: sku })
      .getByRole("button", { name: "Move", exact: true })
      .click();
    await page
      .getByLabel("To storage location", { exact: true })
      .selectOption(bin);
    await page.getByLabel("Quantity (each)", { exact: true }).fill("2");
    await page
      .getByRole("button", { name: "Confirm movement", exact: true })
      .click();
    await scan(labels[0]);
    await scan(labels[1]);
    // Lose the next request before arrival. Retry must send exactly its original
    // labels, ID, command and captured warehouse/resource fences after reload.
    const moved: any[] = [];
    let lost = false;
    await page.route(`**${base}/commands`, async (route) => {
      moved.push(route.request().postDataJSON());
      if (!lost) {
        lost = true;
        await route.abort("failed");
      } else await route.continue();
    });
    await page
      .getByRole("button", { name: "Save with observed serials", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeVisible();
    const move = structuredClone(moved[0]);
    await page.reload();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeHidden();
    expect(moved).toHaveLength(2);
    expect(moved[1]).toEqual(move);
    expect(move.serial_inputs).toEqual(original.serial_inputs);
    units = await get(`/v1/serials?sku=${encodeURIComponent(sku)}`);
    expect(units.page_source_ok).toBe(true);
    expect(
      units.items.every(
        (u: any) => u.place.location === bin && u.revision === 2,
      ),
    ).toBe(true);
    await page.getByRole("button", { name: "Inventory", exact: true }).click();
    await page
      .getByText("Serial custody and original observations", { exact: true })
      .click();
    await page
      .getByRole("combobox", { name: "Serial SKU", exact: true })
      .selectOption(sku);
    await page
      .getByRole("button", { name: "Refresh serial review", exact: true })
      .click();
    await expect(
      page.getByRole("status").filter({ hasText: "Global SKU review" }),
    ).toContainText("2 of 2 retained records");
    await page
      .getByRole("combobox", { name: "Serial evidence view", exact: true })
      .selectOption("history");
    await page
      .getByRole("textbox", { name: "Literal serial (required)", exact: true })
      .fill(labels[0]);
    await page
      .getByRole("button", { name: "Refresh serial review", exact: true })
      .click();
    await expect(
      page.getByRole("status").filter({ hasText: "Global SKU review" }),
    ).toContainText("2 of 2 retained records");
    if (mobile) {
      const controls = await page
        .locator(
          ".serial-trace input,.serial-trace select,.serial-trace form button",
        )
        .evaluateAll((nodes) =>
          nodes.map((n) => {
            const r = n.getBoundingClientRect();
            return { left: r.left, right: r.right };
          }),
        );
      expect(controls.every((r) => r.left >= 0 && r.right <= 390)).toBe(true);
    }
    await page.screenshot({
      path: test.info().outputPath("serial-history.png"),
      fullPage: true,
    });
    await page
      .getByRole("row")
      .filter({ hasText: sku })
      .filter({ hasText: bin })
      .getByRole("button", { name: "Count", exact: true })
      .click();
    await page
      .getByLabel(`Counted each units · ${sku} / ${bin}`, { exact: true })
      .fill("0");
    await page
      .getByLabel("Count evidence / reason", { exact: true })
      .fill("Synthetic witnessed complete empty location");
    await page
      .getByRole("button", { name: "Apply count adjustment", exact: true })
      .click();
    const saveZero = page.getByRole("button", {
      name: "Save with observed serials",
      exact: true,
    });
    await expect(saveZero).toBeDisabled();
    await page
      .getByLabel("I physically observed no units for this complete count.", {
        exact: true,
      })
      .check();
    const zeroResponse = page.waitForResponse(
      (r) =>
        r.url().endsWith(`${base}/commands`) && r.request().method() === "POST",
    );
    await saveZero.click();
    const savedZero = await zeroResponse;
    expect(savedZero.ok(), await savedZero.text()).toBe(true);
    expect(savedZero.request().postDataJSON().serial_inputs).toEqual([
      { target: [], sku, serials: [] },
    ]);
    await expect(
      page.getByRole("dialog", { name: "Observe serial labels" }),
    ).toBeHidden();
    units = await get(`/v1/serials?sku=${encodeURIComponent(sku)}`);
    expect(
      units.items.every(
        (u: any) => u.place.status === "missing" && u.revision === 3,
      ),
    ).toBe(true);
    const observations = await get(
      `/v1/serial-observations?sku=${encodeURIComponent(sku)}`,
    );
    expect(observations.page_source_ok).toBe(true);
    expect(observations.total_records).toBe(5);
    expect(observations.items.at(-1).observed_serial_count).toBe(0);
    const proof = await get(`${base}/reconciliation`);
    expect(proof.ok, JSON.stringify(proof)).toBe(true);
    expect(proof.serials.source_ok).toBe(true);
    expect(proof.serials.physical_ok).toBe(true);
    expect(errors).toEqual([]);
  });
