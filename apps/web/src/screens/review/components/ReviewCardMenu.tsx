import { useCallback, useEffect, useId, useRef, useState, type ReactElement } from "react";
import { Pencil, Tag } from "lucide-react";
import { AppHeaderAction } from "../../../AppHeader";
import { AnchoredFloatingOverlay, useAnchoredFloatingOutsidePointerDismiss } from "../../../floating";
import { useI18n } from "../../../i18n";
import type { Card } from "../../../types";
import { ReviewRepetitionBadgeIcon } from "../../shared/ReviewProgressBadgeIcon";
import { ReviewCardTags } from "./ReviewCardTags";

type Props = Readonly<{
  card: Card;
  onEdit: (card: Card) => void;
}>;

const menuMinimumWidth = { kind: "pixels", pixels: 240 } as const;

function CardMenu({ card, onEdit }: Props): ReactElement {
  const { t, formatNumber } = useI18n();
  const [isOpen, setIsOpen] = useState(false);
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const editRef = useRef<HTMLButtonElement | null>(null);
  const menuId = useId();
  const title = t("cardForm.fields.tags");
  const repetitionValue = card.reps === 0 ? t("reviewScreen.repetitionBadgeNew") : formatNumber(card.reps);
  const close = useCallback(() => setIsOpen(false), []);

  useAnchoredFloatingOutsidePointerDismiss({ triggerRef: buttonRef, overlayRef: menuRef, enabled: isOpen, onClose: close });

  useEffect(() => {
    if (!isOpen) return;
    editRef.current?.focus();

    function handleKeyDown(event: KeyboardEvent): void {
      if (event.key === "Escape") {
        event.preventDefault();
        setIsOpen(false);
        buttonRef.current?.focus();
      }
    }
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [isOpen]);

  return (
    <>
      <button
        ref={buttonRef}
        className="topbar-icon-button"
        type="button"
        aria-label={title}
        title={title}
        aria-expanded={isOpen}
        aria-controls={isOpen ? menuId : undefined}
        data-testid="review-card-menu-trigger"
        onClick={() => setIsOpen((value) => !value)}
      >
        <Tag size={22} aria-hidden="true" />
      </button>
      <AnchoredFloatingOverlay
        isOpen={isOpen}
        referenceRef={buttonRef}
        floatingRef={menuRef}
        placement="bottom-end"
        viewportPaddingPx={12}
        offsetPx={6}
        minimumWidth={menuMinimumWidth}
        maxWidthPx={300}
        maxHeightPx={420}
        className="review-card-menu"
        id={menuId}
        role="region"
        ariaLabel={title}
        ariaLabelledBy={null}
        ariaDescribedBy={null}
        ariaModal={null}
      >
        <div className="review-card-menu-content" data-review-shortcuts-blocked>
          <div className="review-card-menu-tags"><ReviewCardTags tags={card.tags} /></div>
          <div className="review-card-menu-repetition" aria-label={t("reviewScreen.repetitionBadgeAriaLabel", { value: repetitionValue })}>
            <ReviewRepetitionBadgeIcon /><span aria-hidden="true">{repetitionValue}</span>
          </div>
          <button
            ref={editRef}
            type="button"
            className="review-card-menu-edit"
            onClick={() => { close(); onEdit(card); }}
          >
            <Pencil size={18} aria-hidden="true" />{t("reviewScreen.actions.edit")}
          </button>
        </div>
      </AnchoredFloatingOverlay>
    </>
  );
}

export function ReviewCardMenu(props: Props): ReactElement {
  return <AppHeaderAction><CardMenu key={props.card.cardId} {...props} /></AppHeaderAction>;
}
