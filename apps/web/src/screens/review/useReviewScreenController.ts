import { useEffect, useRef, useState } from "react";
import type { ReviewRating } from "../../../../backend/src/scheduling";
import { track } from "../../analytics";
import { useAppData, useReviewLeaderboardBadge, useReviewProgressBadge } from "../../appData";
import {
  markIndexedDbOpenRecoveryFailureAndCheckActive,
  useAppErrorDialog,
} from "../../appError/AppErrorContext";
import { ALL_CARDS_REVIEW_FILTER, currentReviewCard } from "../../appData/domain";
import { useI18n } from "../../i18n";
import { useAiCardHandoff } from "../../chat/handoff/useAiCardHandoff";
import { useTransientMessage } from "../../useTransientMessage";
import type { Card } from "../../types";
import { handleRefreshLocalDataError } from "../shared/refreshLocalDataError";
import { isCardFormStateDirty } from "../cards/form/CardForm";
import type { ReviewEditorModalProps } from "./components/card/ReviewEditorModal";
import type { ReviewPaneProps } from "./components/ReviewPane";
import type { ReviewQueuePanelProps } from "./components/ReviewQueuePanel";
import type { ReviewScreenHeaderProps } from "./components/ReviewScreenHeader";
import { buildReviewButtonOptions, type ReviewButtonOption } from "./components/reviewRatingOptions";
import { type LastSubmittedReview, type ReviewSubmitState } from "./components/reviewScreenTypes";
import { useReviewCardEditor } from "./components/card/useReviewCardEditor";
import { useReviewScreenData, type ReviewSubmissionOutcome } from "./data/useReviewScreenData";
import { resolveReviewFilterTitle } from "./data/reviewScreenDataState";
import { useReviewFilterMenu } from "./filters/useReviewFilterMenu";
import type { ReviewHardReminderDialogProps } from "./hardReminder/ReviewHardReminderDialog";
import {
  appendRecentReviewRatings,
  loadReviewHardReminderLastShownAt,
  saveReviewHardReminderLastShownAt,
  shouldShowReviewHardReminder,
} from "./hardReminder/reviewHardReminder";
import { useReviewKeyboardShortcuts } from "./input/useReviewKeyboardShortcuts";
import { useReviewRatingReactions, type UseReviewRatingReactionsResult } from "./reactions/useReviewRatingReactions";
import { makeReviewSpeakableText, useReviewSpeech } from "./speech/reviewSpeech";

export type UseReviewScreenControllerResult = Readonly<{
  dismissReviewReactions: UseReviewRatingReactionsResult["dismissReactions"];
  editorModalProps: ReviewEditorModalProps;
  hardReminderDialogProps: ReviewHardReminderDialogProps;
  headerProps: ReviewScreenHeaderProps;
  paneProps: ReviewPaneProps;
  queuePanelProps: ReviewQueuePanelProps;
  reviewReactionEvents: UseReviewRatingReactionsResult["events"];
}>;

export type UseReviewScreenControllerParams = Readonly<{
  reviewReactionAnimationsEnabled: boolean;
}>;

