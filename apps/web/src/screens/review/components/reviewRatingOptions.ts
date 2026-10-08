import type { useI18n } from "../../../i18n";
import type { ReviewRating } from "../../../../../backend/src/scheduling";

type ReviewRatingTestId = "again" | "good" | "hard" | "easy";
type Translate = ReturnType<typeof useI18n>["t"];

export type ReviewButtonOption = Readonly<{
  rating: ReviewRating;
  testId: ReviewRatingTestId;
  title: string;
}>;

// Display ratings in increasing difficulty, matching keyboard shortcuts 1 through 4.
export function buildReviewButtonOptions(t: Translate): Array<ReviewButtonOption> {
  return [
    { rating: 3, testId: "easy", title: t("reviewScreen.ratings.easy") },
    { rating: 2, testId: "good", title: t("reviewScreen.ratings.good") },
    { rating: 1, testId: "hard", title: t("reviewScreen.ratings.hard") },
    { rating: 0, testId: "again", title: t("reviewScreen.ratings.again") },
  ];
}
