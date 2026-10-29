// Mobile React Query client, mirroring desktop's defaults (frontend/src/main.tsx).

import { MutationCache, QueryClient } from "@tanstack/react-query";
import { resolveHaptic, type MutationHaptic } from "./hapticEvents";
import { haptic } from "./haptics";

function hapticOf(meta: Record<string, unknown> | undefined): MutationHaptic | undefined {
  return meta?.haptic as MutationHaptic | undefined;
}

// Mutations tagged `meta: { haptic }` play their feedback here (#1286).
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
      // Rely on realtime + explicit invalidations, as desktop does.
      refetchOnWindowFocus: false,
      refetchOnReconnect: true,
      retry: 1,
    },
  },
});
