import { useEffect } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useAppData } from "../../appData";
import { aiSettingsQueryOptions } from "../../api/aiSettingsQuery";
import { isAuthRedirectError } from "../../api";

/** Server selection is shared across devices. Never persist account or sign-in data in browser storage. */
export function useAISettings() {
  const { session, isSessionVerified } = useAppData();
  const userId = isSessionVerified ? session?.userId ?? null : null;
  const queryClient = useQueryClient();
  const options = aiSettingsQueryOptions(userId);
  const query = useQuery(options);
  useEffect(() => {
    if (userId === null) return;
    const refreshListener = (): void => {
      void queryClient.invalidateQueries({ queryKey: ["account", userId, "ai-settings"] });
    };
    window.addEventListener("ai-settings-changed", refreshListener);
    window.addEventListener("focus", refreshListener);
    return () => {
      window.removeEventListener("ai-settings-changed", refreshListener);
      window.removeEventListener("focus", refreshListener);
    };
  }, [queryClient, userId]);
  return {
    settings: userId === null || query.error !== null ? null : query.data ?? null,
    error: userId === null || query.error === null || isAuthRedirectError(query.error)
      ? null
      : query.error.message,
  };
}
