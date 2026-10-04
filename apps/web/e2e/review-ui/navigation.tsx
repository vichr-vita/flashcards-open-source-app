import { createContext, useContext, useEffect, useState, type ReactElement } from "react";
import { createRoot } from "react-dom/client";
import { createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, NavLink, Navigate, routeSearchOptions, useLocation, useParams } from "../../src/routing";
import { serverQueryClient } from "../../src/api/queryClient";
import { aiSettingsQueryOptions } from "../../src/api/aiSettingsQuery";
import { changeAISettings } from "../../src/api/endpoints/aiSettings";
import { primeSessionCsrfToken } from "../../src/api/transport/transport";
import { AppErrorDialog } from "../../src/appError/AppErrorDialog";
import { I18nProvider } from "../../src/i18n";
import "../../src/styles/index.css";

const workspaceId = "11111111-1111-1111-1111-111111111111";
const prefix = `/w/${workspaceId}`;
const longCardId = "latin-" + "agreement".repeat(14);
const Account = createContext("fixture-user-1");

function SettingsReader({ name }: Readonly<{ name: string }>): ReactElement {
  const query = useQuery(aiSettingsQueryOptions(useContext(Account)));
  return <p data-testid={`reader-${name}`} role={query.error === null ? undefined : "alert"}>
    {query.error?.message ?? query.data?.connection?.email ?? "Loading"}
  </p>;
}

function SettingsPage(): ReactElement {
  const userId = useContext(Account);
  const queryClient = useQueryClient();
  useEffect(() => {
    const refresh = (): void => { void queryClient.invalidateQueries({ queryKey: ["account", userId, "ai-settings"] }); };
    window.addEventListener("ai-settings-changed", refresh);
    return () => window.removeEventListener("ai-settings-changed", refresh);
  }, [queryClient, userId]);
  return <>
    <SettingsReader name="first" /><SettingsReader name="second" />
    <button type="button" className="primary-btn" onClick={() => void changeAISettings("api")}>Use API</button>
    <button type="button" className="ghost-btn" onClick={() => void queryClient.invalidateQueries({ queryKey: ["account", userId, "ai-settings"] })}>Retry</button>
  </>;
}

function CardPage(): ReactElement {
  const { cardId } = useParams();
  return <p data-testid="card-id" style={{ overflowWrap: "anywhere" }}>{cardId}</p>;
}

function Layout(): ReactElement {
  const location = useLocation();
  const [userId, setUserId] = useState("fixture-user-1");
  const [isDialogOpen, setIsDialogOpen] = useState(false);
  return <Account value={userId}>
    <main className="container">
      <nav className="screen-actions">
        <NavLink className={({ isActive }) => isActive ? "nav-link nav-link-active" : "nav-link"} to={`${prefix}/cards/${longCardId}?tag=a%20b&tag=c#part%20one`}>Card</NavLink>
        <Link className="nav-link" to={`${prefix}/settings/ai`}>AI settings</Link>
      </nav>
      <output data-testid="address" style={{ display: "block", overflowWrap: "anywhere" }}>{location.pathname}{location.search}{location.hash}</output>
      <div className="screen-actions">
        <button className="ghost-btn" type="button" onClick={() => setUserId("fixture-user-2")}>Switch account</button>
        <button className="ghost-btn" type="button" onClick={() => setIsDialogOpen(true)}>Open error</button>
      </div>
      <Outlet />
    </main>
    <AppErrorDialog
      presentation={isDialogOpen ? {
        kind: "technical-error", title: "Could not sync", message: "Try again.",
        technicalDetails: "GET /sync/pull " + "request-".repeat(60), action: { kind: "dismiss", label: "Close" },
      } : null}
      onAction={() => setIsDialogOpen(false)} onDismiss={() => setIsDialogOpen(false)}
    />
  </Account>;
}

const rootRoute = createRootRoute({ component: Layout });
const router = createRouter({
  ...routeSearchOptions,
  routeTree: rootRoute.addChildren([
    createRoute({ getParentRoute: () => rootRoute, path: "/navigation.html", component: () => <Navigate replace to={`${prefix}/cards/initial?tag=a%20b&tag=c#part%20one`} /> }),
    createRoute({ getParentRoute: () => rootRoute, path: "/w/$workspaceId/cards/$cardId", component: CardPage }),
    createRoute({ getParentRoute: () => rootRoute, path: "/w/$workspaceId/settings/ai", component: SettingsPage }),
  ]),
  caseSensitive: false,
});
primeSessionCsrfToken("isolated-fixture-csrf");
const root = document.getElementById("root");
if (root === null) throw new Error("Navigation fixture root is missing");
createRoot(root).render(<I18nProvider><QueryClientProvider client={serverQueryClient}><RouterProvider router={router} /></QueryClientProvider></I18nProvider>);
