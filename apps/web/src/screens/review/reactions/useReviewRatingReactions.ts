import { useCallback, useEffect, useRef, useState } from "react";
import type { ReviewRating } from "../../../types";
import { makeReviewReactionRating, reviewReactionIntensity, type ReviewReactionEvent } from "./reviewReaction";

export type UseReviewRatingReactionsParams = Readonly<{
  reviewReactionAnimationsEnabled: boolean;
}>;

export type UseReviewRatingReactionsResult = Readonly<{
  dismissReactions: () => void;
  emitReaction: (rating: ReviewRating) => void;
  events: ReadonlyArray<ReviewReactionEvent>;
}>;

/** One short burst at a time. Review input interrupts it without delaying the next action. */
export function useReviewRatingReactions({ reviewReactionAnimationsEnabled }: UseReviewRatingReactionsParams): UseReviewRatingReactionsResult {
  const [events, setEvents] = useState<ReadonlyArray<ReviewReactionEvent>>([]);
  const cleanupTimerRef = useRef<ReturnType<typeof window.setTimeout> | null>(null);

  const clearCleanupTimer = useCallback((): void => {
    if (cleanupTimerRef.current !== null) {
      window.clearTimeout(cleanupTimerRef.current);
      cleanupTimerRef.current = null;
    }
  }, []);

  const dismissReactions = useCallback((): void => {
    clearCleanupTimer();
    setEvents((current) => current.length === 0 ? current : []);
  }, [clearCleanupTimer]);

  useEffect(() => clearCleanupTimer, [clearCleanupTimer]);
  useEffect(() => {
    if (!reviewReactionAnimationsEnabled) dismissReactions();
  }, [dismissReactions, reviewReactionAnimationsEnabled]);

  const emitReaction = useCallback((rating: ReviewRating): void => {
    clearCleanupTimer();
    const reactionRating = makeReviewReactionRating(rating);
    if (!reviewReactionAnimationsEnabled || reactionRating === null) {
      setEvents([]);
      return;
    }

    const event: ReviewReactionEvent = { id: crypto.randomUUID(), rating: reactionRating };
    setEvents([event]);
    const reducedMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    cleanupTimerRef.current = window.setTimeout(() => {
      cleanupTimerRef.current = null;
      setEvents([]);
    }, (reducedMotion ? 250 : reviewReactionIntensity[reactionRating].durationMillis) + 50);
  }, [clearCleanupTimer, reviewReactionAnimationsEnabled]);

  return { dismissReactions, emitReaction, events };
}
