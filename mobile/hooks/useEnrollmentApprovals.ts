// Opens a new device's approval request (#1360) on the live nudge, on landing
// in the app and on every resume; never on a timer. Rules: `lib/enrollmentPrompt.ts`.

import { useCallback, useEffect, useRef } from "react";
import { AppState, type AppStateStatus } from "react-native";
import { useRouter, useSegments } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { useQueryClient } from "@tanstack/react-query";
import { appStore } from "../stores/appStore";
import { invoke } from "../lib/native";
import {
  canPresentApproval,
  nextApproval,
  onEnrollmentRequested,
  type ApprovalCandidate,
} from "../lib/enrollmentPrompt";
import { enrollmentQueryKeys, type PendingEnrollmentRequest } from "./queries/useEnrollment";

export function useEnrollmentApprovals() {
  const router = useRouter();
  const segments = useSegments();
  const queryClient = useQueryClient();
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);
  const presentable = !!userId && canPresentApproval(segments, isLocked);

  // So backing out of one does not re-open it on the next resume.
  const presented = useRef<Set<string>>(new Set());
  const queued = useRef<ApprovalCandidate | null>(null);
  const presentableRef = useRef(presentable);
  presentableRef.current = presentable;

  useEffect(() => {
    presented.current = new Set();
    queued.current = null;
  }, [userId]);

  const present = useCallback(
    (req: ApprovalCandidate) => {
      if (presented.current.has(req.request_id)) {
        return;
      }
      if (!presentableRef.current) {
        queued.current = req;
        return;
      }
      presented.current.add(req.request_id);
      queued.current = null;
      router.push({
        pathname: "/approve-device",
        params: { requestId: req.request_id, newDeviceId: req.new_device_id },
      });
    },
    [router],
  );

  const check = useCallback(async () => {
    if (!userId) {
      return;
    }
    try {
      const pending = await invoke<PendingEnrollmentRequest[]>(
        "list_pending_enrollment_requests",
        { userId },
      );
      queryClient.setQueryData(enrollmentQueryKeys.pending(userId), pending);
      const next = nextApproval(pending, presented.current);
      if (next) {
        present(next);
      }
    } catch (e) {
      console.warn("[enrollment] pending-request check failed:", e);
    }
  }, [userId, queryClient, present]);

  useEffect(() => {
    if (!userId) {
      return;
    }
    return onEnrollmentRequested((req) => {
      void queryClient.invalidateQueries({ queryKey: enrollmentQueryKeys.pending(userId) });
      present(req);
    });
  }, [userId, queryClient, present]);

  useEffect(() => {
    if (!presentable) {
      return;
    }
    const held = queued.current;
    if (held) {
      present(held);
    }
    void check();
    // `segments` changes on every navigation; only the transition matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [presentable, userId]);

  useEffect(() => {
    if (!userId) {
      return;
    }
    let previous: AppStateStatus = AppState.currentState;
    const sub = AppState.addEventListener("change", (next) => {
      const resumed = previous !== "active" && next === "active";
      previous = next;
      if (resumed && presentableRef.current) {
        void check();
      }
    });
    return () => {
      sub.remove();
    };
  }, [userId, check]);
}
