// Actual desktop/mobile browser→API→PostgreSQL. Only synthetic fixture data.
// Lost-response interception forwards the real command or prevents arrival;
// it never supplies invented successful HTTP bodies or database proofs.
import { test, expect } from "@playwright/test";
import { randomUUID } from "node:crypto";
import { serialFixture, type Group } from "./support/serialLive";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});
for (const mobile of [false, true]) {
  test(`serial picking, multi-line packages and partial RMA preserve exact owners (${mobile ? "mobile" : "desktop"})`, async ({
    page,
  }) => {
    test.setTimeout(180000);
    const f = await serialFixture(page, mobile);
    const [a, b, c, d] = ["A", "a", "B", "b"].map((s) => `${s}-${f.suffix}`),
      reference = `SERIAL-SO-${f.suffix}`;
    await f.send(
      {
        type: "count_stock",
        sku: f.sku,
        lot: f.lot,
        location: f.bin,
        counted: 4,
        reason: "Witnessed synthetic four-unit opening",
      },
      [f.group([a, b, c, d])],
    );
    const order = await f.send({
      type: "create_order",
      reference,
      source_order_id: null,
      customer: "Synthetic serialized customer",
      destination: "Synthetic destination",
      priority: "normal",
      lines: [
        { id: "LINE-A", sku: f.sku, quantity: 2 },
        { id: "LINE-B", sku: f.sku, quantity: 2 },
      ],
    });
    const oid = order.result.entity_id;
    await f.send({ type: "allocate_available", order_id: oid });
    await f.login();
    const pick = async (line: string, ids: string[]) => {
      await f.openOrder(reference);
      await page
        .locator(".allocation")
        .filter({ hasText: `line ${line}` })
        .getByRole("button", { name: "Pick units", exact: true })
        .click();
      await page
        .getByLabel("Picked quantity (each)", { exact: true })
        .fill(String(ids.length));
      return f.commit("pick", "Confirm pick", [f.group(ids)]);
    };
    await pick("LINE-A", [a, b]);
    await pick("LINE-B", [c, d]);
    let units = (await f.units()).items;
    expect(units.every((u: any) => u.place.status === "staged")).toBe(true);
    expect(units.find((u: any) => u.serial === a).place.line_id).toBe("LINE-A");
    // Restore one original picked unit, then pick it again; each is a distinct
    // physical transition and explicit observation of the same original owner.
    await f.openOrder(reference);
    await page
      .locator(".allocation")
      .filter({ hasText: "line LINE-B" })
      .getByRole("button", { name: "Return staged units", exact: true })
      .click();
    await page
      .getByLabel("Returned quantity (each)", { exact: true })
      .fill("1");
    await page
      .getByLabel("Return evidence / reason", { exact: true })
      .fill("Original unit physically returned before fresh pick");
    await f.commit("unpick", "Confirm physical return", [f.group([d])]);
    await pick("LINE-B", [d]);
    const definition = (
      await f.send({
        type: "set_unit_pack",
        sku: f.sku,
        code: "PAIR",
        label: "Original two-each pack",
        each_per_pack: 2,
        enabled: true,
        reason: "Original observed packaging",
      })
    ).result.entity_id;
    await f.refresh();
    const pref = `PACK-${f.suffix}`;
    const packForm = async (packageReference = pref, usePair = true) => {
      await f.openOrder(reference);
      await page
        .getByLabel("Package reference", { exact: true })
        .fill(packageReference);
      for (const line of ["LINE-A", "LINE-B"]) {
        const label = `Package each units ${line} ${f.bin} ${f.lot}`;
        await page
          .getByLabel(`Units for ${label}`, { exact: true })
          .selectOption(usePair ? definition : "EA");
        await page
          .getByLabel(usePair ? `${label} (PAIR packs)` : label, {
            exact: true,
          })
          .fill(usePair ? "1" : "2");
      }
    };
    await packForm();
    const before = await f.facts();
    const wrong = await f.commit(
      "pack_shipment",
      "Confirm packed shipment",
      [
        f.group([a, c], ["LINE-A", f.bin, f.lot]),
        f.group([b, d], ["LINE-B", f.bin, f.lot]),
      ],
      422,
    );
    expect(await f.facts()).toEqual(before);
    expect(
      (await f.get(`${f.root}/commands/${wrong.packet.request_id}`)).found,
    ).toBe(false);
    // The original form is retained after definitive rejection. Correct scans
    // must stay attached to both original targets through uncertain save/reload.
    const packed = await f.uncertain(
      "pack_shipment",
      "Confirm packed shipment",
      [
        f.group([a, b], ["LINE-A", f.bin, f.lot]),
        f.group([c, d], ["LINE-B", f.bin, f.lot]),
      ],
      mobile ? "after" : "before",
      "wh-main",
      mobile
        ? async () => {
            await f.send(
              {
                type: "set_unit_pack",
                sku: f.sku,
                code: "PAIR",
                label: "Later three-each pack",
                each_per_pack: 3,
                enabled: true,
                reason:
                  "Later definition cannot reinterpret original uncertain packet",
              },
              [],
              "wh-main",
              {
                Authorization: `Bearer ${process.env.WMS_SCANNER_COUNTER_TOKEN || "scanner-live-counter-credential-0001"}`,
              },
            );
          }
        : undefined,
    );
    expect(packed.packet.unit_inputs).toHaveLength(2);
    expect(
      packed.packet.unit_inputs.every(
        (i: any) =>
          i.definition_id === definition &&
          i.each_per_pack === 2 &&
          i.pack_quantity === 1,
      ),
    ).toBe(true);
    expect(packed.packet.serial_inputs).toEqual([
      f.group([a, b], ["LINE-A", f.bin, f.lot]),
      f.group([c, d], ["LINE-B", f.bin, f.lot]),
    ]);
    const packageId = packed.result.entity_id;
    units = (await f.units()).items;
    expect(units.every((u: any) => u.place.package_id === packageId)).toBe(
      true,
    );
    await f.openOrder(reference);
    await page
      .getByRole("button", { name: `Unpack ${pref}`, exact: true })
      .click();
    await page
      .getByLabel("Unpacking evidence / reason", { exact: true })
      .fill("Original package reviewed before repack");
    const unpacked = await f.commit("unpack_shipment", "Confirm unpacking");
    expect(unpacked.packet.serial_inputs).toBeUndefined();
    const repackedReference = `REPACKED-${f.suffix}`;
    await packForm(repackedReference, false);
    await f.commit("pack_shipment", "Confirm packed shipment", [
      f.group([a, b], ["LINE-A", f.bin, f.lot]),
      f.group([c, d], ["LINE-B", f.bin, f.lot]),
    ]);
    await f.openOrder(reference);
    await page
      .getByRole("button", {
        name: `Hand off ${repackedReference}`,
        exact: true,
      })
      .click();
    await page
      .getByLabel("Shipment carrier", { exact: true })
      .fill("Synthetic carrier");
    await page
      .getByLabel("Shipment handoff reference", { exact: true })
      .fill(`HANDOFF-${f.suffix}`);
    await page
      .getByLabel("Carrier has physically received this exact package", {
        exact: true,
      })
      .check();
    const handed = await f.commit(
      "handoff_shipment",
      "Record shipment handoff",
    );
    expect(handed.packet.serial_inputs).toBeUndefined();
    const rmaRef = `RMA-${f.suffix}`;
    const rmaForm = async (ref: string, q: number) => {
      await f.openOrder(reference);
      await page
        .getByRole("button", { name: "Review return allowance", exact: true })
        .click();
      await page
        .getByLabel("Original line and lot", { exact: true })
        .selectOption(JSON.stringify(["LINE-A", f.lot]));
      await page.getByLabel("RMA reference", { exact: true }).fill(ref);
      await page
        .getByLabel("Return reason / inspection evidence", { exact: true })
        .fill("Exact original shipped units observed");
      await page
        .getByLabel("Return quantity (each)", { exact: true })
        .fill(String(q));
    };
    await rmaForm(rmaRef, 2);
    const authorized = await f.uncertain(
      "create_return",
      "Authorize return receipt",
      (cmd) => [f.group([a, b], [cmd.lines[0].id])],
      mobile ? "before" : "after",
    );
    const rid = authorized.result.entity_id;
    expect(authorized.packet.command.lines[0].order_line_id).toBe("LINE-A");
    expect(authorized.packet.serial_inputs[0].target).toEqual([
      authorized.packet.command.lines[0].id,
    ]);
    const receive = async (ref: string, ids: string[]) => {
      await page
        .getByRole("button", { name: "Receiving", exact: true })
        .click();
      await page.getByRole("button", { name: ref, exact: true }).click();
      await page
        .getByRole("dialog")
        .getByRole("button", { name: "Receive goods", exact: true })
        .click();
      await page
        .getByLabel("Received quantity (each)", { exact: true })
        .fill(String(ids.length));
      return f.commit("receive", "Confirm receipt", [f.group(ids)]);
    };
    await receive(rmaRef, [a]);
    await page.getByRole("button", { name: "Receiving", exact: true }).click();
    await page.getByRole("button", { name: rmaRef, exact: true }).click();
    await page
      .getByRole("button", { name: "Cancel unreceived remainder", exact: true })
      .click();
    await page
      .getByLabel("Cancellation reason", { exact: true })
      .fill("Only the original unreceived unit released");
    const cancelled = await f.uncertain(
      "cancel_receipt",
      "Confirm remainder cancellation",
      undefined,
      mobile ? "after" : "before",
    );
    expect(cancelled.packet.serial_inputs).toBeUndefined();
    units = (await f.units()).items;
    expect(units.find((u: any) => u.serial === a).place.status).toBe("on_hand");
    expect(
      units.find((u: any) => u.serial === b).place.return_receipt_id,
    ).toBeNull();
    expect(units.find((u: any) => u.serial === b).place.status).toBe("shipped");
    const replacement = `RMA-REPLACEMENT-${f.suffix}`;
    await rmaForm(replacement, 1);
    await f.commit("create_return", "Authorize return receipt", (cmd) => [
      f.group([b], [cmd.lines[0].id]),
    ]);
    await receive(replacement, [b]);
    const proof = await f.get(`${f.root}/reconciliation`);
    expect(proof.ok, JSON.stringify(proof)).toBe(true);
    expect(proof.serials.source_ok).toBe(true);
    expect(proof.serials.physical_ok).toBe(true);
    expect(
      (await f.units()).items
        .filter((u: any) => u.place.status === "on_hand")
        .map((u: any) => u.serial)
        .sort(),
    ).toEqual([a, b].sort());
    const old = await page.request.post(`${f.root}/commands`, {
      headers: f.headers,
      data: authorized.packet,
    });
    expect(old.ok(), await old.text()).toBe(true);
    expect((await old.json()).replayed).toBe(true);
    expect((await f.get(`${f.root}/receipts/${rid}`)).status).toBe("cancelled");
    expect(f.errors).toEqual([]);
  });
}

