import { expect, test } from "@playwright/test";
import { renderLoginPage } from "../../../auth/src/templates/login";
import { renderAuthorizePage } from "../../../auth/src/templates/authorize";
import { renderLocalLoginPage } from "../../../auth/src/local/loginPage";

// Render the shipped auth templates in a browser without a session or a remote auth service.
for (const locale of ["cs", "ar"] as const) {
  test(`sign-in and consent show the brand in ${locale}`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 320, height: 844 });
    await page.route("**/api/refresh-session", (route) => route.fulfill({ status: 401, body: "{}" }));
    await page.route("**/branding-login", (route) => route.fulfill({
      contentType: "text/html",
      body: renderLoginPage("http://127.0.0.1:4318/", "http://127.0.0.1:4318/", locale),
    }));
    await page.goto("/branding-login");
    await expect(page).toHaveTitle(/^lingvichr · /);
    const brand = page.locator(".login-brand");
    await expect(brand).toHaveText("lingvichr");
    await expect(brand).toBeInViewport({ ratio: 1 });
    await expect.poll(() => brand.locator("img").evaluate((image: HTMLImageElement) => image.naturalWidth)).toBeGreaterThan(0);
    await expect(page.locator("#login-email")).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath(`sign-in-${locale}.png`) });

    await page.route("**/api/refresh-session", (route) => route.fulfill({ status: 200, body: "{}" }));
    await page.route("**/branding-consent", (route) => route.fulfill({
      contentType: "text/html",
      body: renderAuthorizePage({
        clientId: "branding-preview", clientName: "Study assistant", state: null,
        redirectUri: "http://127.0.0.1:4318/", codeChallenge: "branding-preview",
        scope: "flashcards", resource: "http://127.0.0.1:4318/", nonce: null,
        issuer: "http://127.0.0.1:4318/",
      }, locale),
    }));
    await page.goto("/branding-consent");
    await expect(page.locator("#consent-lead")).toContainText("lingvichr");
    await expect(page.locator("#approve-btn")).toBeVisible();
    await expect(brand).toBeInViewport({ ratio: 1 });
    await expect.poll(() => brand.locator("img").evaluate((image: HTMLImageElement) => image.naturalWidth)).toBeGreaterThan(0);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath(`consent-${locale}.png`) });
  });
}

test("passkey page loads the logo with its restrictive image policy", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 320, height: 844 });
  await page.route("**/assets/local-passkey.js", (route) => route.fulfill({ contentType: "text/javascript", body: "" }));
  await page.route("**/branding-passkey", (route) => route.fulfill({
    contentType: "text/html",
    headers: {
      "Content-Security-Policy": "default-src 'none'; img-src data:; style-src 'nonce-branding'; script-src 'nonce-branding'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'",
    },
    body: renderLocalLoginPage("branding-preview", "http://127.0.0.1:4318/", "branding", false),
  }));
  await page.goto("/branding-passkey");
  await expect(page).toHaveTitle("lingvichr");
  const brand = page.locator("header .login-brand");
  await expect(brand).toHaveText("lingvichr");
  await expect.poll(() => brand.locator("img").evaluate((image: HTMLImageElement) => image.naturalWidth)).toBeGreaterThan(0);
  await expect(page.getByRole("button", { name: "Sign in with passkey" })).toBeInViewport({ ratio: 1 });
  await page.screenshot({ path: testInfo.outputPath("passkey-sign-in.png") });
});
