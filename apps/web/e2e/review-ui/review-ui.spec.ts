import { expect, test, type Page } from "@playwright/test";

const fixturePath = "/";
const viewports = [
  { name: "desktop", width: 1514, height: 1033 },
  { name: "laptop", width: 1280, height: 800 },
  { name: "mobile", width: 390, height: 844 },
];

async function expectNoHorizontalOverflow(page: Page): Promise<void> {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  const pane = page.getByTestId("review-pane");
  expect(await pane.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
}

for (const theme of ["dark", "light"] as const) {
  test.describe(theme, () => {
    test.use({ colorScheme: theme });

    for (const viewport of viewports) {
      test.describe(viewport.name, () => {
        test.use({ viewport, isMobile: viewport.name === "mobile", hasTouch: viewport.name === "mobile" });
        test(`${viewport.name}: reveal, speech, keyboard rating, and next card`, async ({ page }, testInfo) => {
          await page.setViewportSize(viewport);
          const pageErrors: string[] = [];
          page.on("pageerror", (error) => pageErrors.push(error.message));
          page.on("console", (message) => {
            if (message.type() === "error") pageErrors.push(message.text());
          });
          await page.goto(fixturePath);
          const brand = page.getByRole("link", { name: "lingvichr", exact: true });
          await expect(brand).toBeVisible();
          await expect.poll(() => brand.locator("img").evaluate((image: HTMLImageElement) => image.naturalWidth)).toBeGreaterThan(0);
          const front = page.getByTestId("review-current-front-card");
          const flipper = page.getByTestId("review-card-flipper");
          const reveal = page.getByTestId("review-reveal-answer");
          await expect(front).toContainText("Adjective agreement · which features?");
          await expect(page.getByText("Gender, number, and case.")).toHaveCount(0);
          await expect(reveal).toBeInViewport({ ratio: 1 });
          await expectNoHorizontalOverflow(page);

          const surface = await front.evaluate((element) => {
            const style = getComputedStyle(element);
            return { background: style.backgroundColor, border: style.borderTopColor, shadow: style.boxShadow, height: element.getBoundingClientRect().height };
          });
          expect(surface.background).toBe(theme === "light" ? "rgb(255, 255, 255)" : "rgb(24, 24, 28)");
          expect(surface.shadow).not.toBe("none");
          expect(surface.height).toBeGreaterThanOrEqual(220);
          await page.screenshot({ path: testInfo.outputPath("front.png") });

          await front.getByRole("button").click();
          await expect(front.locator(".review-card-speech-btn")).toHaveClass(/review-card-speech-btn-active/);
          // Space uses the production shortcut handler and the same flip as the reveal button.
          await page.keyboard.press("Space");
          await expect(flipper).toHaveAttribute("data-side", "back");
          await expect(page.getByTestId("review-current-back-card")).toContainText("Gender, number, and case.");
          await expect(flipper.locator(".review-card-face-front")).toHaveAttribute("inert", "");
          await expect(flipper.locator(".review-card-face-front")).toHaveAttribute("aria-hidden", "true");
          await expect.poll(() => flipper.evaluate((element) => new DOMMatrixReadOnly(getComputedStyle(element).transform).m11)).toBe(-1);
          await expect(page.getByTestId("review-rate-good")).toBeInViewport({ ratio: 1 });
          await page.screenshot({ path: testInfo.outputPath("back.png") });
          await page.keyboard.press("3");
          await expect(page.getByTestId("review-pane")).toHaveAttribute("data-review-last-submitted-card-id", "latin-agreement");
          await expect(page.getByTestId("review-pane")).toHaveAttribute("data-review-last-submitted-rating", "2");
          await expect(front).toContainText("What does the ablative case express?");
          await expect(flipper).toHaveAttribute("data-side", "front");
          expect(await flipper.evaluate((element) => new DOMMatrixReadOnly(getComputedStyle(element).transform).m11)).toBe(1);
          await expect(page.getByTestId("review-current-back-card")).toHaveCount(0);
          await expectNoHorizontalOverflow(page);
          expect(pageErrors).toEqual([]);
        });
      });
    }

    test("brand and navigation fit a narrow phone", async ({ page }, testInfo) => {
      await page.setViewportSize({ width: 320, height: 844 });
      await page.goto(fixturePath);
      const brand = page.getByRole("link", { name: "lingvichr", exact: true });
      const navigation = page.getByRole("button", { name: "Primary navigation", exact: true });
      await expect(brand).toBeInViewport({ ratio: 1 });
      await expect(navigation).toBeInViewport({ ratio: 1 });
      await expect(page.getByRole("button", { name: "Account", exact: true })).toBeInViewport({ ratio: 1 });
      const brandBounds = await brand.boundingBox();
      const navigationBounds = await navigation.boundingBox();
      expect(brandBounds).not.toBeNull();
      expect(navigationBounds).not.toBeNull();
      expect(brandBounds!.x + brandBounds!.width).toBeLessThanOrEqual(navigationBounds!.x);
      await expectNoHorizontalOverflow(page);
      await page.screenshot({ path: testInfo.outputPath("branding-narrow-phone.png") });
      await brand.click();
      await expect(page.getByTestId("review-reveal-answer")).toBeVisible();
    });

    test("click starts a Y-axis rotation; reduced motion reveals immediately", async ({ page }) => {
      await page.goto(fixturePath);
      await expect(page.getByTestId("review-reveal-answer")).toBeVisible();
      const midFlip = await page.evaluate(async () => {
        document.querySelector<HTMLButtonElement>("[data-testid=review-reveal-answer]")?.click();
        // Sample a frame during the finite transition, rather than only its final CSS class.
        await new Promise((resolve) => setTimeout(resolve, 120));
        const flipper = document.querySelector("[data-testid=review-card-flipper]");
        if (flipper === null) throw new Error("Missing flipper");
        const matrix = new DOMMatrixReadOnly(getComputedStyle(flipper).transform);
        return { x: matrix.m11, z: matrix.m13 };
      });
      expect(Math.abs(midFlip.x)).toBeLessThan(0.999);
      expect(Math.abs(midFlip.z)).toBeGreaterThan(0.01);

      await page.emulateMedia({ reducedMotion: "reduce" });
      await page.reload();
      await page.getByTestId("review-reveal-answer").click();
      const reducedFlip = await page.getByTestId("review-card-flipper").evaluate((element) => ({
        duration: getComputedStyle(element).transitionDuration,
        x: new DOMMatrixReadOnly(getComputedStyle(element).transform).m11,
      }));
      expect(reducedFlip).toEqual({ duration: "0s", x: -1 });
      await expect(page.getByTestId("review-current-back-card")).toBeVisible();
    });

    test("long markdown answer scrolls fully and keeps ratings reachable", async ({ page }, testInfo) => {
      await page.setViewportSize({ width: 390, height: 844 });
      await page.goto(`${fixturePath}?long`);
      await page.getByTestId("review-reveal-answer").click();
      const pane = page.getByTestId("review-pane");
      await expect(page.getByTestId("review-current-back-card").getByRole("heading", { name: "Example 24", exact: true })).toBeAttached();
      await pane.evaluate((element) => { element.scrollTop = element.scrollHeight; });
      await expect(page.getByRole("heading", { name: "Example 24", exact: true })).toBeInViewport();
      await expect(page.getByTestId("review-rate-good")).toBeInViewport({ ratio: 1 });
      await expectNoHorizontalOverflow(page);
      await page.screenshot({ path: testInfo.outputPath("long-answer.png") });
      await page.getByTestId("review-rate-good").click();
      await expect(page.getByTestId("review-current-front-card")).toContainText("What does the ablative case express?");
    });
  });
}
