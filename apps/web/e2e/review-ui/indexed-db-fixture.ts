import type { Card } from "../../src/types";
import { iterateCardsByCreatedAtDesc, queryLocalCardsPage, replaceCards } from "../../src/localDb/cards/cards";
import { loadDecksListSnapshot } from "../../src/localDb/cards/decks";
import { loadWorkspaceTagsSummary } from "../../src/localDb/cards/workspace";
import { loadReviewQueueSnapshot } from "../../src/localDb/reviews/reviews";
import { closeDatabaseAfter, getAllFromStore } from "../../src/localDb/core/database";
import { listOutboxRecords, putOutboxRecord } from "../../src/localDb/sync/outbox";

const workspaceId = "browser-storage-smoke";
const timestamp = "2025-01-01T00:00:00.000Z";
const cards: ReadonlyArray<Card> = Array.from({ length: 12 }, (_, index) => ({
  cardId: `card-${String(index).padStart(2, "0")}`,
  frontText: `Question ${index}`,
  backText: `Answer ${index}`,
  cardType: "basic",
  metadata: { version: 1, source: null },
  tags: ["smoke"],
  dueAt: null,
  createdAt: new Date(Date.parse(timestamp) + index * 1000).toISOString(),
  reps: 0,
  lapses: 0,
  fsrsCardState: "new",
  fsrsStepIndex: null,
  fsrsStability: null,
  fsrsDifficulty: null,
  fsrsLastReviewedAt: null,
  fsrsScheduledDays: null,
  clientUpdatedAt: timestamp,
  lastModifiedByReplicaId: "browser-smoke",
  lastOperationId: `operation-${index}`,
  updatedAt: new Date(Date.parse(timestamp) + index * 1000).toISOString(),
  deletedAt: null,
}));

type FailureMode = "once" | "persistent" | "other" | "abort";
let cursorRequests = 0;
let failedRequests = 0;
let databaseOpens = 0;
let restoreFailure: (() => void) | null = null;

function injectFailure(mode: FailureMode, indexName = "workspaceId_updatedAt_cardId"): void {
  restoreFailure?.();
  cursorRequests = 0;
  failedRequests = 0;
  databaseOpens = 0;
  const originalOpen = IDBFactory.prototype.open;
  const originalCursor = IDBIndex.prototype.openCursor;
  IDBFactory.prototype.open = function (...arguments_) {
    databaseOpens += 1;
    return originalOpen.apply(this, arguments_);
  };
  IDBIndex.prototype.openCursor = function (...arguments_) {
    const request = originalCursor.apply(this, arguments_);
    if (this.name !== indexName) return request;
    cursorRequests += 1;
    let successes = 0;
    request.addEventListener("success", (event) => {
      successes += 1;
      if (successes !== 4 || request.result === null || (mode === "once" && failedRequests > 0)) return;
      // Fail after real records reached the query's accumulators. The native
      // transaction still aborts; only WebKit's missing-cursor response is injected.
      event.stopImmediatePropagation();
      failedRequests += 1;
      if (mode !== "abort") {
        const error = new DOMException(
          mode === "other" ? "Unrelated storage failure" : "Attempt to iterate a cursor that doesn't exist",
          "UnknownError",
        );
        Object.defineProperty(request, "error", { value: error });
        request.dispatchEvent(new Event("error", { cancelable: true, bubbles: true }));
      }
      request.transaction?.abort();
    });
    return request;
  };
  restoreFailure = () => {
    IDBFactory.prototype.open = originalOpen;
    IDBIndex.prototype.openCursor = originalCursor;
    restoreFailure = null;
  };
}

function errorDetails(error: unknown) {
  if (!(error instanceof Error)) throw error;
  return {
    message: error.message,
    sourceName: error.cause instanceof DOMException ? error.cause.name : null,
  };
}

const fixture = {
  async seed() {
    await replaceCards(workspaceId, cards);
    const card = cards[0];
    if (card === undefined) throw new Error("Browser smoke card is missing");
    await putOutboxRecord({
      operationId: "pending-offline-operation",
      workspaceId,
      createdAt: timestamp,
      attemptCount: 0,
      lastError: "",
      operation: {
        operationId: "pending-offline-operation",
        entityType: "card",
        action: "upsert",
        entityId: card.cardId,
        clientUpdatedAt: timestamp,
        payload: { ...card, effortLevel: "fast" },
      },
    });
  },
  injectFailure,
  counters: () => ({ cursorRequests, failedRequests, databaseOpens }),
  restore: () => restoreFailure?.(),
  readCards: (cursor: string | null = null) => queryLocalCardsPage(workspaceId, {
    searchText: null, filter: null, sorts: [], cursor, limit: 5,
  }),
  readReview: () => loadReviewQueueSnapshot(workspaceId, { kind: "allCards" }, 5),
  readDecks: () => loadDecksListSnapshot(workspaceId),
  readTags: () => loadWorkspaceTagsSummary(workspaceId),
  pending: () => listOutboxRecords(workspaceId),
  async readFailure() {
    try {
      await fixture.readCards();
      throw new Error("Expected the storage read to fail");
    } catch (error) {
      return errorDetails(error);
    }
  },
  async probeIterator(mode: "stop" | "throw" | "abort") {
    let visited = 0;
    let completed = false;
    try {
      await closeDatabaseAfter(async (database) => {
        const originalTransaction = database.transaction;
        database.transaction = function (...arguments_) {
          const transaction = originalTransaction.apply(this, arguments_);
          transaction.addEventListener("complete", () => { completed = true; });
          if (mode === "abort") {
            const request = transaction.objectStore("cards").get([workspaceId, cards[0]?.cardId ?? ""]);
            request.addEventListener("success", () => transaction.abort());
          }
          return transaction;
        };
        await iterateCardsByCreatedAtDesc(database, workspaceId, () => {
          visited += 1;
          if (mode === "throw") throw new Error("Browser callback failed");
          return false;
        });
      });
      return { visited, completed, error: null };
    } catch (error) {
      return { visited, completed, error: errorDetails(error) };
    }
  },
  storedCards: () => closeDatabaseAfter((database) => getAllFromStore(database, "cards")),
};

declare global {
  interface Window { indexedDbFixture: typeof fixture }
}
window.indexedDbFixture = fixture;
