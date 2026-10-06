import type { ReactElement } from "react";
import { Sparkles } from "lucide-react";
import { useLocation } from "react-router";
import { useI18n } from "../../i18n";
import { chatRoute, normalizeRoutePath, splitWorkspaceRoutePath } from "../../routes";
import { useChatLayout } from "./ChatLayoutContext";

export function ChatToggle(): ReactElement {
  const { isOpen, setIsOpen } = useChatLayout();
  const { pathname } = useLocation();
  const { t } = useI18n();
  const isFullscreenChat = normalizeRoutePath(splitWorkspaceRoutePath(pathname).appPath) === chatRoute;

  return (
    <button
      type="button"
      className="topbar-icon-button topbar-chat-toggle"
      aria-label={t("navigation.aiChat")}
      title={t("navigation.aiChat")}
      aria-expanded={isFullscreenChat || isOpen}
      disabled={isFullscreenChat}
      onClick={() => setIsOpen(!isOpen)}
    >
      <Sparkles size={22} aria-hidden="true" />
    </button>
  );
}
