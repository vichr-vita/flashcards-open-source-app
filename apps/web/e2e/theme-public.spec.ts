import { expect, test } from "@playwright/test";

test("public app follows the saved theme and system appearance", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/share");
  await expect(page.getByTestId("share-app-screen")).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator("html")).toHaveCSS("background-color", "rgb(0, 0, 0)");

  await page.evaluate(() => localStorage.setItem("flashcards-web-theme-preference", "light"));
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect(page.locator("html")).toHaveCSS("background-color", "rgb(245, 245, 247)");

  await page.evaluate(() => localStorage.removeItem("flashcards-web-theme-preference"));
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");

  const externalDestinations = await page.locator("a[href]").evaluateAll((links) =>
    links.map((link) => (link as HTMLAnchorElement).href),
  );
  expect(externalDestinations.some((destination) => new URL(destination).hostname.endsWith("nibomo.com"))).toBe(false);
});
