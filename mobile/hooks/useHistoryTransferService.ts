// #1735: the background half of history transfer — opens the source's approval page and keeps unfinished
// transfers running. Event-driven only: a nudge, landing in the app (sign-in / unlock), a foreground return.

import { useCallback, useEffect, useRef } from "react";
import { AppState, type AppStateStatus } from "react-native";
import { useRouter, useSegments } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { useQueryClient } from "@tanstack/react-query";
import { appStore } from "../stores/appStore";
import {
  canPresentTransferApproval,
  nextTransferApproval,
  nudgeWantsPendingCheck,
  nudgeWantsRun,
  onHistoryTransferNudge,
} from "../lib/historyTransferPrompt";
import { isInSignedInApp } from "../lib/enrollmentPrompt";
import {
  historyTransferQueryKeys,
  listPendingHistoryTransferRequests,
  resumeHistoryTransfers,
  type PendingTransferRequest,
} from "./queries/useHistoryTransfer";

export function useHistoryTransferService() {
  const router = useRouter();
  const segments = useSegments();
  const queryClient = useQueryClient();
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);
  const signedIn = !!userId && isInSignedInApp(segments, isLocked);
  const presentable = !!userId && canPresentTransferApproval(segments, isLocked);

  const presented = useRef<Set<string>>(new Set());
  const presentableRef = useRef(presentable);
  presentableRef.current = presentable;
  const signedInRef = useRef(signedIn);
  signedInRef.current = signedIn;
  // A request seen while the page could not be shown (another approval page, a lock).
  const held = useRef<PendingTransferRequest | null>(null);

  useEffect(() => {
    presented.current = new Set();
    held.current = null;
  }, [userId]);

  const present = useCallback(
    (req: PendingTransferRequest) => {
      if (presented.current.has(req.transfer_id)) {
        return;
      }
      if (!presentableRef.current) {
        held.current = req;
        return;
      }
      presented.current.add(req.transfer_id);
      held.current = null;
      router.push({ pathname: "/approve-history-transfer", params: { transferId: req.transfer_id } });
    },
    [router],
  );

  const checkPending = useCallback(async () => {
    if (!userId) {
      return;
    }
    try {
      const pending = await listPendingHistoryTransferRequests();
      queryClient.setQueryData(historyTransferQueryKeys.pending(userId), pending);
      const next = nextTransferApproval(pending, presented.current);
      if (next) {
        present(next);
      }
    } catch (e) {
      console.warn("[history-transfer] pending check failed:", e);
    }
  }, [userId, queryClient, present]);

  const resume = useCallback(async () => {
    if (!userId) {
      return;
    }
    try {
      await resumeHistoryTransfers();
    } catch (e) {
      console.warn("[history-transfer] resume failed:", e);
    }
  }, [userId]);

  useEffect(() => {
    if (!userId) {
      return;
    }
    return onHistoryTransferNudge((nudge) => {
      if (!signedInRef.current) {
        return;
      }
      if (nudgeWantsPendingCheck(nudge.event)) {
        void checkPending();
      }
      if (nudgeWantsRun(nudge.event)) {
        void resume();
      } else {
        void queryClient.invalidateQueries({ queryKey: historyTransferQueryKeys.transfers(userId) });
      }
    });
  }, [userId, queryClient, checkPending, resume]);

  // Landing in the signed-in app (sign-in, or the PIN after a lock): catch up and resume.
  useEffect(() => {
    if (!signedIn) {
      return;
    }
    void resume();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [signedIn, userId]);

  useEffect(() => {
    if (!presentable) {
      return;
    }
    const waiting = held.current;
    if (waiting) {
      present(waiting);
    }
    void checkPending();
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
      if (!resumed) {
        return;
      }
      if (signedInRef.current) {
        void resume();
      }
      if (presentableRef.current) {
        void checkPending();
      }
    });
    return () => {
      sub.remove();
    };
  }, [userId, checkPending, resume]);
}
