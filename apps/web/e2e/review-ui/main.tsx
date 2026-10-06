import { useState, type ReactElement } from "react";
import { createRoot } from "react-dom/client";
import { Link, MemoryRouter, useLocation } from "react-router";
import { UserRound } from "lucide-react";
import { AppHeaderActions, AppHeaderIdentity, AppHeaderProvider } from "../../src/AppHeader";
import { AppErrorDialogProvider } from "../../src/appError/AppErrorContext";
import { ChatLayoutProvider, useChatLayout } from "../../src/chat/layout/ChatLayoutContext";
import { ChatToggle } from "../../src/chat/layout/ChatToggle";
import { I18nProvider, useI18n } from "../../src/i18n";
import { initializeTheme, setThemePreference } from "../../src/theme";
import type { Card, ReviewFilter, ReviewRating } from "../../src/types";
import { useReviewRatingReactions } from "../../src/screens/review/reactions/useReviewRatingReactions";
import { ReviewPane } from "../../src/screens/review/components/ReviewPane";
import { ReviewEditorModal } from "../../src/screens/review/components/card/ReviewEditorModal";
import { createCardFormManagedMediaState, toCardFormState } from "../../src/screens/cards/form/CardForm";
import { buildReviewButtonOptions } from "../../src/screens/review/components/reviewRatingOptions";
import { ReviewScreenHeader } from "../../src/screens/review/components/ReviewScreenHeader";
import { ReviewQueuePanel } from "../../src/screens/review/components/ReviewQueuePanel";
import type { LastSubmittedReview } from "../../src/screens/review/components/reviewScreenTypes";
import { useReviewFilterMenu } from "../../src/screens/review/filters/useReviewFilterMenu";
import { useReviewKeyboardShortcuts } from "../../src/screens/review/input/useReviewKeyboardShortcuts";
import type { ReviewSpeechSide } from "../../src/screens/review/speech/reviewSpeech";
import "../../src/styles/index.css";
import "./fixture.css";

