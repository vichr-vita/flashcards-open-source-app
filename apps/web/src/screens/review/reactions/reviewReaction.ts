import type { ReviewRating } from "../../../types";

export type ReviewReactionRating = "hard" | "good" | "easy";
export type ReviewReactionEvent = Readonly<{
  id: string;
  rating: ReviewReactionRating;
}>;

export const reviewReactionRatings: ReadonlyArray<ReviewReactionRating> = ["hard", "good", "easy"];

export const reviewReactionIntensity = {
  hard: { particleCount: 42, durationMillis: 900, rise: 1 },
  good: { particleCount: 22, durationMillis: 650, rise: 0.75 },
  easy: { particleCount: 8, durationMillis: 350, rise: 0.4 },
} as const satisfies Record<ReviewReactionRating, { particleCount: number; durationMillis: number; rise: number }>;

export function makeReviewReactionRating(rating: ReviewRating): ReviewReactionRating | null {
  return rating === 0 ? null : rating === 1 ? "hard" : rating === 2 ? "good" : "easy";
}
