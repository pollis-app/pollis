// Preferences blob shared with desktop: always merge before writing, so desktop keys survive. Mobile-only keys use `mobile_`.

import { useCallback } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

export interface MobilePreferencesPatch {
  accent_hex?: string;
  mobile_behavior?: Record<string, boolean>;
  /** Desktop-shared on purpose: the core's receipt chokepoints read exactly this key (default true). */
  send_read_receipts?: boolean;
}

/** Desktop keys ride along as `unknown` so a merge never drops them. */
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
    // Malformed JSON: treat as empty.
  }
  return {};
}

export function usePreferences() {
  const currentUser = useObserver(() => appStore.currentUser);
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

/** Reciprocal by design; missing reads as true, like the Rust default. */
export function useSendReadReceipts(): boolean {
  const { data } = usePreferences();
  const value = data?.send_read_receipts;
  return typeof value === "boolean" ? value : true;
}
