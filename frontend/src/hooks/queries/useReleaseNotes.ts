import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { ReleaseNotes } from "../../types";

/**
 * The newest desktop release's notes for the Update page (#1916), or null when there are none
 * (offline, or a release without a changelog entry). One request each time the page opens: it is
 * never retried, and neither window focus nor a reconnect asks again.
 */
export function useLatestReleaseNotes() {
  return useQuery({
    queryKey: ["latestReleaseNotes"] as const,
    queryFn: async (): Promise<ReleaseNotes | null> => {
      const latest = await invoke<ReleaseNotes | null>("get_latest_release_notes");
      return latest && latest.notes ? latest : null;
    },
    staleTime: 0,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
}
