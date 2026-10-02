import { expect, test } from "@playwright/test";

test("appearance choice persists, follows the system, and the logo opens Review", async ({ page }) => {
  await page.goto("/settings/appearance");
  await expect(page.getByRole("heading", { name: "Appearance" })).toBeVisible();

  for (const option of ["system", "light", "dark"] as const) {
    await expect(page.getByTestId(`appearance-option-${option}`).locator("xpath=..").locator("svg")).toHaveCount(1);
  }

  await page.getByTestId("appearance-option-light").check();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect(page.locator("html")).toHaveCSS("background-color", "rgb(245, 245, 247)");
  await page.reload();
  await expect(page.getByTestId("appearance-option-light")).toBeChecked();

  await page.getByTestId("appearance-option-dark").check();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.locator("html")).toHaveCSS("background-color", "rgb(0, 0, 0)");

  await page.emulateMedia({ colorScheme: "light" });
  await page.getByTestId("appearance-option-system").check();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  const logo = page.locator(".topbar-brand");
  const logoDestination = await logo.getAttribute("href");
  if (logoDestination === null) {
    throw new Error("The app logo has no destination.");
  }
  expect(new URL(logoDestination, page.url()).origin).toBe(new URL(page.url()).origin);
  await logo.click();
  await expect(page).toHaveURL(/\/review$/);
});
