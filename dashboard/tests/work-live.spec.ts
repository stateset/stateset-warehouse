import { test, expect } from "@playwright/test";
import { serialFixture } from "./support/serialLive";
import { navigateTo } from "./support/navigation";
test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});

for (const mobile of [false, true]) {
  test(`owned putaway scans, team visibility and original serial save recovery (${mobile ? "mobile" : "desktop"})`, async ({
    page,
  }) => {
    test.setTimeout(150000);
    const f = await serialFixture(page, mobile);
    const labels = [`Case-${f.suffix}`, `case-${f.suffix}`];
    const receipt = await f.send({
      type: "create_receipt",
      reference: `WORK-${f.suffix}`,
      supplier: "Synthetic receiving",
      lines: [{ id: "LINE", sku: f.sku, lot: f.lot, quantity: 2 }],
    });
    await f.send(
      {
        type: "receive",
        receipt_id: receipt.result.entity_id,
        line_id: "LINE",
        location: "RECV-01",
        quantity: 2,
      },
      [f.group(labels)],
    );
    await f.login();
    await navigateTo(page, "My work");
    await page.getByRole("button", { name: "Available", exact: true }).click();
    const card = page.getByRole("article", {
      name: `Putaway ${f.sku} lot ${f.lot}`,
      exact: true,
    });
    await expect(card).toContainText("2 each remaining");
    const version = (await f.get()).version;
    await card.getByRole("button", { name: "Claim task", exact: true }).click();
    await page.getByRole("button", { name: "My queue", exact: true }).click();
    await expect(
      card.getByRole("button", { name: "Review putaway", exact: true }),
    ).toBeEnabled();
    await card.getByRole("button", { name: "Block", exact: true }).click();
    await card
      .getByLabel("Work reason", { exact: true })
      .fill("Synthetic aisle obstruction");
    await card
      .getByRole("button", { name: "Save reason", exact: true })
      .click();
    await expect(card).toContainText("blocked");
    await page.getByRole("button", { name: "Team board", exact: true }).click();
    await expect(card).toContainText("Synthetic aisle obstruction");
    await expect(card).toContainText("qualification:operator");
    expect((await f.get()).version).toBe(version);
    await card.getByRole("button", { name: "Resume", exact: true }).click();
    await card
      .getByLabel(`Units this move for ${f.sku} lot ${f.lot}`, { exact: true })
      .fill(mobile ? "2" : "1");
    const reviewResponse = page.waitForResponse(
      (r) => r.url().endsWith("/review") && r.request().method() === "POST",
    );
    await card
      .getByRole("button", { name: "Review putaway", exact: true })
      .click();
    const review = await (await reviewResponse).json();
    const form = page.getByRole("form", {
      name: "Confirm putaway",
      exact: true,
    });
    const command = review.request.command;
    expect(command.quantity).toBe(mobile ? 2 : 1);
    await expect(
      form.getByRole("button", { name: "Confirm putaway", exact: true }),
    ).toBeDisabled();
    for (const [field, value] of [
      ["source bin", command.from],
      ["product sku", command.sku],
      ["lot", command.lot],
      ["destination bin", command.to],
      ["units moved", String(command.quantity)],
    ]) {
      await form.getByLabel(`Confirm ${field}`, { exact: true }).fill(value);
    }
    await form.getByRole("checkbox").check();
    await form
      .getByLabel("Confirm product sku", { exact: true })
      .fill(f.sku.toLowerCase());
    await expect(
      form.getByRole("button", { name: "Confirm putaway", exact: true }),
    ).toBeDisabled();
    await form.getByLabel("Confirm product sku", { exact: true }).fill(f.sku);
    await page.evaluate(() => window.scrollTo(0, 0));
    await page.screenshot({
      path: test
        .info()
        .outputPath(`putaway-review-${mobile ? "mobile" : "desktop"}.png`),
      fullPage: true,
    });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    const saved = await f.uncertain(
      "move_stock",
      "Confirm putaway",
      [f.group(mobile ? labels : labels.slice(0, 1))],
      mobile ? "before" : "after",
    );
    expect(saved.packet.request_id).toBe(review.request.request_id);
    expect(saved.packet.resource_versions).toEqual(
      review.request.resource_versions,
    );
    await navigateTo(page, "My work");
    await expect(
      page.getByRole("group", { name: "Work queue", exact: true }),
    ).toHaveCount(1);
    if (!mobile) {
      await page.getByRole("button", { name: "My queue", exact: true }).click();
      await expect(card).toContainText("1 each remaining");
      await expect(card).toContainText("1 / 2 put away");
      const nextReviewResponse = page.waitForResponse(
        (r) => r.url().endsWith("/review") && r.request().method() === "POST",
      );
      await card
        .getByRole("button", { name: "Review putaway", exact: true })
        .click();
      const next = (await (await nextReviewResponse).json()).request;
      expect(next.request_id).not.toBe(review.request.request_id);
      for (const [field, value] of [
        ["source bin", next.command.from],
        ["product sku", next.command.sku],
        ["lot", next.command.lot],
        ["destination bin", next.command.to],
        ["units moved", "1"],
      ]) {
        await form.getByLabel(`Confirm ${field}`, { exact: true }).fill(value);
      }
      await form.getByRole("checkbox").check();
      await f.commit("move_stock", "Confirm putaway", [
        f.group(labels.slice(1)),
      ]);
      await expect(form).toHaveCount(0);
    }
    await page.getByRole("button", { name: "Closed", exact: true }).click();
    await expect(card).toContainText("completed");
    await expect(card).toContainText("2 / 2 put away");
    const units = (await f.units()).items;
    expect(units.map((u: any) => u.serial).sort()).toEqual([...labels].sort());
    expect(units.every((u: any) => u.place.location === command.to)).toBe(true);
    expect((await f.get(`${f.root}/reconciliation`)).ok).toBe(true);
    await page
      .getByRole("button", { name: "Switch to light theme", exact: true })
      .click();
    await page.evaluate(() => window.scrollTo(0, 0));
    await page.screenshot({
      path: test
        .info()
        .outputPath(
          `putaway-closed-light-${mobile ? "mobile" : "desktop"}.png`,
        ),
      fullPage: true,
    });
    expect(f.errors).toEqual([]);
  });
}
