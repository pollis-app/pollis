// Mobile preferences hook. Reads + writes the user's free-form
// preferences blob via the desktop-shared `get_preferences` /
// `save_preferences` commands. Mobile and desktop share the same Turso
// row keyed by `user_id` — to avoid clobbering desktop-side keys
// (`accent_color`, `noise_suppression_level`, etc.) we always merge the
// remote object with our mobile patch before writing back.
//
// The mobile UI persists:
//   - `accent_hex`             — color picker, also drives ThemeProvider
//   - `mobile_theme`           — Coal | Paper | System (cosmetic, not wired yet)
//   - `mobile_density`         — Compact | Comfortable
//   - `mobile_behavior`        — toggle map for the BEHAVIOR list
//
// Mobile-only keys are namespaced with `mobile_` so future desktop reads
// see them as opaque pass-through, not interpretable preferences.

import { useCallback } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { useAppStore } from "../../stores/appStore";

export interface MobilePreferencesPatch {
  accent_hex?: string;
  mobile_theme?: "Coal" | "Paper" | "System";
  mobile_density?: "Compact" | "Comfortable";
  mobile_behavior?: Record<string, boolean>;
}

/** The full blob as it lives in Turso. Mobile fields are typed; desktop
 *  keys ride along as `unknown` so we don't drop them during merge. */
export type PreferencesBlob = MobilePreferencesPatch & Record<string, unknown>;

export const preferencesQueryKeys = {
  all: ["preferences"] as const,
  user: (userId: string | null) => ["preferences", userId] as const,
};

function parseBlob(raw: string | null | undefined): PreferencesBlob {
  if (!raw) {
    return {};
  }
  try {
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as PreferencesBlob;
    }
  } catch {
    // Server returned malformed JSON — treat as empty.
  }
  return {};
}

export function usePreferences() {
  const currentUser = useAppStore((s) => s.currentUser);
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: preferencesQueryKeys.user(currentUser?.id ?? null),
    queryFn: async (): Promise<PreferencesBlob> => {
      if (!currentUser) {
        return {};
      }
      const raw = await invoke<string>("get_preferences", {
        userId: currentUser.id,
      });
      return parseBlob(raw);
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });

  const save = useMutation({
    mutationFn: async (patch: MobilePreferencesPatch) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      // Merge against the latest cached blob so desktop-side fields are
      // preserved. If the cache is empty (first save after install) we
      // start from `{}` — losing nothing.
      const current =
        queryClient.getQueryData<PreferencesBlob>(
          preferencesQueryKeys.user(currentUser.id),
        ) ?? {};
      const merged: PreferencesBlob = { ...current, ...patch };
      await invoke("save_preferences", {
        userId: currentUser.id,
        preferencesJson: JSON.stringify(merged),
      });
      return merged;
    },
    onSuccess: (merged) => {
      queryClient.setQueryData(
        preferencesQueryKeys.user(currentUser?.id ?? null),
        merged,
      );
    },
  });

  const update = useCallback(
    (patch: MobilePreferencesPatch) => {
      save.mutate(patch);
    },
    [save],
  );

  return { ...query, save, update };
}
