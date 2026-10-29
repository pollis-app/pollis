import { useMutation, useQuery } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type {
  BuildVerifyReport,
  PeerAuditReport,
  SelfAuditReport,
} from "../../types";

export const transparencyQueryKeys = {
  selfAudit: (userId: string | null) => ["selfAuditAccountKey", userId] as const,
  peerAudit: (peerUserId: string) => ["peerAuditAccountKey", peerUserId] as const,
};

// Audit the user's own published account key against the transparency log. Advisory.
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

// Audit a peer's published key against the log and the pinned (TOFU) key. Advisory.
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

// "Verify this build" (#484): a mutation so it runs only on click (it hashes
// the binary and fetches the binaries tree). Advisory.
export function useVerifyOwnBuild() {
  return useMutation({
    mutationFn: async (): Promise<BuildVerifyReport> => {
      return await invoke<BuildVerifyReport>("verify_own_build");
    },
  });
}
