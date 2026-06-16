// Device enrollment hooks. Two sides of the same flow:
//
// 1. **New device** (auth/enrollment screen):
//    - `useStartEnrollment` opens an enrollment request and returns a
//      short verification code the user reads aloud to their existing
//      device.
//    - `useEnrollmentStatus` polls Turso for the approval-or-not.
//    - `useFinalizeEnrollment` finishes setup once status flips to
//      `approved`.
//    - `useRecoverWithSecretKey` is the alternate path — enter the
//      recovery key emitted at first signup.
//
// 2. **Existing device** (self/security screen):
//    - `usePendingEnrollmentRequests` lists requests from sibling devices.
//    - `useApproveEnrollment` / `useRejectEnrollment` decide them.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

export interface EnrollmentHandle {
  request_id: string;
  verification_code: string;
  expires_at: string;
}

export type EnrollmentStatusKind = "pending" | "approved" | "rejected" | "expired";
export interface EnrollmentStatus {
  status: EnrollmentStatusKind;
}

export interface PendingEnrollmentRequest {
  request_id: string;
  new_device_id: string;
  verification_code: string;
  created_at: string;
  expires_at: string;
}

export const enrollmentQueryKeys = {
  status: (requestId: string | null) => ["enrollment", "status", requestId] as const,
  pending: (userId: string | null) => ["enrollment", "pending", userId] as const,
};

export function useStartEnrollment() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (): Promise<EnrollmentHandle> => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      return await invoke<EnrollmentHandle>("start_device_enrollment", {
        userId: currentUser.id,
      });
    },
  });
}

/** Polls every 3s while the request is pending. Stops as soon as the
 *  server reports a terminal state. */
export function useEnrollmentStatus(requestId: string | null) {
  return useQuery({
    queryKey: enrollmentQueryKeys.status(requestId),
    queryFn: async (): Promise<EnrollmentStatus> => {
      if (!requestId) {
        throw new Error("No request");
      }
      return await invoke<EnrollmentStatus>("poll_enrollment_status", {
        requestId,
      });
    },
    enabled: !!requestId,
    refetchInterval: (q) => {
      const data = q.state.data as EnrollmentStatus | undefined;
      if (!data || data.status === "pending") {
        return 3_000;
      }
      return false;
    },
  });
}

export function useFinalizeEnrollment() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async () => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("finalize_device_enrollment", { userId: currentUser.id });
    },
  });
}

export function useRecoverWithSecretKey() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (secretKey: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("recover_with_secret_key", {
        userId: currentUser.id,
        secretKey,
      });
    },
  });
}

export function usePendingEnrollmentRequests() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: enrollmentQueryKeys.pending(currentUser?.id ?? null),
    queryFn: async (): Promise<PendingEnrollmentRequest[]> => {
      if (!currentUser) {
        return [];
      }
      return await invoke<PendingEnrollmentRequest[]>(
        "list_pending_enrollment_requests",
        { userId: currentUser.id },
      );
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
  });
}

export function useApproveEnrollment() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (vars: { requestId: string; verificationCode: string }) => {
      await invoke("approve_device_enrollment", vars);
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: enrollmentQueryKeys.pending(currentUser?.id ?? null),
      });
    },
  });
}

export function useRejectEnrollment() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (requestId: string) => {
      await invoke("reject_device_enrollment", { requestId });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: enrollmentQueryKeys.pending(currentUser?.id ?? null),
      });
    },
  });
}
