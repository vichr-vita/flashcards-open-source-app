import type { ReviewRating } from "../../../../../backend/src/scheduling";

export const reviewRevealShortcutKey = " ";

export const reviewRatingShortcutKeys: Readonly<Record<ReviewRating, string>> = {
  0: "4",
  1: "3",
  2: "2",
  3: "1",
};

export const reviewShortcutRatingsByKey: Readonly<Record<string, ReviewRating>> = {
  [reviewRatingShortcutKeys[0]]: 0,
  [reviewRatingShortcutKeys[1]]: 1,
  [reviewRatingShortcutKeys[2]]: 2,
  [reviewRatingShortcutKeys[3]]: 3,
};
