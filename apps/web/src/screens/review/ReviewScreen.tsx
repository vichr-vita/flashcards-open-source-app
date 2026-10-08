import type { ReactElement } from "react";
import { useAppData } from "../../appData";
import { ReviewEditorModal } from "./components/card/ReviewEditorModal";
import { ReviewPane } from "./components/ReviewPane";
import { ReviewQueuePanel } from "./components/ReviewQueuePanel";
import { ReviewScreenHeader } from "./components/ReviewScreenHeader";
import { ReviewHardReminderDialog } from "./hardReminder/ReviewHardReminderDialog";
import { useReviewScreenController } from "./useReviewScreenController";

export { normalizeReviewMarkdownForWeb } from "./components/card/ReviewCardSide";

export function ReviewScreen(): ReactElement {
  const { session } = useAppData();
  const reviewReactionAnimationsEnabled = session?.preferences.reviewReactionAnimationsEnabled !== false;
  const {
    dismissReviewReactions,
    editorModalProps,
    hardReminderDialogProps,
    headerProps,
    paneProps,
    queuePanelProps,
    reviewReactionEvents,
  } = useReviewScreenController({
    reviewReactionAnimationsEnabled,
  });

  const reviewLayoutClassName = queuePanelProps.isReviewQueuePanelOpen
    ? "review-layout review-layout-queue-open"
    : "review-layout";

  return (
    <main className="container" data-testid="review-screen" onPointerDownCapture={dismissReviewReactions}>
      <section className="panel review-screen-panel">
        <ReviewScreenHeader {...headerProps} />

        <div className={reviewLayoutClassName}>
          <div className="review-pane-reaction-frame">
            <ReviewPane {...paneProps} reviewReactionEvents={reviewReactionEvents} />
          </div>
          {queuePanelProps.isReviewQueuePanelOpen ? <ReviewQueuePanel {...queuePanelProps} /> : null}
        </div>
      </section>

      <ReviewEditorModal {...editorModalProps} />
      <ReviewHardReminderDialog {...hardReminderDialogProps} />
    </main>
  );
}
