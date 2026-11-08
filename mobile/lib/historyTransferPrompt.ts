// #1735: the source side's approval page, opened like approve-device (nudge, landing, foreground). Pure.

import { isInSignedInApp } from "./enrollmentPrompt.ts";
import type { PendingTransferRequest } from "./historyTransfer.ts";

/** Exactly the DS's `history_transfer` nudge events (pollis-delivery `history_transfer::nudge`). */
export type HistoryTransferEventKind =
  | "requested"
  | "accepted"
  | "chunk"
  | "acked"
  | "declined"
  | "cancelled"
  | "completed";

export const HISTORY_TRANSFER_EVENT_KINDS: readonly HistoryTransferEventKind[] = [
  "requested",
  "accepted",
  "chunk",
  "acked",
  "declined",
  "cancelled",
  "completed",
];

export interface HistoryTransferNudge {
  transfer_id: string;
  event: HistoryTransferEventKind;
}

/** Only inside the signed-in, unlocked app, and never on top of another approval page. */
export function canPresentTransferApproval(segments: readonly string[], isLocked: boolean): boolean {
  if (!isInSignedInApp(segments, isLocked)) {
    return false;
  }
  return segments[0] !== "approve-history-transfer" && segments[0] !== "approve-device";
}

// Requester devices whose request a visible screen answers in place (link-device's "Linked" page).
const inlineClaims = new Set<string>();

/** Returns a release. While held, requests from that device are not opened as a page; releasing re-checks. */
export function claimInline(requesterDeviceId: string): () => void {
  inlineClaims.add(requesterDeviceId);
  return () => {
    if (inlineClaims.delete(requesterDeviceId)) {
      // A request left unanswered on the inline screen now opens as a page.
      noticeHistoryTransfer({ transfer_id: "", event: "requested" });
    }
  };
}

export function isClaimedInline(requesterDeviceId: string): boolean {
  return inlineClaims.has(requesterDeviceId);
}

/** The newest request not yet presented in this process and not answered in place. */
export function nextTransferApproval(
  pending: readonly PendingTransferRequest[],
  presented: ReadonlySet<string>,
): PendingTransferRequest | null {
  const sorted = [...pending].sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
  for (const req of sorted) {
    if (!presented.has(req.transfer_id) && !isClaimedInline(req.requester_device_id)) {
      return req;
    }
  }
  return null;
}

// `requested` means "look at pending"; every other event means "(re)run it": a step forward
// restarts a paused run, and an end state lets the local row learn how it ended.
export function nudgeWantsPendingCheck(event: HistoryTransferEventKind): boolean {
  return event === "requested" || event === "cancelled" || event === "declined";
}

export function nudgeWantsRun(event: HistoryTransferEventKind): boolean {
  return event !== "requested";
}

type Listener = (nudge: HistoryTransferNudge) => void;
const listeners = new Set<Listener>();

export function onHistoryTransferNudge(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function noticeHistoryTransfer(nudge: HistoryTransferNudge): void {
  for (const listener of [...listeners]) {
    listener(nudge);
  }
}
