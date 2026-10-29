import { navigateTo } from "./support/navigation";
// Real source/destination UI and API/DB, including a simultaneously retained
// source browser review. Synthetic keyboard events, facilities and inventory.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
import { serialFixture } from "./support/serialLive";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
for (const mobile of [false, true])
  test(`serial transfer groups, original loss and recovered arrival retain cross-facility custody (${mobile ? "mobile" : "desktop"})`, async ({
    page,
    context,
  }) => {
    test.setTimeout(180000);
    const f = await serialFixture(page, mobile);
    const [a, b, c, d] = ["T-A", "t-a", "T-B", "t-b"].map(
        (x) => `${x}-${f.suffix}`,
      ),
      extras = Array.from(
        { length: 51 },
        (_, i) => `KEEP-${String(i).padStart(2, "0")}-${f.suffix}`,
      );
    await f.send(
      {
        type: "count_stock",
        sku: f.sku,
        lot: f.lot,
        location: f.bin,
        counted: 55,
        reason: "Complete synthetic set with a paginated source review",
      },
      [f.group([a, b, c, d, ...extras])],
    );
    const destination = `wh-serial-${f.suffix}`,
      destRoot = `/v1/warehouses/${destination}`,
      code = `DST-${f.suffix}`,
      reference = `SERIAL-TRANSFER-${f.suffix}`;
    const created = await page.request.post(`${destRoot}/commands`, {
      headers: f.headers,
      data: {
        request_id: randomUUID(),
        expected_version: 0,
        command: {
          type: "create_warehouse",
          code,
          name: "Synthetic serialized destination",
          city: "Synthetic",
          timezone: "UTC",
        },
      },
    });
    expect(created.ok(), await created.text()).toBe(true);
    await f.send(
      {
        type: "create_product",
        sku: f.sku,
        name: "Same original controlled SKU",
        category: "Synthetic",
        reorder_point: 0,
        serial_controlled: true,
      },
      [],
      destination,
    );
    await f.login();
    await navigateTo(page, "Facility transfers");
    await page.getByText("New facility transfer", { exact: true }).click();
    await page
      .getByLabel("Destination facility", { exact: true })
      .selectOption(destination);
    await page
      .getByLabel("Transfer reference", { exact: true })
      .fill(reference);
    await page
      .getByLabel("Transfer carrier", { exact: true })
      .fill("Synthetic carrier");
    await page
      .getByLabel("Carrier handoff reference", { exact: true })
      .fill(`DISPATCH-${f.suffix}`);
    await page
      .getByLabel("Source stock 1", { exact: true })
      .selectOption(JSON.stringify([f.sku, f.lot, f.bin]));
    await page.getByLabel("Dispatch each units 1", { exact: true }).fill("2");
    await page
      .getByRole("button", { name: "Add transfer line", exact: true })
      .click();
    await page
      .getByLabel("Source stock 2", { exact: true })
      .selectOption(JSON.stringify([f.sku, f.lot, f.bin]));
    await page.getByLabel("Dispatch each units 2", { exact: true }).fill("2");
    await page
      .getByRole("button", { name: "Review transfer dispatch", exact: true })
      .click();
    const dispatch = await f.uncertain(
      "dispatch_transfer",
      "Record carrier handoff",
      (cmd) => [
        f.group([a, b], [cmd.lines[0].id]),
        f.group([c, d], [cmd.lines[1].id]),
      ],
      mobile ? "before" : "after",
    );
    const tid = dispatch.result.entity_id,
      [first, second] = dispatch.packet.command.lines.map((l: any) => l.id);
    for (const [line, labels] of [
      [first, [a, b]],
      [second, [c, d]],
    ] as [string, string[]][]) {
      expect(
        dispatch.packet.serial_inputs.find((i: any) => i.target[0] === line)
          .serials,
      ).toEqual(labels);
    }
    const sourceVersion = (await f.get()).version;
    // Keep a real source-facility page alive while the other tab changes only the
    // destination. Its warehouse version alone cannot fence these serial pages.
    const sourcePage = await context.newPage();
    sourcePage.on("pageerror", (e) => f.errors.push(e.message));
    if (mobile) await sourcePage.setViewportSize({ width: 390, height: 844 });
    await sourcePage.goto("/");
    await sourcePage
      .getByLabel("API credential", { exact: true })
      .fill(f.headers.Authorization.slice(7));
    await sourcePage
      .getByRole("button", { name: "Connect to workspace", exact: true })
      .click();
    await sourcePage
      .getByRole("button", { name: "Inventory", exact: true })
      .click();
    await sourcePage
      .getByText("Serial custody and original observations", { exact: true })
      .click();
    await sourcePage
      .getByRole("combobox", { name: "Serial SKU", exact: true })
      .selectOption(f.sku);
    await sourcePage.getByLabel("Current facility only").check();
    await sourcePage
      .getByRole("button", { name: "Refresh serial review", exact: true })
      .click();
    await expect(
      sourcePage.getByRole("status").filter({ hasText: "Global SKU review" }),
    ).toContainText("50 of 55 retained records");
    await expect(
      sourcePage.getByRole("button", {
        name: "Load next serial page",
        exact: true,
      }),
    ).toBeEnabled();
    await page
      .getByRole("button", { name: "Change warehouse", exact: true })
      .click();
    await page
      .getByLabel("Active warehouse", { exact: true })
      .selectOption(destination);
    await expect(page.locator(".workspace-card strong")).toHaveText(code);
    await navigateTo(page, "Facility transfers");
    const panel = page.getByRole("region", {
      name: `Loss records for ${reference}`,
    });
    const journal = async () => {
      await expect(panel).toBeVisible();
      const details = panel.locator("details");
      if (!(await details.evaluate((e) => (e as HTMLDetailsElement).open)))
        await panel.locator("summary").click();
      await panel
        .getByRole("button", { name: "Review loss journal", exact: true })
        .click();
    };
    const destinationFacts = async () => {
      const { availability_checked_at, ...state } = await f.get(destRoot);
      return {
        state,
        proof: await f.get(`${destRoot}/reconciliation`),
        serials: await f.units(),
      };
    };
    await journal();
    await panel
      .getByRole("button", {
        name: `Review loss for line ${first}`,
        exact: true,
      })
      .click();
    await panel
      .getByLabel("Carrier incident reference", { exact: true })
      .fill(`INCIDENT-${f.suffix}`);
    await panel
      .getByLabel("Loss review reason", { exact: true })
      .fill("Exact original dispatched unit confirmed missing");
    await panel.getByRole("checkbox").check();
    const beforeWrong = await destinationFacts();
    const wrong = await f.commit(
      "record_transfer_loss",
      "Record confirmed loss",
      [f.group([c])],
      422,
    );
    expect(await destinationFacts()).toEqual(beforeWrong);
    expect(
      (await f.get(`${destRoot}/commands/${wrong.packet.request_id}`)).found,
    ).toBe(false);
    const lost = await f.uncertain(
      "record_transfer_loss",
      "Record confirmed loss",
      [f.group([a])],
      mobile ? "after" : "before",
      destination,
    );
    expect(
      (await f.units()).items.find((u: any) => u.serial === a).place.loss_id,
    ).toBe(lost.result.entity_id);
    await sourcePage
      .getByRole("button", { name: "Load next serial page", exact: true })
      .click();
    await expect(
      sourcePage
        .getByRole("alert")
        .filter({ hasText: "Global SKU identity changed" }),
    ).toBeVisible();
    await expect(sourcePage.getByText(extras[0], { exact: true })).toBeHidden();
    expect((await f.get()).version).toBe(sourceVersion);
    await navigateTo(page, "Facility transfers");
    await journal();
    await panel
      .getByRole("button", {
        name: `Review reversal of loss ${lost.result.entity_id}`,
        exact: true,
      })
      .click();
    await panel
      .getByLabel("Loss review reason", { exact: true })
      .fill("Exact original unit found; actual arrival remains separate");
    await panel.getByRole("checkbox").check();
    const physicalBefore = await f.get(destRoot);
    await f.uncertain(
      "reverse_transfer_loss",
      "Record loss reversal",
      [f.group([a])],
      mobile ? "before" : "after",
      destination,
    );
    const physicalAfter = await f.get(destRoot);
    expect(physicalAfter.stocks).toEqual(physicalBefore.stocks);
    expect(physicalAfter.movements).toEqual(physicalBefore.movements);
    expect(
      (await f.units()).items.find((u: any) => u.serial === a).place.status,
    ).toBe("in_transit");
    const arrival = async (
      line: string,
      ids: string[],
      when: "before" | "after",
    ) => {
      await navigateTo(page, "Facility transfers");
      await page
        .getByRole("button", {
          name: `Receive transfer ${reference} line ${line}`,
          exact: true,
        })
        .click();
      await page
        .getByLabel("Arrived each units", { exact: true })
        .fill(String(ids.length));
      await page
        .getByLabel("Arrival evidence reference", { exact: true })
        .fill(`ARRIVAL-${line}-${f.suffix}`);
      return f.uncertain(
        "receive_transfer",
        "Record transfer arrival",
        [f.group(ids)],
        when,
        destination,
      );
    };
    await arrival(first, [a, b], mobile ? "after" : "before");
    await arrival(second, [c, d], mobile ? "before" : "after");
    const units = (await f.units()).items;
    expect(
      units
        .filter((u: any) => u.place.warehouse_id === destination)
        .map((u: any) => u.serial)
        .sort(),
    ).toEqual([a, b, c, d].sort());
    expect(
      units.filter((u: any) => u.place.warehouse_id === "wh-main"),
    ).toHaveLength(51);
    expect((await f.get()).version).toBe(sourceVersion);
    for (const w of ["wh-main", destination]) {
      const p = await f.get(`/v1/warehouses/${w}/reconciliation`);
      expect(p.ok, JSON.stringify(p)).toBe(true);
      expect(p.serials.source_ok).toBe(true);
      expect(p.serials.physical_ok).toBe(true);
    }
    await sourcePage.close();
    expect(f.errors).toEqual([]);
  });
