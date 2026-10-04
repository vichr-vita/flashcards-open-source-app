import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Children, createContext, isValidElement, useContext, useState, type ReactElement, type ReactNode } from "react";
import { routeSearchOptions, toTanStackPath } from "./index";

export { Link, Navigate, NavLink, useLocation, useNavigate, useParams } from "./index";

type RouteProps = Readonly<{ path: string; element: ReactElement }>;
const FixtureContent = createContext<ReactNode>(null);

// These declarations only adapt existing component fixtures to native TanStack route trees.
export function Route(_props: RouteProps): null { return null; }
export function Routes(): ReactElement { return <Outlet />; }

function readRoutes(children: ReactNode): ReadonlyArray<RouteProps> {
  return Children.toArray(children).flatMap((child) => {
    if (!isValidElement<{ children?: ReactNode }>(child)) return [];
    if (child.type === Route) return [child.props as RouteProps];
    return readRoutes(child.props.children);
  });
}

export function MemoryRouter({
  children,
  initialEntries = ["/"],
  initialIndex,
}: Readonly<{ children: ReactNode; initialEntries?: Array<string>; initialIndex?: number }>): ReactElement {
  const [queryClient] = useState(() => new QueryClient({ defaultOptions: { queries: { retry: false } } }));
  const [router] = useState(() => {
    const root = createRootRoute({ component: () => useContext(FixtureContent) });
    const declarations = readRoutes(children);
    const routes = declarations.length === 0
      ? [createRoute({ getParentRoute: () => root, path: "$", component: () => null })]
      : declarations.map(({ path }) => createRoute({
        getParentRoute: () => root,
        path: toTanStackPath(path),
        component: () => readRoutes(useContext(FixtureContent)).find((route) => route.path === path)?.element,
        remountDeps: ({ params }) => params,
      }));
    return createRouter({
      ...routeSearchOptions,
      routeTree: root.addChildren(routes),
      history: createMemoryHistory({ initialEntries, initialIndex }),
      caseSensitive: false,
      defaultPendingMs: 0,
      defaultPendingMinMs: 0,
    });
  });
  return <QueryClientProvider client={queryClient}>
    <FixtureContent value={children}><RouterProvider router={router} /></FixtureContent>
  </QueryClientProvider>;
}
