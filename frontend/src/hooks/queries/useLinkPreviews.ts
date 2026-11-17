import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";

// "Generate link previews" (#1987): this device's own sends only; received previews always render.
const linkPreviewsEnabledKey = ["link_previews_enabled"] as const;

/** On by default; pollis-core stores it per device and refuses to fetch while it is off. */
export function useLinkPreviewsEnabled(): boolean {
  const query = useQuery({
    queryKey: linkPreviewsEnabledKey,
    queryFn: async (): Promise<boolean> =>
      (await invoke<boolean | null>("get_link_previews_enabled")) !== false,
    staleTime: 1000 * 60 * 5,
  });
  return query.data ?? true;
}

/** Optimistic: the switch moves on the same frame and rolls back if the write fails. */
export function useSetLinkPreviewsEnabled() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (enabled: boolean): Promise<void> => {
      await invoke("set_link_previews_enabled", { enabled });
    },
    onMutate: async (enabled) => {
      await queryClient.cancelQueries({ queryKey: linkPreviewsEnabledKey });
      const previous = queryClient.getQueryData<boolean>(linkPreviewsEnabledKey);
      queryClient.setQueryData(linkPreviewsEnabledKey, enabled);
      return { previous };
    },
    onError: (_err, _enabled, context) => {
      queryClient.setQueryData(linkPreviewsEnabledKey, context?.previous ?? true);
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: linkPreviewsEnabledKey });
    },
  });
}
