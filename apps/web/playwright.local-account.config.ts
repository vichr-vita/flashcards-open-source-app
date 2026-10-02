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
  use: {
    baseURL: "http://localhost:19411",
    storageState,
    browserName: "chromium",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    serviceWorkers: "block",
  },
  webServer: {
    command: "npm exec -- vite preview --outDir /tmp/nibomo-local-web --host 127.0.0.1 --port 19411 --strictPort",
    url: "http://localhost:19411",
    reuseExistingServer: false,
  },
});
