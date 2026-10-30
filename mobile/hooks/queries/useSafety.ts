// Safety-number hooks for the DM's "You & <name>" screen (#1563); never the own user's number.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

export interface SafetyNumberInfo {
  safety_number: string;
  status: "unverified" | "verified" | "changed";
  peer_identity_version: number;
  qr_payload: string;
}

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
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
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

/** One recorded change of a peer's pinned key (#1605); mirrors `KeyChangeNotice` in safety.rs. */
export interface KeyChangeNotice {
  id: number;
  peer_user_id: string;
  /** UTC ISO 8601 with milliseconds: where the line sits in the timeline. */
  detected_at: string;
}

export const keyChangeNoticeKeys = {
  all: ["safety", "key-change-notices"] as const,
  forPeers: (peerUserIds: string[]) =>
    ["safety", "key-change-notices", [...peerUserIds].sort().join(",")] as const,
};

/** A DM's safety-number change notices, whichever path recorded them (app, iOS extension, Android FCM). Local only. */
export function useKeyChangeNotices(peerUserIds: string[]) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: keyChangeNoticeKeys.forPeers(peerUserIds),
    queryFn: async (): Promise<KeyChangeNotice[]> => {
      return await invoke<KeyChangeNotice[]>("list_key_change_notices", {
        peerUserIds,
      });
    },
    enabled: !!currentUser && peerUserIds.length > 0,
    staleTime: 1000 * 30,
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
