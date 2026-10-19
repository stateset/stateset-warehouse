// Owned synthetic live/API fixtures. Credentials are supplied by the isolated
// runner; no HTTP interception fabricates a server result or source proof.
import { test, expect, type Page } from "@playwright/test";
import { randomUUID } from "node:crypto";
export type Observation = Group[] | ((command: any) => Group[]);
export type Group = { target: string[]; sku: string; serials: string[] };
export async function serialFixture(page: Page, mobile = false) {
  if (mobile) await page.setViewportSize({ width: 390, height: 844 });
  page.setDefaultTimeout(10000);
  const token =
    process.env.WMS_SCANNER_OPERATOR_TOKEN ||
    "scanner-live-operator-credential-0001";
  const headers = { Authorization: `Bearer ${token}` },
    root = "/v1/warehouses/wh-main",
    suffix = randomUUID().slice(0, 8),
    sku = `SERIAL-WORK-${suffix}`,
    lot = `LOT-${suffix}`,
    bin = `BIN-${suffix}`;
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  const get = async (path = root) => {
    const r = await page.request.get(path, { headers });
    expect(r.ok(), await r.text()).toBe(true);
    return r.json();
  };
  const send = async (
    command: any,
    serial_inputs: Group[] = [],
    warehouse = "wh-main",
    auth = headers,
  ) => {
    const s = await page.request.get(`/v1/warehouses/${warehouse}`, {
      headers: auth,
    });
    expect(s.ok(), await s.text()).toBe(true);
    const state = await s.json();
    const r = await page.request.post(`/v1/warehouses/${warehouse}/commands`, {
      headers: auth,
      data: {
        request_id: randomUUID(),
        expected_version: state.version,
        command,
        ...(serial_inputs.length ? { serial_inputs } : {}),
      },
    });
    expect(r.ok(), await r.text()).toBe(true);
    return r.json();
  };
  const group = (serials: string[], target: string[] = []): Group => ({
    target,
    sku,
    serials,
  });
  const session = await get("/v1/session");
  expect(session.demo).toBe(false);
  expect(session.identity.tenant).toBe("scanner-qualification");
  await send({
    type: "create_product",
    sku,
    name: "Synthetic serial workflow",
    category: "Synthetic",
    reorder_point: 0,
    serial_controlled: true,
  });
  await send({
    type: "create_location",
    code: bin,
    zone: "Synthetic",
    kind: "storage",
  });
  const login = async () => {
    await page.goto("/");
    await page.getByLabel("API credential", { exact: true }).fill(token);
    await page
      .getByRole("button", { name: "Connect to workspace", exact: true })
      .click();
  };
  const refresh = async () => {
    await page
      .getByRole("button", { name: "Refresh warehouse", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Refresh warehouse", exact: true }),
    ).toBeEnabled();
  };
  const openOrder = async (reference: string) => {
    await page
      .getByRole("button", { name: "Fulfillment", exact: true })
      .click();
    await page.getByRole("button", { name: reference, exact: true }).click();
    await expect(
      page.getByRole("heading", {
        name: "Shipment packages and backorders",
        exact: true,
      }),
    ).toBeVisible();
  };
  const close = async () => {
    if (await page.getByRole("dialog").count())
      await page
        .getByRole("button", { name: "Close dialog", exact: true })
        .click();
  };
  const collect = async (groups: Group[]) => {
    const dialog = page.getByRole("dialog", {
      name: "Observe serial labels",
      exact: true,
    });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByLabel("Original physical action")).toBeVisible();
    const fields = dialog.locator("fieldset");
    await expect(fields).toHaveCount(groups.length);
    for (const g of groups) {
      const field = fields.filter({
        has: page.getByRole("textbox", {
          name: `Scan serial for ${g.sku}${g.target.length ? ` · ${g.target.join(" / ")}` : ""}`,
          exact: true,
        }),
      });
      if (g.target.length === 3) {
        await expect(
          field.getByText(`Original order line: ${g.target[0]}`, {
            exact: true,
          }),
        ).toBeVisible();
        await expect(
          field.getByText(`Original allocation bin: ${g.target[1]}`, {
            exact: true,
          }),
        ).toBeVisible();
        await expect(
          field.getByText(`Original lot: ${g.target[2]}`, { exact: true }),
        ).toBeVisible();
      }
      for (const label of g.serials) {
        const input = field.getByRole("textbox");
        await input.fill(label);
        await input.press("Enter");
      }
    }
    await expect(
      dialog.getByRole("button", {
        name: "Save with observed serials",
        exact: true,
      }),
    ).toBeEnabled();
  };
  const commit = async (
    type: string,
    trigger: string,
    groups?: Observation,
    expectedStatus = 200,
  ) => {
    const response = page.waitForResponse(
      (r) =>
        r.url().endsWith("/commands") &&
        r.request().method() === "POST" &&
        r.request().postDataJSON()?.command?.type === type,
    );
    const plan =
      typeof groups === "function"
        ? page.waitForRequest(
            (r) =>
              r.url().endsWith("/serial-observation-plan") &&
              r.postDataJSON()?.command?.type === type,
          )
        : null;
    await page.getByRole("button", { name: trigger, exact: true }).click();
    if (groups) {
      await collect(
        typeof groups === "function"
          ? groups((await plan!).postDataJSON().command)
          : groups,
      );
      await page
        .getByRole("button", {
          name: "Save with observed serials",
          exact: true,
        })
        .click();
    }
    const r = await response;
    expect(r.status(), await r.text()).toBe(expectedStatus);
    const packet = r.request().postDataJSON();
    const result = await r.json();
    if (expectedStatus === 200)
      await expect(page.getByRole("dialog")).toHaveCount(0);
    return { packet, result: result.result };
  };
  const units = () =>
    get(`/v1/serials?sku=${encodeURIComponent(sku)}&limit=100`);
  const facts = async () => {
    const { availability_checked_at, ...s } = await get();
    return {
      state: s,
      serials: await units(),
      proof: await get(`${root}/reconciliation`),
    };
  };
  let captureIndex = 0;
  const uncertain = async (
    type: string,
    trigger: string,
    groups: Observation | undefined,
    when: "before" | "after",
    warehouse = "wh-main",
    afterFault?: () => Promise<void>,
  ) => {
    const commandRoot = `/v1/warehouses/${warehouse}`;
    const packets: any[] = [];
    let dropped = false;
    let injected!: () => void, failed!: (error: unknown) => void;
    const faultDone = new Promise<void>((resolve, reject) => {
      injected = resolve;
      failed = reject;
    });
    await page.route(`**${commandRoot}/commands`, async (route) => {
      const p = route.request().postDataJSON();
      if (p.command.type !== type) return route.continue();
      packets.push(p);
      if (!dropped) {
        dropped = true;
        try {
          if (when === "after") {
            const r = await route.fetch();
            expect(r.ok(), await r.text()).toBe(true);
          }
          await route.abort("failed");
          injected();
        } catch (error) {
          failed(error);
        }
        return;
      }
      return route.continue();
    });
    const plan =
      typeof groups === "function"
        ? page.waitForRequest(
            (r) =>
              r.url().endsWith("/serial-observation-plan") &&
              r.postDataJSON()?.command?.type === type,
          )
        : null;
    await page.getByRole("button", { name: trigger, exact: true }).click();
    if (groups) {
      await collect(
        typeof groups === "function"
          ? groups((await plan!).postDataJSON().command)
          : groups,
      );
      await page
        .getByRole("dialog", { name: "Observe serial labels", exact: true })
        .screenshot({
          path: test
            .info()
            .outputPath(`serial-review-${type}-${captureIndex++}.png`),
        });
      await page
        .getByRole("button", {
          name: "Save with observed serials",
          exact: true,
        })
        .click();
    }
    await faultDone;
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeEnabled();
    expect(packets).toHaveLength(1);
    const original = structuredClone(packets[0]);
    await afterFault?.();
    const retained = await page.evaluate(
      ({ tenant, actor }) =>
        JSON.parse(sessionStorage.getItem(`wms-pending:${tenant}:${actor}`)!),
      session.identity,
    );
    expect(retained.request).toEqual(original);
    await page.reload();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "Check original save", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check original save", exact: true }),
    ).toBeHidden();
    if (when === "before") {
      expect(packets).toHaveLength(2);
      expect(packets[1]).toEqual(original);
    } else expect(packets).toHaveLength(1);
    await page.unroute(`**${commandRoot}/commands`);
    const receipt = await get(`${commandRoot}/commands/${original.request_id}`);
    expect(receipt.found).toBe(true);
    return { packet: original, result: receipt.result };
  };
  return {
    page,
    headers,
    root,
    suffix,
    sku,
    lot,
    bin,
    errors,
    get,
    send,
    group,
    login,
    refresh,
    openOrder,
    close,
    collect,
    commit,
    uncertain,
    units,
    facts,
  };
}
