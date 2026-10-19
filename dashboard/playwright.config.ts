import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./tests",
  timeout: 30000,
  fullyParallel: false,
  workers: 1,
  use: {
    headless: true,
    launchOptions: {
      executablePath: process.env.WMS_CHROME_PATH || undefined,
      args: ["--no-sandbox", "--disable-dev-shm-usage"],
    },
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "offline-contract",
      testMatch:
        /(?:offline|native|scanner|operations|auth|facilities|lot-controls|lot-trace|transfer-loss|units|serials)\.spec\.ts/,
    },
    {
      name: "live-postgres",
      testMatch:
        /(?:^|\/)(?:live|scanner-live|operations-live|facilities-live|cancellation-live|transfers-live|split-live|lot-controls-live|lot-trace-live|transfer-loss-live|units-live|serials-live|serial-workflows-live|serial-transfers-live)\.spec\.ts$/,
      use: { baseURL: process.env.WMS_BROWSER_URL || "http://127.0.0.1:8088" },
    },
  ],
});
