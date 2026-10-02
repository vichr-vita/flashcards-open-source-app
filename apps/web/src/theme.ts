import { useSyncExternalStore } from "react";

export type ThemePreference = "system" | "light" | "dark";

const storageKey = "flashcards-web-theme-preference";
const systemDarkQuery = "(prefers-color-scheme: dark)";
const subscribers = new Set<() => void>();

function readStoredPreference(): ThemePreference {
  try {
    const stored = window.localStorage.getItem(storageKey);
    return stored === "light" || stored === "dark" ? stored : "system";
  } catch {
    return "system";
  }
}

let preference: ThemePreference = "system";
let initialized = false;

function applyTheme(): void {
  const isDark = preference === "dark"
    || (preference === "system" && window.matchMedia?.(systemDarkQuery).matches === true);
  const theme = isDark ? "dark" : "light";
  document.documentElement.dataset.theme = theme;
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", isDark ? "#000000" : "#f5f5f7");
}

function notifySubscribers(): void {
  for (const subscriber of subscribers) {
    subscriber();
  }
}

/** Apply the stored choice before React mounts, then follow OS and other-tab changes. */
export function initializeTheme(): void {
  if (initialized) {
    return;
  }
  initialized = true;
  preference = readStoredPreference();
  applyTheme();
  window.matchMedia?.(systemDarkQuery).addEventListener("change", applyTheme);
  window.addEventListener("storage", (event) => {
    if (event.key !== storageKey && event.key !== null) {
      return;
    }
    preference = readStoredPreference();
    applyTheme();
    notifySubscribers();
  });
}

export function setThemePreference(next: ThemePreference): void {
  preference = next;
  try {
    if (next === "system") {
      window.localStorage.removeItem(storageKey);
    } else {
      window.localStorage.setItem(storageKey, next);
    }
  } catch {
    // A storage failure does not prevent changing the current page's theme.
  }
  applyTheme();
  notifySubscribers();
}

function subscribe(subscriber: () => void): () => void {
  subscribers.add(subscriber);
  return () => subscribers.delete(subscriber);
}

export function useThemePreference(): ThemePreference {
  return useSyncExternalStore(subscribe, () => preference, () => "system");
}
