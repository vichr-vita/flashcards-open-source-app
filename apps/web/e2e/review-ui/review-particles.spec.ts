import { expect, test } from "@playwright/test";

for (const reducedMotion of ["no-preference", "reduce"] as const) {
  test(`320px: rating bursts, interruption, and cleanup with ${reducedMotion} motion`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 320, height: 844 });
    await page.emulateMedia({ colorScheme: "dark", reducedMotion });
    await page.clock.install();
    await page.goto("/?long-tag&theme=dark");
    await page.clock.pauseAt(new Date(Date.now() + 1000));
    const event = page.getByTestId("review-rating-reaction-event");
    for (const rating of ["hard", "good", "easy"] as const) {
      await page.getByTestId("review-reveal-answer").click();
      await page.getByTestId(`review-rate-${rating}`).click();
      await expect(event).toHaveCount(1);
      await expect(event).toHaveAttribute("data-review-reaction-rating", rating);
      await expect(page.getByTestId("review-current-front-card")).toContainText("What does the ablative case express?");
      await expect(page.getByTestId("review-reveal-answer")).toBeEnabled();
      const particles = event.locator(".review-reaction-particle");
      await expect(particles).toHaveCount(rating === "hard" ? 42 : rating === "good" ? 22 : 8);
      // Freeze real CSS motion mid-burst for visual evidence and bounds checks.
      await event.evaluate((element) => {
        for (const animation of element.getAnimations({ subtree: true })) {
          animation.pause();
          animation.currentTime = 180;
        }
      });
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      expect(await particles.evaluateAll((elements) => elements.every((element) => {
        const mark = element.querySelector("span");
        if (getComputedStyle(element).display === "none" || mark === null) return true;
        const rect = mark.getBoundingClientRect();
        return rect.left >= 0 && rect.right <= innerWidth;
      }))).toBe(true);
      await page.screenshot({ path: testInfo.outputPath(`${rating}-${reducedMotion}.png`) });
      await page.getByTestId("review-reveal-answer").click();
      await expect(event).toHaveCount(0);
      await expect(page.getByTestId("review-current-back-card")).toBeVisible();
      await page.keyboard.press("1");
      await expect(event).toHaveCount(0);
    }
    await page.keyboard.press("Space");
    await page.keyboard.press("3");
    await expect(event).toHaveAttribute("data-review-reaction-rating", "good");
    await page.clock.runFor(1200);
    await expect(event).toHaveCount(0);
    await page.keyboard.press("Space");
    await page.keyboard.press("2");
    await expect(event).toHaveCount(1);
    await page.keyboard.press("Space");
    await expect(event).toHaveCount(0);
    await page.getByTestId("review-filter-trigger").click();
    await expect(page.getByRole("listbox")).toBeInViewport({ ratio: 1 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  });
}

test("disabled animation preference still advances cards without particles", async ({ page }) => {
  await page.goto("/?no-animations");
  await page.getByTestId("review-reveal-answer").click();
  await page.getByTestId("review-rate-hard").click();
  await expect(page.getByTestId("review-current-front-card")).toContainText("What does the ablative case express?");
  await expect(page.getByTestId("review-rating-reaction-event")).toHaveCount(0);
});
