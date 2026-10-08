import type { CSSProperties, ReactElement } from "react";
import { reviewReactionIntensity, type ReviewReactionEvent } from "./reviewReaction";

type ReviewRatingReactionLayerProps = Readonly<{
  events: ReadonlyArray<ReviewReactionEvent>;
}>;

type ReactionStyle = CSSProperties & Readonly<{ "--review-reaction-duration": string }>;

type ParticleStyle = CSSProperties & Readonly<{
  "--particle-x": string;
  "--particle-dx": string;
  "--particle-rise": string;
  "--particle-turn": string;
  "--particle-delay": string;
  "--particle-color": string;
}>;

const particleColors = ["var(--accent)", "#d8c7ff", "#ffd46b", "#fff"];
const sparkle = <svg viewBox="0 0 16 16" focusable="false"><path d="M8 0Q9 7 16 8Q9 9 8 16Q7 9 0 8Q7 7 8 0Z" /></svg>;

/** CSS moves small wrappers once. No player, asset fetching, or per-frame JavaScript. */
export function ReviewRatingReactionLayer({ events }: ReviewRatingReactionLayerProps): ReactElement {
  return (
    <div className="review-rating-reaction-layer" aria-hidden="true" data-testid="review-rating-reaction-layer">
      {events.map((event) => {
        const intensity = reviewReactionIntensity[event.rating];
        const reactionStyle: ReactionStyle = { "--review-reaction-duration": `${intensity.durationMillis}ms` };
        return (
          <div
            key={event.id}
            className="review-rating-reaction-event"
            data-review-reaction-rating={event.rating}
            data-testid="review-rating-reaction-event"
            style={reactionStyle}
          >
            {Array.from({ length: intensity.particleCount }, (_, index) => {
              const isSparkle = event.rating === "easy" || index % 5 === 0;
              const side = index % 2 === 0 ? -1 : 1;
              const spread = (index * 17 % 31) / 30;
              const style: ParticleStyle = {
                "--particle-x": `${side < 0 ? 1 + spread * 1.5 : 94 - spread * 1.5}%`,
                "--particle-dx": `${side * (1 + spread * 2)}px`,
                "--particle-rise": `${-(22 + (index * 13 % 40)) * intensity.rise}%`,
                "--particle-turn": `${side * (50 + index * 23 % 160)}deg`,
                "--particle-delay": `${index % 4 * 12}ms`,
                "--particle-color": particleColors[index % particleColors.length],
              };
              return (
                <div key={index} className={`review-reaction-particle${isSparkle ? " review-reaction-particle-sparkle" : ""}`} style={style}>
                  <span className="review-reaction-particle-mark">{isSparkle ? sparkle : null}</span>
                </div>
              );
            })}
          </div>
        );
      })}
    </div>
  );
}
