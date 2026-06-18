// Safety-number hooks — used by the other-user profile screen to show
// the 60-digit-ish fingerprint and a "Verified" toggle. Mirrors the
// desktop verification pattern: we never display the *own* user's
// safety number, only peer-vs-self pairs.
//
// `get_safety_number` returns an info struct with both fingerprints and
// the current verification state from the local pin store. Marking a
// peer verified writes a row to the local `peer_verification` table; the
// listing hook surfaces those for the contacts list / banners elsewhere.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

export interface SafetyNumberInfo {
  my_fingerprint: string;
  peer_fingerprint: string;
  /** Concatenated fingerprint used for the QR / readable code display. */
  combined: string;
  /** Verification status pinned locally. */
  verification: "unverified" | "verified" | "changed";
  peer_identity_version: number;
}

export interface PeerVerification {
  peer_user_id: string;
  peer_username?: string;
  status: "verified" | "changed";
  verified_at: string;
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
