// Actual browser/HTTP/PostgreSQL operational visibility; no request interception.
import { expect, test } from "@playwright/test";
import { randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";

test.use({
  baseURL: process.env.WMS_SCANNER_BROWSER_URL || "http://127.0.0.1:8089",
});

test("real operational snapshot, metrics, and CSP-protected export agree with committed exception recovery", async ({
  page,
}) => {
  test.setTimeout(90000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: "Bearer " + token };
  const root = "/v1/warehouses/wh-main";
  const get = async (path: string) => {
    const response = await page.request.get(path, { headers });
    expect(response.ok(), path + ": " + (await response.text())).toBe(true);
    return response.json();
  };
  const command = async (value: unknown) => {
    const state = await get(root);
    const response = await page.request.post(root + "/commands", {
      headers,
      data: {
        request_id: randomUUID(),
        expected_version: state.version,
        command: value,
      },
    });
    expect(response.ok(), await response.text()).toBe(true);
    return response.json();
  };
  const session = await get("/v1/session");
  expect(session.demo).toBe(false);
  expect(session.identity.role).toBe("admin");
  expect(session.identity.tenant).toBe("scanner-qualification");
  const before = await get("/v1/operations/health?limit=25");
  const opened = await command({
    type: "report_exception",
    order_id: null,
    category: "Operational qualification",
    detail: "Synthetic release qualification " + randomUUID(),
  });
  const exceptionId = opened.result.entity_id;
  let resolved = false;
  const pageErrors: string[] = [];
  page.on("pageerror", (error) => pageErrors.push(error.message));
  try {
    const canonical = await get("/v1/operations/health?limit=25");
    expect(canonical.warehouses.open_exceptions).toBe(
      before.warehouses.open_exceptions + 1,
    );
    const document = await page.goto("/");
    expect(document?.headers()["content-security-policy"]).toContain(
      "script-src 'self'",
    );
    expect(document?.headers()["x-content-type-options"]).toBe("nosniff");
    await page.getByLabel("API credential", { exact: true }).fill(token);
    await page
      .getByRole("button", { name: "Connect to workspace", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "Warehouse overview", exact: true }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Operations", exact: true }).click();
    await expect(
      page.getByText("Operational review needed", { exact: true }),
    ).toBeVisible();
    await expect(
      page
        .locator(".operations-cards article")
        .filter({ hasText: "Open warehouse exceptions" })
        .locator("strong"),
    ).toHaveText(String(canonical.warehouses.open_exceptions));
    const downloadPromise = page.waitForEvent("download");
    await page
      .getByRole("button", { name: "Download snapshot", exact: true })
      .click();
    const download = await downloadPromise;
    const path = await download.path();
    expect(path).not.toBeNull();
    const report = JSON.parse(await readFile(path!, "utf8"));
    expect(report.tenant).toBe(session.identity.tenant);
    expect(report.snapshot.warehouses.open_exceptions).toBe(
      canonical.warehouses.open_exceptions,
    );
    expect(report.snapshot.outbox.total).toBe(canonical.outbox.total);
    expect(report.snapshot.native.total).toBe(canonical.native.total);
    const metrics = await page.request.get("/v1/operations/metrics", {
      headers,
    });
    expect(metrics.ok()).toBe(true);
    expect(metrics.headers()["cache-control"]).toBe("no-store");
    const text = await metrics.text();
    expect(text).toContain(
      'wms_outbox_receiver_configured{destination="oms"} 0',
    );
    expect(text).not.toContain(token);
    expect(text).not.toContain("qualification:operator");
    await command({
      type: "resolve_exception",
      exception_id: exceptionId,
      resolution: "Synthetic release qualification complete",
    });
    resolved = true;
    await page
      .getByRole("button", { name: "Refresh status", exact: true })
      .click();
    await expect(
      page
        .locator(".operations-cards article")
        .filter({ hasText: "Open warehouse exceptions" })
        .locator("strong"),
    ).toHaveText(String(before.warehouses.open_exceptions));
    expect((await get(root + "/reconciliation")).ok).toBe(true);
    expect(pageErrors).toEqual([]);
  } finally {
    if (!resolved)
      await command({
        type: "resolve_exception",
        exception_id: exceptionId,
        resolution: "Clean up interrupted synthetic operational qualification",
      });
  }
});
