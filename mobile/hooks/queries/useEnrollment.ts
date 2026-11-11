// Device enrollment hooks: the new device's side and the approving device's.

import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import {
  waitForEnrollment,
  type EnrollmentStatus,
  type EnrollmentWaitOutcome,
} from "../../lib/enrollmentWait";

export interface EnrollmentHandle {
  request_id: string;
  verification_code: string;
  expires_at: string;
}

/// No verification code on purpose (#1096): the approver types the one shown on the new device.
export interface PendingEnrollmentRequest {
  request_id: string;
  new_device_id: string;
  created_at: string;
  expires_at: string;
}

export const enrollmentQueryKeys = {
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

/**
 * The new device's wait for approval (#1809): one awaited `await_enrollment_approval` per
 * request and attempt, never a poll. `retry` starts a fresh await on the same request.
 * Unmounting, a new handle or a retry cancels the wait; a late answer is ignored.
 */
export function useAwaitEnrollmentApproval(handle: EnrollmentHandle | null) {
  const [attempt, setAttempt] = useState(0);
  const [settled, setSettled] = useState<{
    handle: EnrollmentHandle;
    attempt: number;
    outcome: EnrollmentWaitOutcome;
  } | null>(null);

  useEffect(() => {
    if (!handle) {
      return;
    }
    return waitForEnrollment({
      requestId: handle.request_id,
      expiresAt: handle.expires_at,
      call: (requestId) =>
        invoke<EnrollmentStatus>("await_enrollment_approval", { requestId }),
      onSettle: (outcome) => setSettled({ handle, attempt, outcome }),
    });
  }, [handle, attempt]);

  const outcome =
    settled && settled.handle === handle && settled.attempt === attempt
      ? settled.outcome
      : null;
  return { outcome, retry: () => setAttempt((a) => a + 1) };
}

export function useFinalizeEnrollment() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
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
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
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
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
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
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
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