for (const mobile of [false, true])
  test(`serial scans cannot survive an equal-quantity identity replacement without review (${mobile ? "mobile" : "desktop"})`, async ({
    page,
  }) => {
    test.setTimeout(120000);
    const f = await serialFixture(page, mobile);
    const [a, b, c, fresh] = ["OLD", "KEEP", "OTHER", "NEW"].map(
        (x) => `${x}-${f.suffix}`,
      ),
      reference = `STALE-SERIAL-${f.suffix}`;
    await f.send(
      {
        type: "count_stock",
        sku: f.sku,
        lot: f.lot,
        location: f.bin,
        counted: 3,
        reason: "Original observed set",
      },
      [f.group([a, b, c])],
    );
    const created = await f.send({
      type: "create_order",
      reference,
      source_order_id: null,
      customer: "Synthetic concurrency customer",
      destination: "Synthetic",
      priority: "normal",
      lines: [{ id: "STALE", sku: f.sku, quantity: 1 }],
    });
    await f.send({
      type: "allocate_available",
      order_id: created.result.entity_id,
    });
    await f.login();
    await f.openOrder(reference);
    await page
      .locator(".allocation")
      .filter({ hasText: "line STALE" })
      .getByRole("button", { name: "Pick units", exact: true })
      .click();
    await page
      .getByRole("button", { name: "Confirm pick", exact: true })
      .click();
    await f.collect([f.group([a])]);
    await expect(page.getByLabel("Original physical action")).toContainText(
      f.bin,
    );
    const counter = {
      Authorization: `Bearer ${process.env.WMS_SCANNER_COUNTER_TOKEN || "scanner-live-counter-credential-0001"}`,
    };
    await f.send(
      {
        type: "count_stock",
        sku: f.sku,
        lot: f.lot,
        location: f.bin,
        counted: 3,
        reason:
          "Equal total, one physically missing identity and one newly observed",
      },
      [f.group([b, c, fresh])],
      "wh-main",
      counter,
    );
    const afterCounter = await f.facts();
    const response = page.waitForResponse(
      (r) =>
        r.url().endsWith("/commands") &&
        r.request().method() === "POST" &&
        r.request().postDataJSON()?.command?.type === "pick",
    );
    await page
      .getByRole("button", { name: "Save with observed serials", exact: true })
      .click();
    const rejected = await response;
    expect(rejected.status(), await rejected.text()).toBe(409);
    expect(rejected.request().postDataJSON().serial_inputs).toEqual([
      f.group([a]),
    ]);
    expect(rejected.request().postDataJSON().expected_version).toBeLessThan(
      afterCounter.state.version,
    );
    expect(await f.facts()).toEqual(afterCounter);
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeHidden();
    await f.close();
    await f.openOrder(reference);
    await page
      .locator(".allocation")
      .filter({ hasText: "line STALE" })
      .getByRole("button", { name: "Pick units", exact: true })
      .click();
    await f.commit("pick", "Confirm pick", [f.group([b])]);
    const units = (await f.units()).items;
    expect(units.find((u: any) => u.serial === a).place.status).toBe("missing");
    expect(units.find((u: any) => u.serial === b).place.status).toBe("staged");
    expect((await f.get(`${f.root}/reconciliation`)).ok).toBe(true);
    expect(f.errors).toEqual([]);
  });
