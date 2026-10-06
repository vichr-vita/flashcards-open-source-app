import { expect, test, type Page } from "@playwright/test";

async function expectNoOverflow(page: Page): Promise<void> {
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  for (const selector of [".topbar", ".review-pane", ".review-card-menu", ".review-editor-modal"]) {
    for (const element of await page.locator(selector).all()) {
      expect(await element.evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
    }
  }
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("flashcards-chat-open", "false"));
});

for (const width of [320, 390]) {
  for (const theme of ["dark", "light"] as const) {
    test(`${theme}, ${width}px: compact header, card menu, editing, and ratings`, async ({ page }, testInfo) => {
      await page.setViewportSize({ width, height: 844 });
      await page.emulateMedia({ colorScheme: theme, reducedMotion: "reduce" });
      await page.goto("/?long-tag");
      const header = page.locator(".topbar");
      const trigger = page.getByTestId("review-card-menu-trigger");
      const menu = page.getByRole("region", { name: "Tags", exact: true });
      await expect(page.getByTestId("topbar-screen-title")).toHaveText("Review");
      await expect(header.getByRole("link", { name: "lingvichr", exact: true }).locator("img")).toBeVisible();
      for (const control of await header.locator("button").all()) {
        await expect(control).toBeInViewport({ ratio: 1 });
        const size = await control.evaluate((element) => {
          const bounds = element.getBoundingClientRect();
          return { width: bounds.width, height: bounds.height, border: getComputedStyle(element).borderTopWidth };
        });
        expect(size.width).toBeGreaterThanOrEqual(44);
        expect(size.height).toBeGreaterThanOrEqual(44);
        expect(size.border).toBe("0px");
      }
      expect((await header.boundingBox())!.height).toBeLessThanOrEqual(52);
      await expect(page.getByText("latin-ranieri-dowling", { exact: true })).toHaveCount(0);
      await expectNoOverflow(page);

      await trigger.click();
      await expect(trigger).toHaveAttribute("aria-expanded", "true");
      await expect(menu).toBeInViewport({ ratio: 1 });
      await expect(menu).toContainText("latin-".repeat(40));
      const edit = menu.getByRole("button", { name: "Edit", exact: true });
      await expect(edit).toBeFocused();
      await expectNoOverflow(page);
      await page.screenshot({ path: testInfo.outputPath("card-menu.png") });

      // Card actions must not consume the review keyboard shortcuts.
      await page.keyboard.press("1");
      await expect(page.getByTestId("review-card-flipper")).toHaveAttribute("data-side", "front");
      await page.keyboard.press("Escape");
      await expect(menu).toHaveCount(0);
      await expect(trigger).toBeFocused();
      await trigger.click();
      await page.getByTestId("review-reveal-answer").click();
      await expect(menu).toHaveCount(0);
      await expect(page.getByTestId("review-rate-good")).toBeVisible();
      await expect(page.locator(".rating-btn-subtitle")).toHaveCount(0);
      for (const name of ["again", "hard", "good", "easy"]) {
        await expect(page.getByTestId(`review-rate-${name}`).locator(".rating-btn-title")).toHaveText(new RegExp(`^${name}$`, "i"));
      }

      await trigger.click();
      await page.keyboard.press("3");
      await expect(page.getByTestId("review-pane")).toHaveAttribute("data-review-current-card-id", "latin-agreement");
      await edit.click();
      const editor = page.getByRole("dialog");
      await expect(editor).toBeVisible();
      await expect(menu).toHaveCount(0);
      await expectNoOverflow(page);
      await editor.getByLabel("Front", { exact: true }).fill("Updated review prompt");
      await editor.getByRole("button", { name: "Save card", exact: true }).click();
      await expect(editor).toHaveCount(0);
      await expect(page.getByTestId("review-current-front-card")).toContainText("Updated review prompt");
      await trigger.click();
      // Pointer input outside the popup can advance the queue without leaving stale labels open.
      await page.getByTestId("review-rate-good").click();
      await expect(menu).toHaveCount(0);
      await expect(page.getByTestId("review-current-front-card")).toContainText("What does the ablative case express?");
      await expectNoOverflow(page);
      await page.screenshot({ path: testInfo.outputPath("compact-header.png") });
    });
  }
}

test("screen heading follows navigation, logo remains, and header sparkle controls chat", async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 844 });
  await page.goto("/");
  const chat = page.locator(".topbar").getByRole("button", { name: "AI chat", exact: true });
  await expect(chat).toHaveText("");
  await chat.click();
  await expect(chat).toHaveAttribute("aria-expanded", "true");
  await expect(page.getByRole("complementary", { name: "AI chat" })).toBeVisible();
  await chat.click();
  await expect(page.getByRole("complementary", { name: "AI chat" })).toHaveCount(0);
  for (const [route, title] of [["cards", "Cards"], ["progress", "Progress"], ["settings", "Settings"], ["chat", "AI chat"]]) {
    await page.getByRole("button", { name: "Primary navigation", exact: true }).click();
    await page.getByRole("navigation", { name: "Fixture navigation" }).getByRole("link", { name: route, exact: true }).click();
    await expect(page.getByTestId("topbar-screen-title")).toHaveText(title);
    await expect(page.getByTestId("review-card-menu-trigger")).toHaveCount(0);
    await expect(page.locator(".topbar").getByRole("link", { name: "lingvichr", exact: true }).locator("img")).toBeVisible();
    await expectNoOverflow(page);
  }
  await expect(chat).toBeDisabled();
  await page.getByRole("link", { name: "lingvichr", exact: true }).click();
  await expect(page.getByTestId("topbar-screen-title")).toHaveText("Review");
  await expect(page.getByTestId("review-card-menu-trigger")).toBeVisible();
});

test("a card without labels still offers editing; loading exposes no stale card action", async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 844 });
  await page.goto("/?no-tags");
  await page.getByTestId("review-card-menu-trigger").click();
  const menu = page.getByRole("region", { name: "Tags", exact: true });
  await expect(menu).toContainText("No tags");
  await expect(menu.getByRole("button", { name: "Edit", exact: true })).toBeEnabled();
  await expectNoOverflow(page);
  await page.goto("/?loading");
  await expect(page.getByTestId("review-card-menu-trigger")).toHaveCount(0);
  await expect(page.getByTestId("review-reveal-answer")).toBeDisabled();
  await expectNoOverflow(page);
});
