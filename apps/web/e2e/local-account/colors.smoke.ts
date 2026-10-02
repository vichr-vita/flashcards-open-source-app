import { expect, test } from "@playwright/test";

test("a free account saves preset and custom colors across reloads", async ({ page }) => {
  await page.goto("/settings/accent-color");
  const worker = await page.request.get("/sw.js");
  expect(worker.ok()).toBe(true);
  expect(await worker.text()).toContain('self.addEventListener("install"');
  const purple = page.getByTestId("accent-preset-purple");
  await expect(purple).toBeEnabled();

  const presetSaved = page.waitForResponse((response) =>
    response.url().endsWith("/v1/me/preferences")
    && response.request().method() === "PATCH" && response.ok());
  await purple.check();
  await presetSaved;
  await expect(page.locator("html")).toHaveCSS("--accent", "#A78BFA");
  await page.reload();
  await expect(purple).toBeChecked();

  const custom = page.getByTestId("accent-custom-hex");
  const customSaved = page.waitForResponse((response) =>
    response.url().endsWith("/v1/me/preferences")
    && response.request().method() === "PATCH" && response.ok());
  await custom.fill("#123ABC");
  await customSaved;
  await expect(page.locator("html")).toHaveCSS("--accent", "#123ABC");
  await page.reload();
  await expect(custom).toHaveValue("#123ABC");
  await expect(page.getByTestId("accent-custom-selected")).toBeVisible();
  await expect(page.locator("html")).toHaveCSS("--accent", "#123ABC");
});
