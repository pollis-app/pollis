// "Generate link previews" (#1987): device-local, stored by pollis-core. Governs this device's own
// sends only; received previews always render.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";

export const linkPreviewsQueryKeys = {
  user: (userId: string | null) => ["linkPreviewsEnabled", userId] as const,
};

/** On by default; the core also refuses to fetch while it is off. */
export function useLinkPreviewsEnabled() {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const queryClient = useQueryClient();
  const key = linkPreviewsQueryKeys.user(userId);

  const query = useQuery({
    queryKey: key,
    queryFn: async (): Promise<boolean> =>
      (await invoke<boolean | null>("get_link_previews_enabled")) !== false,
    enabled: !!userId,
  });

  // Optimistic: the switch moves on the same frame, and rolls back if the write fails.
  const set = useMutation({
    // ToggleRow already ticks on the press (#1286); a failed write still plays `error`.
    meta: { haptic: {} },
    mutationFn: async (enabled: boolean) => {
      await invoke("set_link_previews_enabled", { enabled });
      return enabled;
    },
    onMutate: async (enabled) => {
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<boolean>(key);
      queryClient.setQueryData(key, enabled);
      return { previous };
    },
    onError: (_err, _enabled, context) => {
      queryClient.setQueryData(key, context?.previous ?? true);
    },
  });

  return { enabled: query.data ?? true, set };
}
