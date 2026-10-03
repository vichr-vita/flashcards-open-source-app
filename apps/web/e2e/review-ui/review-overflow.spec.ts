import { expect, test } from "@playwright/test";

for (const theme of ["dark", "light"] as const) {
  for (const width of [320, 390]) {
    test(`${theme}, ${width}px: source text wraps and long answers remain scrollable`, async ({ page }, testInfo) => {
      await page.setViewportSize({ width, height: 844 });
      await page.emulateMedia({ colorScheme: theme, reducedMotion: "reduce" });
      await page.goto("/?source");
      await page.getByTestId("review-reveal-answer").click();
      const content = page.getByTestId("review-current-back-card").locator(".review-card-content");
      await expect(content).toContainText("https://dcc.dickinson.edu/grammar/latin/number-and-case.");
      expect(await content.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
      const pane = page.getByTestId("review-pane");
      expect(await pane.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await page.screenshot({ path: testInfo.outputPath("source-answer.png") });

      await page.getByTestId("review-filter-trigger").click();
      await expect(page.getByRole("listbox")).toBeInViewport({ ratio: 1 });
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await page.keyboard.press("Escape");
      await expect(page.getByRole("listbox")).toHaveCount(0);

      await page.goto("/?long");
      await page.getByTestId("review-reveal-answer").click();
      await pane.evaluate((element) => { element.scrollTop = element.scrollHeight; });
      expect(await pane.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
      await expect(page.getByRole("heading", { name: "Example 24", exact: true })).toBeInViewport();
      await expect(page.getByTestId("review-rate-good")).toBeInViewport({ ratio: 1 });
      await page.getByTestId("review-rate-good").click();
      await expect(page.getByTestId("review-current-front-card")).toContainText("What does the ablative case express?");
    });
  }
}
