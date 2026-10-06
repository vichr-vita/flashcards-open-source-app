import { createContext, useContext, useState, type ReactElement, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Link, useLocation } from "react-router";
import { Brand } from "./Brand";
import { useI18n, type TranslationKey } from "./i18n";
import { cardsRoute, chatRoute, normalizeRoutePath, progressRoute, reviewRoute, splitWorkspaceRoutePath } from "./routes";

type HeaderActionsContextValue = Readonly<{
  element: HTMLDivElement | null;
  setElement: (element: HTMLDivElement | null) => void;
}>;

const HeaderActionsContext = createContext<HeaderActionsContextValue | null>(null);

/** Screens can put their own controls in the header without lifting their card or editor state. */
export function AppHeaderProvider({ children }: Readonly<{ children: ReactNode }>): ReactElement {
  const [element, setElement] = useState<HTMLDivElement | null>(null);
  return <HeaderActionsContext.Provider value={{ element, setElement }}>{children}</HeaderActionsContext.Provider>;
}

export function AppHeaderActions(): ReactElement {
  const context = useContext(HeaderActionsContext);
  return <div className="topbar-screen-actions" ref={context?.setElement} data-review-shortcuts-blocked />;
}

export function AppHeaderAction({ children }: Readonly<{ children: ReactNode }>): ReactNode {
  const context = useContext(HeaderActionsContext);
  // Standalone screen consumers still expose the action when there is no app shell.
  if (context === null) return children;
  return context.element === null ? null : createPortal(children, context.element);
}

function screenTitleKey(appPath: string): TranslationKey {
  if (appPath === "/" || appPath === reviewRoute) return "navigation.review";
  if (appPath === cardsRoute || appPath.startsWith(`${cardsRoute}/`)) return "navigation.cards";
  if (appPath === progressRoute || appPath.startsWith(`${progressRoute}/`)) return "navigation.progress";
  if (appPath === chatRoute) return "navigation.aiChat";
  return "navigation.settings";
}

export function AppHeaderIdentity({ reviewUrl }: Readonly<{ reviewUrl: string }>): ReactElement {
  const { pathname } = useLocation();
  const { t } = useI18n();
  const { appPath } = splitWorkspaceRoutePath(pathname);

  return (
    <div className="topbar-identity">
      <Link className="topbar-brand" to={reviewUrl} aria-label="lingvichr">
        <Brand />
      </Link>
      <span className="topbar-screen-title" data-testid="topbar-screen-title">{t(screenTitleKey(normalizeRoutePath(appPath)))}</span>
    </div>
  );
}
