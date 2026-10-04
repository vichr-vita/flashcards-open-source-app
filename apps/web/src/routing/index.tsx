import {
  Link as RouterLink,
  useRouter,
  useRouterState,
} from "@tanstack/react-router";
import { useCallback, useEffect, type ComponentProps, type ReactElement } from "react";

/** The public URL contract still uses colon parameters in shared route constants. */
export function toTanStackPath(path: string): string {
  return path.replace(/:([A-Za-z][A-Za-z0-9_]*)/gu, "$$$1");
}

// Existing bookmarks may carry repeated parameters and signed/encoded values. Preserve their
// bytes instead of applying TanStack's default JSON query-string normalization.
export const routeSearchOptions = {
  parseSearch: (search: string): Record<string, unknown> => ({ __urlSearch: search }),
  stringifySearch: (search: Record<string, unknown>): string => (
    typeof search.__urlSearch === "string" ? search.__urlSearch : ""
  ),
};

export function useLocation() {
  const router = useRouter();
  return useRouterState({
    select: ({ location }) => ({
      pathname: router.history.location.pathname,
      search: location.searchStr,
      hash: router.history.location.hash,
      key: location.state.__TSR_key ?? location.href,
    }),
  });
}

export function useParams(): Readonly<Record<string, string | undefined>> {
  return useRouterState({
    select: ({ matches }): Readonly<Record<string, string | undefined>> => matches.at(-1)?.params ?? {},
  });
}

type NavigationOptions = Readonly<{ replace?: boolean }>;

/** Accept established full URLs, including query strings and fragment targets. */
export function useNavigate() {
  const router = useRouter();
  return useCallback((to: string, options: NavigationOptions = {}): void => {
    void router.navigate({ href: to, replace: options.replace });
  }, [router]);
}

type LinkProps = Omit<ComponentProps<"a">, "href"> & Readonly<{
  to: string;
  replace?: boolean;
}>;

export function Link({ to, ...props }: LinkProps): ReactElement {
  const hashStart = to.indexOf("#");
  const withoutHash = hashStart === -1 ? to : to.slice(0, hashStart);
  const searchStart = withoutHash.indexOf("?");
  const pathname = searchStart === -1 ? withoutHash : withoutHash.slice(0, searchStart);
  return <RouterLink
    {...props}
    to={pathname}
    search={routeSearchOptions.parseSearch(searchStart === -1 ? "" : withoutHash.slice(searchStart))}
    hash={hashStart === -1 ? "" : to.slice(hashStart + 1)}
    activeProps={{}}
  />;
}

type NavLinkProps = Omit<LinkProps, "className"> & Readonly<{
  className: string | ((state: Readonly<{ isActive: boolean }>) => string);
}>;

export function NavLink({ className, to, ...props }: NavLinkProps): ReactElement {
  const location = useLocation();
  const pathname = to.split(/[?#]/u, 1)[0] ?? to;
  const isActive = location.pathname.toLowerCase() === pathname.toLowerCase()
    || location.pathname.toLowerCase().startsWith(`${pathname.toLowerCase()}/`);
  return <Link
    {...props}
    to={to}
    aria-current={isActive ? "page" : undefined}
    className={typeof className === "function" ? className({ isActive }) : className}
  />;
}

export function Navigate({ to, replace = false }: Readonly<{ to: string; replace?: boolean }>): null {
  const navigate = useNavigate();
  useEffect(() => { navigate(to, { replace }); }, [navigate, replace, to]);
  return null;
}
