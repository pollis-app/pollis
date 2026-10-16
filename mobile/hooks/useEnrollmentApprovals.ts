// #1360: bring a new device's approval request to the user instead of waiting
// for them to find it under Self → Security. Mounted once, beside the inbox
// realtime (`app/_layout.tsx`). Desktop's twin is the enrollment poll fallback
// plus the `enrollment_requested` takeover in `frontend/src/hooks/useLiveKitRealtime.ts`.
//
// Triggers, all event-driven (no timer):
//   - the DS's live `enrollment_requested` nudge (`useInboxRealtime` hands it
//     to `noticeEnrollmentRequested`);
//   - landing in the signed-in, unlocked app — sign-in, unlock, cold start —
//     which catches a request filed while the phone was closed;
//   - every return to the foreground, since a nudge sent while backgrounded is
//     gone.
// Rules (which request, when it may show): `lib/enrollmentPrompt.ts`.

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

  // Requests already opened in this process, so backing out of one does not
  // re-open it on the next resume. Reset with the user.
  const presented = useRef<Set<string>>(new Set());
  // A nudge that arrived while the screen could not be shown (locked, still
  // in the auth flow); opened as soon as it can be.
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

  // Ask the DS what is pending, refresh the Security card from the answer, and
  // open the newest request not yet shown.
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

  // Live nudge.
  useEffect(() => {
    if (!userId) {
      return;
    }
    return onEnrollmentRequested((req) => {
      void queryClient.invalidateQueries({ queryKey: enrollmentQueryKeys.pending(userId) });
      present(req);
    });
  }, [userId, queryClient, present]);

  // Landing in the signed-in app (sign-in, unlock, cold start): catch up on
  // anything filed meanwhile, and open a nudge that was held back.
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

  // Return to the foreground.
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
