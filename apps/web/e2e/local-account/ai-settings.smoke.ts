import { expect, test, type Page } from "@playwright/test";

async function expectNoHorizontalOverflow(page: Page): Promise<void> {
  expect(await page.evaluate(() => ({
    viewport: document.documentElement.clientWidth,
    page: document.documentElement.scrollWidth,
    body: document.body.scrollWidth,
  }))).toEqual({ viewport: 320, page: 320, body: 320 });
}

test.use({ viewport: { width: 320, height: 720 }, locale: "en-US" });

test("loads the real provider and starts and cancels device sign-in", async ({ page }) => {
  const loaded = page.waitForResponse((response) =>
    response.url().endsWith("/v1/ai/settings") && response.request().method() === "GET" && response.ok());
  await page.goto("/settings/ai");
  expect(await (await loaded).json()).toMatchObject({ enabled: true, provider: "api", login: null });
  await expect(page.getByRole("heading", { name: "AI settings", exact: true })).toBeVisible();
  await expect(page.getByRole("radio", { name: "OpenAI API", exact: true })).toBeChecked();
  await expectNoHorizontalOverflow(page);

  const started = page.waitForResponse((response) =>
    response.url().endsWith("/v1/ai/settings/chatgpt/start") && response.request().method() === "POST" && response.ok());
  await page.getByRole("button", { name: "Connect ChatGPT", exact: true }).click();
  await started;
  await expect(page.locator(".ai-device-code")).toHaveText("TEST-1234");
  await expectNoHorizontalOverflow(page);

  const cancelled = page.waitForResponse((response) =>
    response.url().endsWith("/v1/ai/settings") && response.request().method() === "POST" && response.ok());
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  expect(await (await cancelled).json()).toMatchObject({ login: null });
  await expect(page.locator(".ai-device-code")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Connect ChatGPT", exact: true })).toBeEnabled();
  await expectNoHorizontalOverflow(page);
});
