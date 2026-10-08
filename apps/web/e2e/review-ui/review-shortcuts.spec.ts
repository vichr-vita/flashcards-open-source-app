import { expect, test, type Page } from "@playwright/test";

const ratings = [
  { name: "easy", title: "Easy", key: "1", rating: "3" },
  { name: "good", title: "Good", key: "2", rating: "2" },
  { name: "hard", title: "Hard", key: "3", rating: "1" },
  { name: "again", title: "Again", key: "4", rating: "0" },
] as const;

async function expectRatingLayout(page: Page, columns: number): Promise<void> {
  await expect(page.locator(".rating-btn-title")).toHaveText(ratings.map(({ title }) => title));
  const bounds = await page.locator(".rating-btn").evaluateAll((buttons) => buttons.map((button) => {
    const rect = button.getBoundingClientRect();
    return { x: rect.x, y: rect.y, right: rect.right, width: rect.width, height: rect.height };
  }));
  expect(new Set(bounds.map(({ y }) => y)).size).toBe(4 / columns);
  for (let index = 0; index < bounds.length; index++) {
    const rect = bounds[index];
    expect(rect.width).toBeGreaterThanOrEqual(44);
    expect(rect.height).toBeGreaterThanOrEqual(44);
    if (index % columns !== 0) expect(rect.x).toBeGreaterThanOrEqual(bounds[index - 1].right);
    await expect(page.getByTestId(`review-rate-${ratings[index].name}`)).toBeInViewport({ ratio: 1 });
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  expect(await page.locator(".rating-bar").evaluate((bar) => bar.scrollWidth <= bar.clientWidth + 1)).toBe(true);
}

test.describe("desktop shortcuts", () => {
  test.skip(({ browserName }) => browserName !== "chromium", "Desktop coverage runs in Chromium.");
  test.use({ hasTouch: false, isMobile: false, colorScheme: "dark", reducedMotion: "reduce" });

  for (const width of [1280, 1514]) {
    test(`${width}px: persistent hints follow keyboard order and submit the correct rating`, async ({ page }, testInfo) => {
      await page.setViewportSize({ width, height: 900 });
      for (const [index, rating] of ratings.entries()) {
        await page.goto("/");
        await expect(page.getByTestId("review-reveal-answer").locator("kbd")).toBeVisible();
        // Number keys only rate a revealed answer.
        await page.keyboard.press(rating.key);
        await expect(page.getByTestId("review-card-flipper")).toHaveAttribute("data-side", "front");
        await page.keyboard.press("Space");
        await expectRatingLayout(page, 4);
        for (const option of ratings) {
          const button = page.getByTestId(`review-rate-${option.name}`);
          await expect(button).toHaveAttribute("aria-keyshortcuts", option.key);
          await expect(button.locator("kbd")).toHaveText(option.key);
          await expect(button.locator("kbd")).toBeInViewport({ ratio: 1 });
          expect(await button.evaluate((element) => element.matches(":hover, :focus-visible"))).toBe(false);
          expect(await button.evaluate((element) => {
            const title = element.querySelector(".rating-btn-title")!.getBoundingClientRect();
            const key = element.querySelector("kbd")!.getBoundingClientRect();
            return title.right <= key.left && key.right <= element.getBoundingClientRect().right;
          })).toBe(true);
        }
        if (index === 0) await page.screenshot({ path: testInfo.outputPath("desktop-shortcuts.png") });
        await page.keyboard.press(rating.key);
        await expect(page.getByTestId("review-pane")).toHaveAttribute("data-review-last-submitted-rating", rating.rating);
        await expect(page.getByTestId("review-current-front-card")).toContainText("What does the ablative case express?");
      }
    });
  }
});

test.describe("touch review controls", () => {
  test.use({ hasTouch: true, isMobile: true, colorScheme: "dark", reducedMotion: "reduce" });

  for (const width of [320, 390, 1280]) {
    test(`${width}px: hints stay hidden and ratings reflow with a populated answer and open filter`, async ({ page }, testInfo) => {
      await page.setViewportSize({ width, height: 844 });
      await page.goto("/?source&long-tag");
      await expect(page.getByTestId("review-reveal-answer").locator("kbd")).toBeHidden();
      await page.getByTestId("review-reveal-answer").click();
      await expectRatingLayout(page, 2);
      for (const rating of ratings) {
        await expect(page.getByTestId(`review-rate-${rating.name}`).locator("kbd")).toBeHidden();
      }
      const content = page.getByTestId("review-current-back-card").locator(".review-card-content");
      await expect(content).toContainText("https://dcc.dickinson.edu/grammar/latin/number-and-case.");
      expect(await content.evaluate((element) => element.scrollWidth <= element.clientWidth + 1)).toBe(true);
      await page.screenshot({ path: testInfo.outputPath("touch-ratings.png") });
      await page.getByTestId("review-filter-trigger").click();
      await expect(page.getByRole("listbox")).toBeInViewport({ ratio: 1 });
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await page.keyboard.press("Escape");
      await page.getByTestId("review-rate-easy").click();
      await expect(page.getByTestId("review-pane")).toHaveAttribute("data-review-last-submitted-rating", "3");
      await expect(page.getByTestId("review-current-front-card")).toContainText("What does the ablative case express?");
    });
  }
});
