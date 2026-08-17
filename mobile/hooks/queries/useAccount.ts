// Account-deletion hook for the Delete Account screen. Backed by
// `delete_account` (server-side wipe via the DS + local cleanup) and
// `wipe_local_data` (belt-and-suspenders local wipe).
//
// Failure semantics mirror `pollis-core/src/commands/auth.rs`:
//   - `delete_account` errors out BEFORE any local cleanup if the remote
//     (DS) delete fails — the user stays signed in and we surface the error.
//   - Once the remote delete succeeds, core's local cleanup is best-effort;
//     the follow-up `wipe_local_data` here is too. A local-wipe failure must
//     NOT strand the user signed-in to a deleted account, so it is logged
//     and swallowed — the caller always proceeds to sign-out.

import { useMutation } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

export function useDeleteAccount() {
  return useMutation({
    mutationFn: async (userId: string) => {
      // Remote + primary local wipe. Throws (and aborts) on server failure.
      await invoke("delete_account", { userId });
      try {
        // Sweep any remaining local state (other-account remnants, data dir).
        await invoke("wipe_local_data");
      } catch (e) {
        console.warn("[account] wipe_local_data after delete failed (ignored):", e);
      }
    },
  });
}
