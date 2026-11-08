import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useRouter } from "@tanstack/react-router";
import {
  resumeTransfers,
  usePendingHistoryTransferRequests,
} from "./queries/useHistoryTransfers";

// Requests this window already put in front of the user, so a refetch never re-opens one they left.
const shown = new Set<string>();

// The link page answers the request of the device it just linked inline.
const ANSWERS_INLINE = "/security/link-device";

/**
 * #1735, mounted with the unlocked app: resume every unfinished history transfer (the app just unlocked),
 * and open the approval page for a sibling's request, checked on landing and on each realtime nudge.
 */
export function useHistoryTransferRuntime(): void {
  const queryClient = useQueryClient();
  const router = useRouter();
  const pending = usePendingHistoryTransferRequests();

  useEffect(() => {
    void resumeTransfers(queryClient);
  }, [queryClient]);

  useEffect(() => {
    const requests = pending.data ?? [];
    const pathname = router.state.location.pathname;
    for (const request of requests) {
      if (shown.has(request.transfer_id)) {
        continue;
      }
      shown.add(request.transfer_id);
      if (pathname === ANSWERS_INLINE) {
        continue;
      }
      // A push, so Back returns to exactly where the user was.
      void router.navigate({
        to: "/security/history-transfer/$transferId",
        params: { transferId: request.transfer_id },
      });
      return;
    }
  }, [pending.data, router]);
}
