import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type {
  BuildVerifyReport,
  PeerAuditReport,
  SelfAuditReport,
} from "../../types";

// React Query keys for the account-key transparency audits (issue #330).
export const transparencyQueryKeys = {
  selfAudit: (userId: string | null) => ["selfAuditAccountKey", userId] as const,
  peerAudit: (peerUserId: string) => ["peerAuditAccountKey", peerUserId] as const,
};

// Query: audit the current user's own published account key against the
// transparency log. Advisory only — never blocks anything.
export function useSelfAuditAccountKey() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: transparencyQueryKeys.selfAudit(currentUser?.id ?? null),
    queryFn: async (): Promise<SelfAuditReport | null> => {
      if (!currentUser) {
        return null;
      }
      return await invoke<SelfAuditReport>("self_audit_account_key", {
        myUserId: currentUser.id,
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60 * 5,
    refetchOnWindowFocus: false,
  });
}

// Query: audit a peer's published account key against the transparency log,
// comparing it to the locally pinned (TOFU) key. Advisory only.
export function usePeerAuditAccountKey(peerUserId: string) {
  return useQuery({
    queryKey: transparencyQueryKeys.peerAudit(peerUserId),
    queryFn: async (): Promise<PeerAuditReport> => {
      return await invoke<PeerAuditReport>("audit_peer_account_key", {
        peerUserId,
      });
    },
    enabled: !!peerUserId,
    staleTime: 1000 * 60 * 5,
    refetchOnWindowFocus: false,
  });
}

// On-demand "verify this build" (issue #484). A mutation, NOT a query, so it
// only runs when the user clicks the button — never on page mount, respecting
// the zero-burden + perf constraint (it hashes the local binary and fetches the
// binaries tree). Advisory only; it never blocks anything.
export function useVerifyOwnBuild() {
  return useMutation({
    mutationFn: async (): Promise<BuildVerifyReport> => {
      return await invoke<BuildVerifyReport>("verify_own_build");
    },
  });
}
