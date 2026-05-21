// Mobile React Query client. Mirrors the desktop client's defaults in
// frontend/src/main.tsx so hooks ported across produce equivalent caching
// behavior on both platforms. Tweak per-query options at the hook level
// (staleTime, refetchOnReconnect, etc.) the same way desktop hooks do.

import { QueryClient } from "@tanstack/react-query";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 1000 * 60,
      gcTime: 1000 * 60 * 5,
      // RN has no "window focus" event in the browser sense, but
      // react-query polyfills it via AppState (foreground/background).
      // Match desktop and rely on realtime push + explicit invalidations
      // instead of focus-driven refetch storms.
      refetchOnWindowFocus: false,
      refetchOnReconnect: true,
      retry: 1,
    },
  },
});
