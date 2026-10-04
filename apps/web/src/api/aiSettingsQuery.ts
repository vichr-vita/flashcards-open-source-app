import { queryOptions } from "@tanstack/react-query";
import { loadAISettings } from "./endpoints/aiSettings";

export function aiSettingsQueryOptions(userId: string | null) {
  return queryOptions({
    queryKey: ["account", userId, "ai-settings"] as const,
    queryFn: ({ signal }) => loadAISettings(signal),
    enabled: userId !== null,
    // Never retain ChatGPT identity or a device-login code after the last observer leaves.
    gcTime: 0,
    refetchInterval: (query) => query.state.data?.login == null ? false : 2000,
  });
}
