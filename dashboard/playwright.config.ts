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
    { name: "offline-contract", testMatch: "offline.spec.ts" },
    {
      name: "live-postgres",
      testMatch: "live.spec.ts",
      use: { baseURL: process.env.WMS_BROWSER_URL || "http://127.0.0.1:8088" },
    },
  ],
});
