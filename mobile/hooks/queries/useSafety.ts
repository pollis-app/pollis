// Safety numbers are only ever peer-vs-self pairs (#1563), as on desktop;
// verification is pinned locally.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

// Field names must match pollis-core `SafetyNumberInfo` (serde, snake_case).
export interface SafetyNumberInfo {
  /** 60 decimal digits, grouped into 5-digit blocks for display. */
  safety_number: string;
  /** Verification status pinned locally. */
  status: "unverified" | "verified" | "changed";
  peer_identity_version: number;
  /** Both pubkeys joined canonically; what a QR code would encode. */
  qr_payload: string;
}

// Matches pollis-core `PeerVerificationEntry`.
export interface PeerVerification {
  peer_user_id: string;
  verified: boolean;
  key_changed: boolean;
}

export const safetyQueryKeys = {
  number: (myId: string | null, peerId: string | null) =>
    ["safety", "number", myId, peerId] as const,
  list: (myId: string | null) => ["safety", "list", myId] as const,
};

export function useSafetyNumber(peerUserId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: safetyQueryKeys.number(currentUser?.id ?? null, peerUserId),
    queryFn: async (): Promise<SafetyNumberInfo | null> => {
      if (!currentUser || !peerUserId) {
        return null;
      }
      return await invoke<SafetyNumberInfo>("get_safety_number", {
        myUserId: currentUser.id,
        peerUserId,
      });
    },
    enabled: !!(currentUser && peerUserId),
    staleTime: 1000 * 60,
  });
}

export function useSetContactVerified() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    meta: { haptic: {
      success: (vars: { verified: boolean }) => (vars.verified ? "success" : "toggle"),
    } },
    mutationFn: async (vars: { peerUserId: string; verified: boolean }) => {
      await invoke("set_contact_verified", {
        peerUserId: vars.peerUserId,
        verified: vars.verified,
      });
      return vars;
    },
    onSuccess: (vars) => {
      queryClient.invalidateQueries({
        queryKey: safetyQueryKeys.number(currentUser?.id ?? null, vars.peerUserId),
      });
      queryClient.invalidateQueries({
        queryKey: safetyQueryKeys.list(currentUser?.id ?? null),
      });
    },
  });
}

export function usePeerVerifications() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: safetyQueryKeys.list(currentUser?.id ?? null),
    queryFn: async (): Promise<PeerVerification[]> => {
      if (!currentUser) {
        return [];
      }
      return await invoke<PeerVerification[]>("list_peer_verifications");
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}
