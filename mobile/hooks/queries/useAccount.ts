// `delete_account` fails before any local cleanup if the remote delete fails.
// After it succeeds, local wiping is best-effort: a failure must not strand
// the user signed in to a deleted account, so it is logged and swallowed.

import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

export function useDeleteAccount() {
  return useMutation({
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (userId: string) => {
      await invoke("delete_account", { userId });
      try {
        await invoke("wipe_local_data");
      } catch (e) {
        console.warn("[account] wipe_local_data after delete failed (ignored):", e);
      }
    },
  });
}
