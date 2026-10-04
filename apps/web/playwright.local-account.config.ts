import { defineConfig } from "@playwright/test";

const storageState = process.env.LOCAL_AUTH_BROWSER_STATE;
if (storageState === undefined) {
  throw new Error("Run this browser check through the disposable local-auth integration fixture.");
}

export default defineConfig({
  testDir: "./e2e/local-account",
  testMatch: "*.smoke.ts",
  timeout: 60_000,
  workers: 1,
  reporter: "list",
  outputDir: "test-results/local-account",
  use: {
    baseURL: "http://localhost:19411",
    storageState,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    serviceWorkers: "block",
  },
  projects: [
    { name: "chromium", use: { browserName: "chromium" } },
    { name: "webkit", use: { browserName: "webkit" } },
  ],
  webServer: {
    command: "pnpm exec vite preview --outDir dist --host 127.0.0.1 --port 19411 --strictPort",
    url: "http://localhost:19411",
    reuseExistingServer: false,
  },
});
