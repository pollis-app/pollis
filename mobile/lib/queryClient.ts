// Mobile React Query client. Mirrors the desktop client's defaults in
// frontend/src/main.tsx so hooks ported across produce equivalent caching
// behavior on both platforms. Tweak per-query options at the hook level
// (staleTime, refetchOnReconnect, etc.) the same way desktop hooks do.

import { MutationCache, QueryClient } from "@tanstack/react-query";
import { resolveHaptic, type MutationHaptic } from "./hapticEvents";
import { haptic } from "./haptics";

function hapticOf(meta: Record<string, unknown> | undefined): MutationHaptic | undefined {
  return meta?.haptic as MutationHaptic | undefined;
}

// Tactile feedback for user actions (#1286), in one place: a mutation tagged
// `meta: { haptic }` plays it here (lib/hapticEvents.ts, MutationHaptic).
const mutationCache = new MutationCache({
  onMutate: (vars, mutation) => {
    const event = resolveHaptic(hapticOf(mutation.meta)?.mutate, vars);
    if (event) {
      haptic(event);
    }
  },
  onSuccess: (_data, vars, _ctx, mutation) => {
    const event = resolveHaptic(hapticOf(mutation.meta)?.success, vars);
    if (event) {
      haptic(event);
    }
  },
  onError: (_error, _vars, _ctx, mutation) => {
    const spec = hapticOf(mutation.meta);
    if (spec && spec.error !== false) {
      haptic("error");
    }
  },
});

export const queryClient = new QueryClient({
  mutationCache,
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
