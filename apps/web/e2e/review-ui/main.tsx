import { useState, type ReactElement } from "react";
import { createRoot } from "react-dom/client";
import { MemoryRouter } from "react-router";
import { UserRound } from "lucide-react";
import { I18nProvider, useI18n } from "../../src/i18n";
import { initializeTheme, setThemePreference } from "../../src/theme";
import type { Card, ReviewFilter, ReviewRating } from "../../src/types";
import { ReviewPane } from "../../src/screens/review/components/ReviewPane";
import { ReviewScreenHeader } from "../../src/screens/review/components/ReviewScreenHeader";
import { ReviewQueuePanel } from "../../src/screens/review/components/ReviewQueuePanel";
import type { LastSubmittedReview } from "../../src/screens/review/components/reviewScreenTypes";
import { useReviewFilterMenu } from "../../src/screens/review/filters/useReviewFilterMenu";
import { useReviewKeyboardShortcuts } from "../../src/screens/review/input/useReviewKeyboardShortcuts";
import type { ReviewSpeechSide } from "../../src/screens/review/speech/reviewSpeech";
import "../../src/styles/index.css";

const createdAt = "2026-10-02T09:00:00.000Z";
const parameters = new URLSearchParams(location.search);
const longContent = parameters.has("long");
const sampleCard: Card = {
  cardId: "latin-agreement",
  frontText: "Adjective agreement · which features?",
  backText: longContent
    ? Array.from({ length: 24 }, (_, index) => `### Example ${index + 1}\n\nAn adjective agrees with its noun in gender, number, and case.\n\n\`\`\`text\nbona puella\n\`\`\``).join("\n\n")
    : "Gender, number, and case.",
  cardType: "basic",
  metadata: { version: 1, source: null },
  tags: ["latin-ranieri-dowling", "latin-foundations", "latin-concept", "latin-start"],
  dueAt: null,
  createdAt,
  reps: 1,
  lapses: 0,
  fsrsCardState: "review",
  fsrsStepIndex: null,
  fsrsStability: null,
  fsrsDifficulty: null,
  fsrsLastReviewedAt: null,
  fsrsScheduledDays: null,
  clientUpdatedAt: createdAt,
  lastModifiedByReplicaId: "review-ui",
  lastOperationId: "review-ui",
  updatedAt: createdAt,
  deletedAt: null,
};

