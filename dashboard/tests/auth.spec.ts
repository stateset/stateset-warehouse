// Real bundled AuthKit SDK with intercepted provider/API HTTP. JWT signature and
// persistence checks live in Rust tests; this is not a live WorkOS project proof.
import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
const root = resolve(process.cwd(), "..");
const origin = "https://warehouse.test";
function token(user: string, organization: string, role: string) {
  const now = Math.floor(Date.now() / 1000);
  const encode = (value: unknown) =>
    Buffer.from(JSON.stringify(value)).toString("base64url");
  return `${encode({ alg: "RS256", kid: "browser-fixture" })}.${encode({ sub: user, org_id: organization, role, sid: "session_fixture", iss: "https://auth.warehouse.test/", iat: now, exp: now + 300 })}.browser-contract-only`;
}
async function setup(page: Page, signedIn = true) {
  const fixture = spawnSync(
    resolve(root, "target/debug/examples/domain_driver"),
    [],
    { encoding: "utf8" },
  );
  if (fixture.status !== 0) throw new Error(fixture.stderr);
  const state = JSON.parse(fixture.stdout);
  if (signedIn)
    await page.context().addCookies([
      {
        name: "workos-has-session",
        value: "client_warehouse",
        url: origin,
        secure: true,
        sameSite: "Lax",
      },
    ]);
  const control = {
    signedIn,
    refreshGate: null as Promise<void> | null,
    user: "user_alice",
    org: "org_acme",
    role: "warehouse_manager",
    failConfig: false,
    loseSave: false,
    calls: 0,
    writes: [] as any[],
    tokens: [] as string[],
    authRequests: [] as any[],
  };
  await page.route("https://auth.warehouse.test/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/user_management/authenticate") {
      if (route.request().method() === "OPTIONS")
        return route.fulfill({
          status: 204,
          headers: {
            "access-control-allow-origin": origin,
            "access-control-allow-credentials": "true",
            "access-control-allow-headers": "content-type",
            "access-control-allow-methods": "POST",
          },
        });
      control.calls++;
      if (control.refreshGate) await control.refreshGate;
      control.authRequests.push(route.request().postDataJSON());
      return route.fulfill({
        status: control.signedIn ? 200 : 401,
        contentType: "application/json",
        headers: {
          "access-control-allow-origin": origin,
          "access-control-allow-credentials": "true",
        },
        body: JSON.stringify(
          control.signedIn
            ? {
                access_token: token(control.user, control.org, control.role),
                organization_id: control.org,
                user: {
                  id: control.user,
                  email: "operator@example.test",
                  email_verified: true,
                  first_name: "Warehouse",
                  last_name: "Operator",
                  created_at: "2026-01-01T00:00:00Z",
                  updated_at: control.user,
                },
              }
            : { error: "invalid_grant", error_description: "Session ended" },
        ),
      });
    }
    if (path === "/user_management/sessions/logout") {
      control.signedIn = false;
      return route.fulfill({
        status: 302,
        headers: { location: `${origin}/` },
      });
    }
    return route.fulfill({
      contentType: "text/html",
      body: "<h1>WorkOS sign-in fixture</h1>",
    });
  });
  await page.route(`${origin}/**`, async (route) => {
    const path = new URL(route.request().url()).pathname;
    const send = (data: unknown, status = 200) =>
      route.fulfill({
        status,
        contentType: "application/json",
        body: JSON.stringify(data),
      });
    if (path === "/auth/config")
      return control.failConfig
        ? send({ error: "Unavailable" }, 503)
        : send({
            provider: "workos",
            clientId: "client_warehouse",
            apiHostname: "auth.warehouse.test",
          });
    if (path.startsWith("/v1/"))
      control.tokens.push(route.request().headers()["authorization"] ?? "");
    if (path === "/v1/session")
      return send({
        identity: {
          tenant: control.org === "org_acme" ? "acme" : "second",
          actor: `workos:${control.user}`,
          role: "admin",
        },
        demo: false,
        integrations: [],
      });
    if (path === "/v1/warehouses")
      return send({
        warehouses: [{ warehouse: state.warehouse, version: state.version }],
      });
    if (path === "/v1/warehouses/wh-pdx") return send(state);
    if (path.endsWith("/outbox")) return send({ items: [] });
    if (path.includes("/orders/"))
      return send(
        state.orders.find((o: any) => o.id === path.split("/").at(-1)),
      );
    if (path.endsWith("/commands")) {
      control.writes.push(route.request().postDataJSON());
      if (control.loseSave) return route.abort("failed");
      return send({ replayed: false, result: {} });
    }
    if (path.includes("/commands/")) return send({ found: false });
    if (path.startsWith("/v1/"))
      return send({ error: "Unhandled fixture route" }, 404);
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
  return control;
}
async function warehouse(page: Page) {
  await page.goto(`${origin}/`);
  await expect(
    page.getByRole("button", { name: "Fulfillment", exact: true }),
  ).toBeVisible();
}
async function loseSave(page: Page) {
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await page.getByRole("button", { name: "SO-1041", exact: true }).click();
  await page.getByRole("button", { name: "Allocate stock" }).click();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeEnabled();
}
async function storage(page: Page) {
  return page.evaluate(() => ({
    session: { ...sessionStorage },
    local: { ...localStorage },
  }));
}
test("WorkOS signs in without persisting bearer tokens and signs out through the provider", async ({
  page,
}) => {
  const control = await setup(page);
  await page.addInitScript(() =>
    sessionStorage.setItem("wms-token", "obsolete-static-key"),
  );
  await warehouse(page);
  expect(control.tokens.length).toBeGreaterThan(0);
  expect(control.tokens.every((t) => t.startsWith("Bearer eyJ"))).toBe(true);
  expect(
    control.authRequests.every(
      (r) => r.grant_type === "refresh_token" && !("refresh_token" in r),
    ),
  ).toBe(true);
  const saved = await storage(page);
  expect(saved.session["wms-token"]).toBeUndefined();
  expect(JSON.stringify(saved)).not.toContain("Bearer");
  for (const bearer of control.tokens)
    expect(JSON.stringify(saved)).not.toContain(bearer.slice(7));
  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(
    page.getByRole("button", { name: "Sign in with WorkOS" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Fulfillment", exact: true }),
  ).toHaveCount(0);
});
test("sign-in redirects use PKCE and a fixed same-origin callback", async ({
  page,
}) => {
  await setup(page, false);
  await page.goto(`${origin}/?returnTo=https://attacker.test`);
  await page.getByRole("button", { name: "Sign in with WorkOS" }).click();
  await expect(
    page.getByRole("heading", { name: "WorkOS sign-in fixture" }),
  ).toBeVisible();
  const url = new URL(page.url());
  expect(url.origin).toBe("https://auth.warehouse.test");
  expect(url.searchParams.get("redirect_uri")).toBe(`${origin}/`);
  expect(url.searchParams.get("code_challenge_method")).toBe("S256");
  expect(url.searchParams.get("code_challenge")).toMatch(/^[A-Za-z0-9_-]{43}$/);
});
test("missing auth configuration fails closed without credential fallback", async ({
  page,
}) => {
  const control = await setup(page);
  control.failConfig = true;
  await page.goto(`${origin}/`);
  await expect(page.getByRole("alert")).toContainText(
    "Sign-in configuration could not be loaded",
  );
  await expect(page.getByLabel("API credential")).toHaveCount(0);
  expect(control.calls).toBe(0);
  expect(control.tokens).toEqual([]);
});
test("refresh preserves a pending save and same-user reload restores its exact journal", async ({
  page,
}) => {
  const control = await setup(page);
  control.loseSave = true;
  await warehouse(page);
  await loseSave(page);
  const key = "wms-pending:acme:workos:user_alice";
  const original = (await storage(page)).session[key];
  expect(original).toBeTruthy();
  await page.clock.install();
  const calls = control.calls;
  await page.clock.fastForward(295000);
  await expect.poll(() => control.calls).toBeGreaterThan(calls);
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeEnabled();
  expect((await storage(page)).session[key]).toBe(original);
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeEnabled();
  expect((await storage(page)).session[key]).toBe(original);
  expect(control.writes).toHaveLength(1);
});
for (const change of ["organization", "user", "expiry"] as const) {
  test(`${change} change closes old work and retains its unresolved save`, async ({
    page,
  }) => {
    const control = await setup(page);
    control.loseSave = true;
    await warehouse(page);
    await loseSave(page);
    const key = "wms-pending:acme:workos:user_alice";
    const original = (await storage(page)).session[key];
    if (change === "organization") control.org = "org_second";
    if (change === "user") control.user = "user_bob";
    if (change === "expiry") control.signedIn = false;
    await page.clock.install();
    const calls = control.calls;
    await page.clock.fastForward(295000);
    await expect.poll(() => control.calls).toBeGreaterThan(calls);
    await expect(
      page.getByRole("button", { name: "Check original save" }),
    ).toHaveCount(0);
    if (change === "expiry")
      await expect(
        page.getByRole("button", { name: "Sign in with WorkOS" }),
      ).toBeVisible();
    else
      await expect(
        page.getByRole("button", { name: "Fulfillment", exact: true }),
      ).toBeVisible();
    expect((await storage(page)).session[key]).toBe(original);
    expect(control.writes).toHaveLength(1);
  });
}

test("a command waiting for token refresh cannot cross into a different organization", async ({
  page,
}) => {
  const control = await setup(page);
  await warehouse(page);
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await page.getByRole("button", { name: "SO-1041", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Allocate stock" }),
  ).toBeEnabled();
  let release!: () => void;
  control.refreshGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const calls = control.calls;
  await page.clock.install();
  await page.clock.fastForward(295000);
  await expect.poll(() => control.calls).toBeGreaterThan(calls);
  await page.getByRole("button", { name: "Allocate stock" }).click();
  const key = "wms-pending:acme:workos:user_alice";
  await expect
    .poll(async () => (await storage(page)).session[key])
    .toBeTruthy();
  const original = (await storage(page)).session[key];
  control.org = "org_second";
  control.refreshGate = null;
  release();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Fulfillment", exact: true }),
  ).toBeVisible();
  expect(control.writes).toEqual([]);
  expect((await storage(page)).session[key]).toBe(original);
});

test("a stalled refresh releases the busy view without sending or discarding the pending command", async ({
  page,
}) => {
  const control = await setup(page);
  await warehouse(page);
  await page.getByRole("button", { name: "Fulfillment", exact: true }).click();
  await page.getByRole("button", { name: "SO-1041", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Allocate stock" }),
  ).toBeEnabled();
  let release!: () => void;
  control.refreshGate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.clock.install();
  const calls = control.calls;
  await page.clock.fastForward(295000);
  await expect.poll(() => control.calls).toBeGreaterThan(calls);
  await page.getByRole("button", { name: "Allocate stock" }).click();
  const key = "wms-pending:acme:workos:user_alice";
  await expect
    .poll(async () => (await storage(page)).session[key])
    .toBeTruthy();
  const original = (await storage(page)).session[key];
  await page.clock.fastForward(21000);
  await expect(
    page.getByRole("button", { name: "Check original save" }),
  ).toBeEnabled();
  expect(control.writes).toEqual([]);
  expect((await storage(page)).session[key]).toBe(original);
  control.refreshGate = null;
  release();
  // A late token resolution must not resume the expired request.
  await page.getByRole("button", { name: "Close dialog", exact: true }).click();
  await Promise.all([
    page.waitForResponse(
      (response) => response.url() === `${origin}/v1/warehouses/wh-pdx`,
    ),
    page
      .getByRole("button", { name: "Refresh warehouse", exact: true })
      .click(),
  ]);
  expect(control.writes).toEqual([]);
});
