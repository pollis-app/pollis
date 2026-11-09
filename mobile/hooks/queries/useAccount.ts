// Account deletion. A remote failure aborts before any local cleanup; after it, a local-wipe failure is swallowed so the user is never stranded signed in.

import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { forgetDeviceKey } from "../../lib/deviceUnlock";

export function useDeleteAccount() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (userId: string) => {
      await invoke("delete_account", { userId });
      // Core dropped the biometric slots; drop the key that unwrapped them too (#1685).
      await forgetDeviceKey(userId);
      try {
        await invoke("wipe_local_data");
      } catch (e) {
        console.warn("[account] wipe_local_data after delete failed (ignored):", e);
      }
    },
  });
}