/** Browser integration fixture for production review components, with an in-memory card queue. */
function ReviewUiFixture(): ReactElement {
  const { t } = useI18n();
  const [card, setCard] = useState(sampleCard);
  const [isAnswerVisible, setIsAnswerVisible] = useState(false);
  const [isQueueOpen, setIsQueueOpen] = useState(false);
  const [selectedReviewFilter, setSelectedReviewFilter] = useState<ReviewFilter>({ kind: "allCards" });
  const [activeSpeechSide, setActiveSpeechSide] = useState<ReviewSpeechSide | null>(null);
  const [lastSubmittedReview, setLastSubmittedReview] = useState<LastSubmittedReview | null>(null);
  const filterMenu = useReviewFilterMenu({
    deckSummaries: [],
    reviewTagSummaries: sampleCard.tags.map((tag) => ({ tag, cardsCount: 2 })),
    onSelectReviewFilter: setSelectedReviewFilter,
    selectedReviewFilter,
    workspaceId: "review-ui-workspace",
  });

  async function handleReview(reviewedCard: Card, rating: ReviewRating): Promise<void> {
    setLastSubmittedReview({ cardId: reviewedCard.cardId, rating });
    setCard({ ...sampleCard, cardId: "latin-case", frontText: "What does the ablative case express?" });
    setIsAnswerVisible(false);
    setActiveSpeechSide(null);
  }

  const { handleShortcutButtonPointerEnter } = useReviewKeyboardShortcuts({
    handleReview,
    isAnswerVisible,
    isEditorPresented: false,
    isHardReminderVisible: false,
    isReviewFilterMenuOpen: filterMenu.isReviewFilterMenuOpen,
    isSubmitting: false,
    onShortcutInputStart: () => {},
    selectedCard: card,
    setIsAnswerVisible,
  });

  return (
    <div className="app-shell">
      <header className="header-sticky">
        <div className="topbar-shell">
          <div className="topbar">
            <div className="topbar-brand-block"><span className="topbar-brand">Nibomo</span></div>
            <nav className="nav">
              {["Review", "Progress", "AI chat", "Cards", "Settings"].map((label, index) => (
                <a key={label} className={`nav-link${index === 0 ? " nav-link-active" : ""}`} href="#">{label}</a>
              ))}
            </nav>
            <div className="topbar-actions"><button className="account-menu-button" aria-label="Account"><UserRound size={20} /></button></div>
          </div>
        </div>
      </header>
      <main className="container">
        <section className="panel review-screen-panel">
          <ReviewScreenHeader
            filterMenuProps={{ ...filterMenu, selectedReviewFilterTitle: t("filters.allCards") }}
            hasLoadedReviewData
            isReviewQueuePanelOpen={isQueueOpen}
            onRetry={() => {}}
            onReviewQueueShortcutClick={() => setIsQueueOpen(!isQueueOpen)}
            reviewQueueTotalCount={2}
            reviewLoadErrorMessage=""
            reviewLeaderboardBadge={{ isInteractive: true, rank: null }}
            reviewProgressBadge={{
              streakDays: 2,
              hasReviewedToday: true,
              isInteractive: true,
              streakFreeze: { availableCredits: 0, capacity: 2, balanceUnits: 0, unitsPerCredit: 3, earnedUnitsPerStreakDay: 1, nextCreditProgressUnits: 0, nextCreditRequiredUnits: 3 },
            }}
            reviewSpeechMessage=""
          />
          <div className={`review-layout${isQueueOpen ? " review-layout-queue-open" : ""}`}>
            <div className="review-pane-reaction-frame">
              <ReviewPane
                activeSpeechSide={activeSpeechSide}
                hasCards
                isAnswerVisible={isAnswerVisible}
                isInitialReviewLoad={parameters.has("loading")}
                isSubmitting={false}
                lastSubmittedReview={lastSubmittedReview}
                localReadVersion={0}
                loadingReviewCurrentCard={null}
                onAiHandoff={async () => true}
                onEditCard={() => {}}
                onRevealAnswer={() => setIsAnswerVisible(true)}
                onReview={handleReview}
                onShortcutButtonPointerEnter={handleShortcutButtonPointerEnter}
                onSwitchToAllCards={() => {}}
                onToggleSpeech={(side) => setActiveSpeechSide(activeSpeechSide === side ? null : side)}
                reviewButtonErrorMessage=""
                reviewButtonOptions={[
                  { rating: 0, testId: "again", title: t("reviewScreen.ratings.again"), intervalDescription: "< 1 min" },
                  { rating: 2, testId: "good", title: t("reviewScreen.ratings.good"), intervalDescription: "3 days" },
                  { rating: 1, testId: "hard", title: t("reviewScreen.ratings.hard"), intervalDescription: "1 day" },
                  { rating: 3, testId: "easy", title: t("reviewScreen.ratings.easy"), intervalDescription: "7 days" },
                ]}
                reviewLoadingSnapshot={null}
                reviewSubmitState={lastSubmittedReview === null ? "idle" : "settled"}
                selectedBackSpeakableText={card.backText}
                selectedCard={card}
                selectedFrontSpeakableText={card.frontText}
                shouldShowSwitchToAllCardsAction={false}
                workspaceId={null}
              />
            </div>
            {isQueueOpen ? <ReviewQueuePanel
              isInitialReviewLoad={false}
              isReviewQueuePanelOpen={true}
              loadingReviewCurrentCard={null}
              nowTimestamp={Date.now()}
              onClose={() => setIsQueueOpen(false)}
              queueCards={[card]}
              reviewLoadingSnapshot={null}
              selectedCardId={card.cardId}
              visibleQueueCardsCount={1}
            /> : null}
          </div>
        </section>
      </main>
    </div>
  );
}

initializeTheme();
const previewTheme = parameters.get("theme");
if (previewTheme === "dark" || previewTheme === "light") setThemePreference(previewTheme);
// Match the reference's user-selected accent without changing the product's color preferences.
document.documentElement.style.setProperty("--accent", "#9472f4");
document.documentElement.style.setProperty("--accent-rgb", "148, 114, 244");
const rootElement = document.getElementById("root");
if (rootElement === null) throw new Error("Review UI fixture root is missing");
createRoot(rootElement).render(<I18nProvider><MemoryRouter><ReviewUiFixture /></MemoryRouter></I18nProvider>);
