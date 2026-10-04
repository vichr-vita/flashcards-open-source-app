import { QueryClient } from "@tanstack/react-query";

/** Server reads live in memory. IndexedDB and the outbox remain the durable offline store. */
export const serverQueryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: false,
      networkMode: "always",
      staleTime: 0,
      refetchOnWindowFocus: true,
    },
    mutations: { retry: false, networkMode: "always" },
  },
});
