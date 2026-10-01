import { useEffect, useState } from "react";
import { useAppData } from "../../appData";
import { loadAISettings, type AISettings } from "../../api/endpoints/aiSettings";
import { isAuthRedirectError } from "../../api";

/** Server selection is shared across devices. Never persist account or sign-in data in browser storage. */
export function useAISettings() {
  const { session, isSessionVerified } = useAppData();
  const userId = isSessionVerified ? session?.userId ?? null : null;
  const [result, setResult] = useState<{ userId: string; settings: AISettings | null; error: string | null } | null>(null);
  useEffect(() => {
    if (userId === null) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    let loading = false;
    let refreshRequested = false;
    async function refresh(): Promise<void> {
      if (loading) { refreshRequested = true; return; }
      if (controller.signal.aborted) return;
      loading = true;
      if (timer !== undefined) clearTimeout(timer);
      try {
        const settings = await loadAISettings(controller.signal);
        if (!controller.signal.aborted && userId !== null) {
          setResult({ userId, settings, error: null });
          if (settings?.login !== null && settings?.login !== undefined) timer = setTimeout(() => { void refresh(); }, 2000);
        }
      } catch (error) {
        if (!controller.signal.aborted && !isAuthRedirectError(error) && userId !== null) setResult({ userId, settings: null, error: error instanceof Error ? error.message : String(error) });
      } finally {
        loading = false;
        if (refreshRequested) { refreshRequested = false; void refresh(); }
      }
    }
    const refreshListener = (): void => { void refresh(); };
    window.addEventListener("ai-settings-changed", refreshListener);
    window.addEventListener("focus", refreshListener);
    void refresh();
    return () => {
      controller.abort();
      if (timer !== undefined) clearTimeout(timer);
      window.removeEventListener("ai-settings-changed", refreshListener);
      window.removeEventListener("focus", refreshListener);
    };
  }, [userId]);
  return result?.userId === userId ? result : { settings: null, error: null };
}
