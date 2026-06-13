import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type { PeerAuditReport, SelfAuditReport } from "../../types";

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
