import { expect, test } from "@playwright/test";

test("workspace links preserve query/fragment bytes, history, account queries and error overlays", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 320, height: 844 });
  let user = 1;
  let reads = 0;
  let fail = false;
  let hold: (() => void) | undefined;
  await page.route("http://localhost:8080/v1/ai/settings", async (route) => {
    if (route.request().method() === "POST") {
      expect(route.request().headers()["x-csrf-token"]).toBe("isolated-fixture-csrf");
      user += 1;
    } else {
      reads += 1;
      if (hold !== undefined) await new Promise<void>((resolve) => { hold = resolve; });
    }
    await route.fulfill({ status: fail ? 503 : 200, contentType: "application/json", json: fail ? { error: "Unavailable" } : {
      enabled: true, provider: "api", login: null, error: null,
      connection: { email: `user-${user}@example.test`, plan: null, modelId: "model", reasoningEffort: null, models: [] },
    } });
  });
  await page.goto("/navigation.html");
  await expect(page.getByTestId("card-id")).toHaveText("initial");
  await expect(page.getByTestId("address")).toContainText("?tag=a%20b&tag=c#part%20one");
  await page.getByRole("link", { name: "Card", exact: true }).click();
  await expect(page.getByTestId("card-id")).toContainText("latin-agreement");
  await expect(page.getByRole("link", { name: "Card", exact: true })).toHaveClass("nav-link nav-link-active");
  await expect(page.getByTestId("address")).toContainText("?tag=a%20b&tag=c#part%20one");
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.goBack();
  await expect(page.getByTestId("card-id")).toHaveText("initial");
  await page.goForward();
  await expect(page.getByTestId("card-id")).toContainText("latin-agreement");

  await page.getByRole("link", { name: "AI settings", exact: true }).click();
  await expect(page.getByTestId("reader-first")).toHaveText("user-1@example.test");
  await expect(page.getByTestId("reader-second")).toHaveText("user-1@example.test");
  expect(reads).toBe(1);
  await page.getByRole("button", { name: "Use API", exact: true }).click();
  await expect(page.getByTestId("reader-first")).toHaveText("user-2@example.test");
  expect(reads).toBe(2);

  hold = () => {};
  await page.getByRole("button", { name: "Switch account", exact: true }).click();
  await expect(page.getByTestId("reader-first")).toHaveText("Loading");
  await expect.poll(() => reads).toBe(3);
  hold?.();
  hold = undefined;
  await expect(page.getByTestId("reader-first")).toHaveText("user-2@example.test");

  fail = true;
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.getByTestId("reader-first")).toHaveText("Unavailable");
  fail = false;
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.getByTestId("reader-first")).toHaveText("user-2@example.test");

  await page.getByRole("button", { name: "Open error", exact: true }).click();
  await expect(page.getByTestId("app-error-dialog")).toBeInViewport({ ratio: 1 });
  await expect(page.getByTestId("app-error-dialog-close")).toBeFocused();
  await page.getByTestId("app-error-dialog-details").locator("summary").click();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath("router-query-dialog.png") });
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("app-error-dialog")).toHaveCount(0);
});