export function useReviewScreenController(
  params: UseReviewScreenControllerParams,
): UseReviewScreenControllerResult {
  const { reviewReactionAnimationsEnabled } = params;
  const {
    activeWorkspace,
    cloudSettings,
    errorMessage,
    selectedReviewFilter,
    workspaceSettings,
    isSyncing,
    localReadVersion,
    getCardById,
    refreshLocalData,
    runMediaUploadTransfers,
    selectReviewFilter,
    session,
    submitReviewItem,
    updateCardItem,
    deleteCardItem,
    setErrorMessage,
  } = useAppData();
  const reviewLeaderboardBadge = useReviewLeaderboardBadge();
  const reviewProgressBadge = useReviewProgressBadge();
  const { formatCount, locale, messages, t } = useI18n();
  const { indexedDbOpenRecoveryState, showCapturedTechnicalError, showTechnicalError } = useAppErrorDialog();
  const [answerVisibleForCardId, setAnswerVisibleForCardId] = useState<string | null>(null);
  const [isSubmitting, setIsSubmitting] = useState<boolean>(false);
  const [reviewSubmitState, setReviewSubmitState] = useState<ReviewSubmitState>("idle");
  const [lastSubmittedReview, setLastSubmittedReview] = useState<LastSubmittedReview | null>(null);
  const [isHardReminderVisible, setIsHardReminderVisible] = useState<boolean>(false);
  const [isReviewQueuePanelOpen, setIsReviewQueuePanelOpen] = useState<boolean>(false);
  const [hardReminderLastShownAt, setHardReminderLastShownAt] = useState<number | null>(() => loadReviewHardReminderLastShownAt());
  const recentReviewRatingsRef = useRef<Array<ReviewRating>>([]);
  const revealedCardIdRef = useRef<string | null>(null);
  const { message: reviewSpeechMessage, showMessage: showReviewSpeechMessage } = useTransientMessage(3000);
  const {
    dismissReactions: dismissReviewReactions,
    emitReaction: emitReviewReaction,
    events: reviewReactionEvents,
  } = useReviewRatingReactions({
    reviewReactionAnimationsEnabled,
  });
  const {
    activeReviewQueue,
    deckSummaries,
    handleReview: handleReviewData,
    hasLoadedReviewData,
    isInitialReviewLoad,
    isReviewLoading,
    localWorkspaceCardCount,
    queueCards,
    resolvedReviewFilter,
    reviewCounts,
    reviewLoadErrorMessage,
    reviewLoadingSnapshot,
    reviewTagSummaries,
    selectedReviewFilterTitle,
    tagSuggestions,
  } = useReviewScreenData({
    activeWorkspaceId: activeWorkspace?.workspaceId ?? null,
    appErrorMessage: errorMessage,
    getCardById,
    installationId: cloudSettings?.installationId ?? null,
    isSyncing,
    localReadVersion,
    selectReviewFilter,
    selectedReviewFilter,
    setErrorMessage,
    submitReviewItem,
    userId: session?.userId ?? null,
  });
  const selectedCard = currentReviewCard(activeReviewQueue);
  // A replacement card must render its front immediately, before effects reset the old reveal.
  const isAnswerVisible = selectedCard !== null && selectedCard.cardId === answerVisibleForCardId;
  const {
    activeSide: activeSpeechSide,
    stopSpeech,
    toggleSpeech,
  } = useReviewSpeech({
    locale,
    showMessage: showReviewSpeechMessage,
    speechUnavailableMessage: t("reviewScreen.speechUnavailable"),
  });
  const {
    activeReviewFilterOptionId,
    activeReviewFilterOptionKey,
    getReviewFilterOptionId,
    handleCloseMenu,
    handleReviewFilterComboboxKeyDown,
    handleReviewFilterListboxKeyDown,
    handleReviewFilterMenuToggle,
    handleReviewFilterSelect,
    hasVisibleReviewFilterChoices,
    isReviewFilterMenuOpen,
    reviewDeckSearchInputRef,
    reviewDeckSearchText,
    reviewFilterListboxId,
    reviewFilterListboxRef,
    reviewFilterMenuRef,
    reviewFilterMenuItems,
    reviewFilterTriggerRef,
    setReviewDeckSearchText,
    shouldShowReviewDeckSearch,
    visibleReviewDeckFilterMenuItems,
    visibleReviewTagFilterMenuItems,
  } = useReviewFilterMenu({
    deckSummaries,
    onSelectReviewFilter: selectReviewFilter,
    reviewTagSummaries,
    selectedReviewFilter,
    workspaceId: activeWorkspace?.workspaceId ?? null,
  });
  const {
    captureEditorPresentationToken,
    editorErrorMessage,
    editingCard,
    editorFormState,
    handleEditorDelete,
    handleEditorSaveForAiHandoff,
    handleRetryMediaUploadTransfer,
    handleEditorSave,
    handlePrepareImageMedia,
    handleOpenEditor,
    handleCloseEditor,
    handleCloseEditorIfCurrent,
    isEditorPresented,
    isEditorSaving,
    isEditorSubmissionAllowed,
    isEditorSubmissionBlocked,
    managedMediaState,
    setEditorFormState,
  } = useReviewCardEditor({
    deleteCardItem,
    installationId: cloudSettings?.installationId ?? null,
    localReadVersion,
    queueCards,
    runMediaUploadTransfers,
    selectedCard,
    setErrorMessage,
    t,
    updateCardItem,
    userId: session?.userId ?? null,
    workspaceId: activeWorkspace?.workspaceId ?? null,
  });
  const handoffCardToAi = useAiCardHandoff();
  const nowTimestamp = Date.now();
  const selectedFrontSpeakableText = selectedCard === null ? "" : makeReviewSpeakableText(selectedCard.frontText);
  const selectedBackSpeakableText = selectedCard === null ? "" : makeReviewSpeakableText(selectedCard.backText);
  const hasCards = localWorkspaceCardCount > 0;
  const shouldShowSwitchToAllCardsAction = resolvedReviewFilter.kind !== "allCards";
  const loadingReviewCurrentCard = reviewLoadingSnapshot?.currentCard ?? reviewLoadingSnapshot?.queuePreview[0] ?? null;
  const requestedReviewFilterTitle = resolveReviewFilterTitle(
    selectedReviewFilter,
    deckSummaries,
    t("filters.allCards"),
    t("reviewFilterMenu.noTags"),
    formatCount(
      selectedReviewFilter.kind === "tags" ? selectedReviewFilter.tags.length : 0,
      messages.common.countLabels.tag,
    ),
  );
  const visibleSelectedReviewFilterTitle = isInitialReviewLoad && reviewLoadingSnapshot !== null
    ? reviewLoadingSnapshot.resolvedReviewFilterTitle
    : isReviewLoading
      ? requestedReviewFilterTitle
      : selectedReviewFilterTitle;
  const visibleQueueCardsCount = isInitialReviewLoad && reviewLoadingSnapshot !== null
    ? reviewLoadingSnapshot.queuePreview.length
    : queueCards.length;
  let reviewButtonOptions: Array<ReviewButtonOption> = [];
  let reviewButtonErrorMessage: string = "";

  function markIndexedDbOpenRecoveryFailure(error: unknown): boolean {
    return markIndexedDbOpenRecoveryFailureAndCheckActive(indexedDbOpenRecoveryState, error);
  }

  function handleReviewQueueShortcutClick(): void {
    setIsReviewQueuePanelOpen((currentIsReviewQueuePanelOpen) => !currentIsReviewQueuePanelOpen);
  }

  function handleReviewQueuePanelClose(): void {
    setIsReviewQueuePanelOpen(false);
  }

  async function handleReview(card: Card, rating: ReviewRating): Promise<void> {
    if (indexedDbOpenRecoveryState.hasFailed()) {
      return;
    }

    emitReviewReaction(rating);
    setIsSubmitting(true);
    setReviewSubmitState("submitting");
    setLastSubmittedReview({
      cardId: card.cardId,
      rating,
    });
    let reviewSubmissionOutcome: ReviewSubmissionOutcome = "failed";

    try {
      reviewSubmissionOutcome = await handleReviewData(card, rating);
      if (indexedDbOpenRecoveryState.hasFailed()) {
        reviewSubmissionOutcome = "cancelled";
        return;
      }
      if (reviewSubmissionOutcome !== "saved") {
        return;
      }

      const nextRecentReviewRatings = appendRecentReviewRatings(recentReviewRatingsRef.current, rating);
      recentReviewRatingsRef.current = nextRecentReviewRatings;

      if (rating === 1) {
        const nowMillis = Date.now();
        if (shouldShowReviewHardReminder(nextRecentReviewRatings, hardReminderLastShownAt, nowMillis)) {
          setHardReminderLastShownAt(nowMillis);
          saveReviewHardReminderLastShownAt(nowMillis);
          setIsHardReminderVisible(true);
        }
      }
    } finally {
      if (reviewSubmissionOutcome !== "saved") dismissReviewReactions();
      if (reviewSubmissionOutcome !== "cancelled" && indexedDbOpenRecoveryState.hasFailed() === false) {
        setIsSubmitting(false);
        if (reviewSubmissionOutcome === "stale") {
          setLastSubmittedReview(null);
          setReviewSubmitState("idle");
        } else {
          setReviewSubmitState(reviewSubmissionOutcome === "saved" ? "settled" : "failed");
        }
      }
    }
  }

  async function handleEditorAiHandoff(): Promise<void> {
    if (
      indexedDbOpenRecoveryState.hasFailed()
      || editingCard === null
      || isEditorSubmissionAllowed() === false
    ) {
      return;
    }

    const presentationToken = captureEditorPresentationToken();
    if (presentationToken === null) {
      return;
    }
    const cardForHandoff = isCardFormStateDirty(editingCard, editorFormState)
      ? await handleEditorSaveForAiHandoff()
      : editingCard;
    if (cardForHandoff === null || indexedDbOpenRecoveryState.hasFailed()) {
      return;
    }

    const didHandoff = await handleReviewPaneAiHandoff(cardForHandoff);
    if (didHandoff && indexedDbOpenRecoveryState.hasFailed() === false) {
      handleCloseEditorIfCurrent(presentationToken);
    }
  }

  async function handleReviewPaneAiHandoff(card: Card): Promise<boolean> {
    if (indexedDbOpenRecoveryState.hasFailed()) {
      return false;
    }

    try {
      indexedDbOpenRecoveryState.throwIfFailed();
      const didHandoff = await handoffCardToAi(card);
      indexedDbOpenRecoveryState.throwIfFailed();
      return didHandoff;
    } catch (error) {
      if (markIndexedDbOpenRecoveryFailure(error)) {
        return false;
      }
      throw error;
    }
  }

  function handleDismissHardReminder(): void {
    setIsHardReminderVisible(false);
  }

  /**
   * The card flip, reported once per presentation of a card.
   *
   * Reported from the two actions that reveal an answer — the button and the keyboard shortcut — and
   * never from an effect on `isAnswerVisible`. Rating a card moves `activeReviewQueue` and hides the
   * answer in the same commit, so an effect watching that flag cannot tell a flip apart from the
   * next card arriving while the flag has not been reset yet; it would count cards presented,
   * timestamp them at presentation, and credit the last card of every session as revealed. The gap
   * to the server-derived `review_answered` is exactly the population that abandons a card without
   * flipping it, so that reading would invert the fact this event exists for.
   *
   * The ref names *this* presentation rather than the last card reported: the effect that hides the
   * answer for a newly presented card clears it, keyed on the same `selectedCard?.cardId` that
   * decides what is on screen, so the two can never fall out of step. A repeat reveal inside one
   * presentation — a double click landing before the button is replaced by the rating bar — finds
   * the ref still holding that card and stays one event; every new presentation starts from a
   * cleared ref and reports again.
   *
   * That distinction is the whole point. A card rated `Again` is due one learning step later, so it
   * leaves the queue and returns as the head — with no other card in between when it is the last due
   * one, in a single-card deck, or across a filter round trip. Holding only the last card reported
   * would suppress every one of those returns while the server-derived `review_answered` counts each
   * answer, inverting the ratio this event exists to give.
   */
  function reportCardRevealed(): void {
    const presentedCardId = selectedCard?.cardId ?? null;
    if (presentedCardId === null || revealedCardIdRef.current === presentedCardId) {
      return;
    }

    revealedCardIdRef.current = presentedCardId;
    track({ name: "review_card_revealed", screen: "review" });
  }

  function handleRevealAnswer(): void {
    reportCardRevealed();
    stopSpeech();
    setAnswerVisibleForCardId(selectedCard?.cardId ?? null);
  }

  async function handleRetryReviewLoad(): Promise<void> {
    try {
      indexedDbOpenRecoveryState.throwIfFailed();
      await refreshLocalData();
      indexedDbOpenRecoveryState.throwIfFailed();
    } catch (error) {
      if (markIndexedDbOpenRecoveryFailure(error)) {
        return;
      }
      handleRefreshLocalDataError({
        error,
        context: {
          feature: "sync",
          operation: "refresh_local_metadata",
          userId: session?.userId ?? null,
          workspaceId: activeWorkspace?.workspaceId ?? null,
          installationId: cloudSettings?.installationId ?? null,
          entityId: activeWorkspace?.workspaceId ?? null,
        },
        setErrorMessage,
        showCapturedTechnicalError,
        technicalErrorMessage: t("appError.technicalError.message"),
      });
    }
  }

  function handleSwitchToAllCards(): void {
    selectReviewFilter(ALL_CARDS_REVIEW_FILTER);
  }

  useEffect(() => {
    // Clearing the reveal ref here is what makes it mean "this presentation": a new presentation
    // always changes this dep, so the guard and the card on screen move together.
    revealedCardIdRef.current = null;
    setAnswerVisibleForCardId(null);
    stopSpeech();
  }, [selectedCard?.cardId, stopSpeech]);

  useEffect(() => {
    if (indexedDbOpenRecoveryState.isFailed === false) {
      return;
    }

    stopSpeech();
    dismissReviewReactions();
  }, [dismissReviewReactions, indexedDbOpenRecoveryState.isFailed, stopSpeech]);

  useEffect(() => {
    recentReviewRatingsRef.current = [];
    setIsHardReminderVisible(false);
  }, [activeWorkspace?.workspaceId]);

  useEffect(() => {
    return () => {
      stopSpeech();
    };
  }, [stopSpeech]);

  const { handleShortcutButtonPointerEnter } = useReviewKeyboardShortcuts({
    handleReview: async (card, rating) => {
      await handleReview(card, rating);
    },
    isAnswerVisible,
    isEditorPresented,
    isHardReminderVisible,
    isReviewFilterMenuOpen,
    isSubmitting,
    onShortcutInputStart: dismissReviewReactions,
    selectedCard,
    setIsAnswerVisible: (value) => {
      if (value) {
        reportCardRevealed();
        stopSpeech();
      }
      setAnswerVisibleForCardId(value ? selectedCard?.cardId ?? null : null);
    },
  });

  if (isAnswerVisible && selectedCard !== null && workspaceSettings !== null) {
    reviewButtonOptions = buildReviewButtonOptions(t);
  } else if (isAnswerVisible && selectedCard !== null) {
    reviewButtonErrorMessage = t("reviewScreen.errors.schedulerUnavailable");
  }

  return {
    dismissReviewReactions,
    editorModalProps: {
      editingCard,
      editorErrorMessage,
      formState: editorFormState,
      isEditorPresented,
      isEditorSaving,
      isSubmissionBlocked: isEditorSubmissionBlocked,
      localReadVersion,
      managedMediaState,
      workspaceId: activeWorkspace?.workspaceId ?? null,
      onEditWithAi: handleEditorAiHandoff,
      onChange: setEditorFormState,
      onClose: handleCloseEditor,
      onDelete: handleEditorDelete,
      onPrepareImageMedia: handlePrepareImageMedia,
      onRetryMediaUploadTransfer: handleRetryMediaUploadTransfer,
      onSave: handleEditorSave,
      tagSuggestions,
    },
    hardReminderDialogProps: {
      isOpen: isHardReminderVisible,
      onDismiss: handleDismissHardReminder,
    },
    headerProps: {
      filterMenuProps: {
        activeReviewFilterOptionId,
        activeReviewFilterOptionKey,
        getReviewFilterOptionId,
        handleCloseMenu,
        handleReviewFilterComboboxKeyDown,
        handleReviewFilterListboxKeyDown,
        handleReviewFilterMenuToggle,
        handleReviewFilterSelect,
        hasVisibleReviewFilterChoices,
        isReviewFilterMenuOpen,
        reviewDeckSearchInputRef,
        reviewDeckSearchText,
        reviewFilterListboxId,
        reviewFilterListboxRef,
        reviewFilterMenuRef,
        reviewFilterMenuItems,
        reviewFilterTriggerRef,
        selectedReviewFilterTitle: visibleSelectedReviewFilterTitle,
        setReviewDeckSearchText,
        shouldShowReviewDeckSearch,
        visibleReviewDeckFilterMenuItems,
        visibleReviewTagFilterMenuItems,
      },
      hasLoadedReviewData,
      isReviewQueuePanelOpen,
      onRetry: handleRetryReviewLoad,
      onReviewQueueShortcutClick: handleReviewQueueShortcutClick,
      reviewQueueTotalCount: isInitialReviewLoad && reviewLoadingSnapshot !== null
        ? reviewLoadingSnapshot.reviewCounts.totalCount
        : reviewCounts.totalCount,
      reviewLoadErrorMessage,
      reviewLeaderboardBadge,
      reviewProgressBadge,
      reviewSpeechMessage,
    },
    paneProps: {
      activeSpeechSide,
      hasCards,
      isAnswerVisible,
      isInitialReviewLoad,
      isSubmitting,
      lastSubmittedReview,
      localReadVersion,
      loadingReviewCurrentCard,
      onAiHandoff: handleReviewPaneAiHandoff,
      onEditCard: handleOpenEditor,
      onRevealAnswer: handleRevealAnswer,
      onReview: handleReview,
      onShortcutButtonPointerEnter: handleShortcutButtonPointerEnter,
      onSwitchToAllCards: handleSwitchToAllCards,
      onToggleSpeech: toggleSpeech,
      reviewButtonErrorMessage,
      reviewButtonOptions,
      reviewLoadingSnapshot,
      reviewSubmitState,
      selectedBackSpeakableText,
      selectedCard,
      selectedFrontSpeakableText,
      shouldShowSwitchToAllCardsAction,
      workspaceId: activeWorkspace?.workspaceId ?? null,
    },
    queuePanelProps: {
      isInitialReviewLoad,
      isReviewQueuePanelOpen,
      loadingReviewCurrentCard,
      nowTimestamp,
      onClose: handleReviewQueuePanelClose,
      queueCards,
      reviewLoadingSnapshot,
      selectedCardId: selectedCard?.cardId ?? null,
      visibleQueueCardsCount,
    },
    reviewReactionEvents,
  };
}
