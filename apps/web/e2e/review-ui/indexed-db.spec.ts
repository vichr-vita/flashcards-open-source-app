import { expect, test } from "@playwright/test";
import type {} from "./indexed-db-fixture";

test.beforeEach(async ({ page }) => {
  await page.goto("/indexed-db.html");
  await page.waitForFunction(() => window.indexedDbFixture !== undefined);
  await page.evaluate(() => window.indexedDbFixture.seed());
});

test("recovers a failed card read with exact counts, pages and pending offline changes", async ({ page }) => {
  await page.evaluate(() => window.indexedDbFixture.injectFailure("once"));
  const first = await page.evaluate(() => window.indexedDbFixture.readCards());
  expect(first.totalCount).toBe(12);
  expect(first.cards.map((card) => card.cardId)).toEqual(["card-11", "card-10", "card-09", "card-08", "card-07"]);
  expect(await page.evaluate(() => window.indexedDbFixture.counters())).toEqual({
    cursorRequests: 2, failedRequests: 1, databaseOpens: 2,
  });
  const second = await page.evaluate((cursor) => window.indexedDbFixture.readCards(cursor), first.nextCursor);
  expect(second.totalCount).toBe(12);
  expect(second.cards.map((card) => card.cardId)).toEqual(["card-06", "card-05", "card-04", "card-03", "card-02"]);
  expect(await page.evaluate(() => window.indexedDbFixture.pending())).toHaveLength(1);

  await page.evaluate(() => window.indexedDbFixture.injectFailure("once", "workspaceId_createdAt_cardId"));
  const review = await page.evaluate(() => window.indexedDbFixture.readReview());
  expect(review.reviewCounts).toEqual({ totalCount: 12, dueCount: 12 });
  expect(new Set(review.cards.map((card) => card.cardId)).size).toBe(5);
  expect(await page.evaluate(() => window.indexedDbFixture.counters())).toMatchObject({ failedRequests: 1, databaseOpens: 2 });

  await page.evaluate(() => window.indexedDbFixture.injectFailure("once", "workspaceId_createdAt_cardId"));
  const decks = await page.evaluate(() => window.indexedDbFixture.readDecks());
  expect(decks.allCardsStats.totalCards).toBe(12);
  await page.evaluate(() => window.indexedDbFixture.injectFailure("once", "workspaceId_createdAt_cardId"));
  expect(await page.evaluate(() => window.indexedDbFixture.readTags())).toEqual({
    totalCards: 12, tags: [{ tag: "smoke", cardsCount: 12 }],
  });
  expect(await page.evaluate(() => window.indexedDbFixture.counters())).toMatchObject({ failedRequests: 1, databaseOpens: 2 });
  expect(await page.evaluate(() => window.indexedDbFixture.pending())).toHaveLength(1);
});

test("retries once for lost cursors and surfaces persistent or unrelated errors", async ({ page }) => {
  await page.evaluate(() => window.indexedDbFixture.injectFailure("persistent"));
  const persistent = await page.evaluate(() => window.indexedDbFixture.readFailure());
  expect(persistent.message).toContain("Attempt to iterate a cursor that doesn't exist");
  expect(persistent.sourceName).toBe("UnknownError");
  expect(await page.evaluate(() => window.indexedDbFixture.counters())).toEqual({
    cursorRequests: 2, failedRequests: 2, databaseOpens: 2,
  });
  await page.evaluate(() => window.indexedDbFixture.injectFailure("other"));
  const other = await page.evaluate(() => window.indexedDbFixture.readFailure());
  expect(other.message).toContain("Unrelated storage failure");
  expect(await page.evaluate(() => window.indexedDbFixture.counters())).toEqual({
    cursorRequests: 1, failedRequests: 1, databaseOpens: 1,
  });
  await page.evaluate(() => window.indexedDbFixture.restore());
  expect(await page.evaluate(() => window.indexedDbFixture.pending())).toHaveLength(1);
  expect(await page.evaluate(() => window.indexedDbFixture.storedCards())).toHaveLength(12);
});

test("settles early stopping, callback exceptions and aborted reads", async ({ page }) => {
  expect(await page.evaluate(() => window.indexedDbFixture.probeIterator("stop"))).toEqual({ visited: 1, completed: true, error: null });
  const thrown = await page.evaluate(() => window.indexedDbFixture.probeIterator("throw"));
  expect(thrown.error?.message).toBe("Browser callback failed");
  const aborted = await page.evaluate(() => window.indexedDbFixture.probeIterator("abort"));
  expect(aborted.error?.message).toContain("aborted");
});