const createdAt = "2026-10-02T09:00:00.000Z";
const parameters = new URLSearchParams(location.search);
const longContent = parameters.has("long");
const sourceContent = parameters.has("source");
const sampleCard: Card = {
  cardId: "latin-agreement",
  frontText: "Adjective agreement · which features?",
  backText: longContent
    ? Array.from({ length: 24 }, (_, index) => `### Example ${index + 1}\n\nAn adjective agrees with its noun in gender, number, and case.\n\n\`\`\`text\nbona puella\n\`\`\``).join("\n\n")
    : sourceContent
      ? "rosa\n\nrosa: rose. Vocative singular.\n\nSource:\nhttps://dcc.dickinson.edu/grammar/latin/number-and-case."
      : "Gender, number, and case.",
  cardType: "basic",
  metadata: { version: 1, source: null },
  tags: parameters.has("no-tags") ? [] : ["latin-ranieri-dowling", "latin-foundations", "latin-concept", "latin-start", ...(parameters.has("long-tag") ? ["latin-".repeat(40)] : [])],
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
  const { events, emitReaction, dismissReactions } = useReviewRatingReactions({ reviewReactionAnimationsEnabled: !parameters.has("no-animations") });
  const { pathname } = useLocation();
  const { isOpen: isChatOpen, setIsOpen: setIsChatOpen } = useChatLayout();
  const [card, setCard] = useState(sampleCard);
  const [isAnswerVisible, setIsAnswerVisible] = useState(false);
  const [isQueueOpen, setIsQueueOpen] = useState(false);
  const [selectedReviewFilter, setSelectedReviewFilter] = useState<ReviewFilter>({ kind: "allCards" });
  const [activeSpeechSide, setActiveSpeechSide] = useState<ReviewSpeechSide | null>(null);
  const [lastSubmittedReview, setLastSubmittedReview] = useState<LastSubmittedReview | null>(null);
  const [editingCard, setEditingCard] = useState<Card | null>(null);
  const [formState, setFormState] = useState(() => toCardFormState(sampleCard));
  const [isNavigationOpen, setIsNavigationOpen] = useState(false);
  const filterMenu = useReviewFilterMenu({
    deckSummaries: [],
    reviewTagSummaries: sampleCard.tags.map((tag) => ({ tag, cardsCount: 2 })),
    onSelectReviewFilter: setSelectedReviewFilter,
    selectedReviewFilter,
    workspaceId: "review-ui-workspace",
  });

  async function handleReview(reviewedCard: Card, rating: ReviewRating): Promise<void> {
    emitReaction(rating);
    setLastSubmittedReview({ cardId: reviewedCard.cardId, rating });
    setCard({ ...sampleCard, cardId: "latin-case", frontText: "What does the ablative case express?" });
    setIsAnswerVisible(false);
    setActiveSpeechSide(null);
  }

  const { handleShortcutButtonPointerEnter } = useReviewKeyboardShortcuts({
    handleReview,
    isAnswerVisible,
    isEditorPresented: editingCard !== null,
    isHardReminderVisible: false,
    isReviewFilterMenuOpen: filterMenu.isReviewFilterMenuOpen,
    isSubmitting: false,
    onShortcutInputStart: dismissReactions,
    selectedCard: card,
    setIsAnswerVisible,
  });

  return (
    <div className="app-shell">
      <header className="header-sticky">
        <div className="topbar-shell">
          <div className="topbar">
            <div className="topbar-brand-block"><AppHeaderIdentity reviewUrl="/review" /></div>
            <nav className="nav">
              {["Review", "Progress", "AI chat", "Cards", "Settings"].map((label, index) => (
                <Link key={label} className={`nav-link${index === 0 ? " nav-link-active" : ""}`} to={["/review", "/progress", "/chat", "/cards", "/settings"][index]}>{label}</Link>
              ))}
            </nav>
            <div className="topbar-actions">
              <AppHeaderActions />
              <ChatToggle />
              <button className="mobile-nav-toggle" type="button" aria-label={t("shell.primaryNavigation")} aria-expanded={isNavigationOpen} onClick={() => setIsNavigationOpen(!isNavigationOpen)}>
                <span className="mobile-nav-toggle-line" aria-hidden="true" />
                <span className="mobile-nav-toggle-line" aria-hidden="true" />
                <span className="mobile-nav-toggle-line" aria-hidden="true" />
              </button>
              <button className="account-menu-button" aria-label="Account"><UserRound size={20} /></button>
            </div>
          </div>
        </div>
        {isNavigationOpen ? <nav className="fixture-mobile-navigation" aria-label="Fixture navigation">
          {["review", "progress", "chat", "cards", "settings"].map((route) => <Link key={route} to={`/${route}`} onClick={() => setIsNavigationOpen(false)}>{route}</Link>)}
        </nav> : null}
      </header>
      {isChatOpen ? <aside className="fixture-chat" aria-label="AI chat"><strong>AI chat</strong><button type="button" onClick={() => setIsChatOpen(false)}>Close chat</button></aside> : null}
      <main className="container" onPointerDownCapture={dismissReactions}>
        {pathname === "/review" ? (
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
                  reviewReactionEvents={events}
                  activeSpeechSide={activeSpeechSide}
                  hasCards
                  isAnswerVisible={isAnswerVisible}
                  isInitialReviewLoad={parameters.has("loading")}
                  isSubmitting={false}
                  lastSubmittedReview={lastSubmittedReview}
                  localReadVersion={0}
                  loadingReviewCurrentCard={null}
                  onAiHandoff={async () => { setIsChatOpen(true); return true; }}
                  onEditCard={(selectedCard) => { setFormState(toCardFormState(selectedCard)); setEditingCard(selectedCard); }}
                  onRevealAnswer={() => setIsAnswerVisible(true)}
                  onReview={handleReview}
                  onShortcutButtonPointerEnter={handleShortcutButtonPointerEnter}
                  onSwitchToAllCards={() => {}}
                  onToggleSpeech={(side) => setActiveSpeechSide(activeSpeechSide === side ? null : side)}
                  reviewButtonErrorMessage=""
                  reviewButtonOptions={buildReviewButtonOptions(t)}
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
        ) : <section className="panel"><p>{pathname.slice(1)}</p><Link to="/review">Return to review</Link></section>}
      </main>
      <ReviewEditorModal
        editingCard={editingCard}
        editorErrorMessage=""
        formState={formState}
        isEditorPresented={editingCard !== null}
        isEditorSaving={false}
        isSubmissionBlocked={false}
        localReadVersion={0}
        managedMediaState={createCardFormManagedMediaState()}
        workspaceId={null}
        onEditWithAi={async () => {}}
        onChange={setFormState}
        onClose={() => setEditingCard(null)}
        onDelete={async () => {}}
        onPrepareImageMedia={async () => null}
        onRetryMediaUploadTransfer={async () => {}}
        onSave={async () => { setCard({ ...card, ...formState }); setEditingCard(null); }}
        tagSuggestions={[]}
      />
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
createRoot(rootElement).render(
  <I18nProvider>
    <MemoryRouter initialEntries={["/review"]}>
      <AppErrorDialogProvider>
        <ChatLayoutProvider>
          <AppHeaderProvider><ReviewUiFixture /></AppHeaderProvider>
        </ChatLayoutProvider>
      </AppErrorDialogProvider>
    </MemoryRouter>
  </I18nProvider>,
);
