import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e/review-ui",
  testMatch: "*.spec.ts",
  timeout: 30_000,
  fullyParallel: true,
  workers: process.env.CI ? 2 : undefined,
  reporter: "list",
  outputDir: "test-results/review-ui",
  use: {
    baseURL: "http://127.0.0.1:4318",
    browserName: "chromium",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "npm run dev -- --config e2e/review-ui/vite.config.ts",
    url: "http://127.0.0.1:4318/",
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
});
