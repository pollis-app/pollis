/* #1735: the rules the history-transfer screens share, kept pure so they are unit-tested. Mirrors mobile/lib/historyTransfer.ts. */

import type {
  HistoryTransfer,
  HistoryTransferState,
  TransferSource,
} from "../services/api";

const TERMINAL: ReadonlySet<HistoryTransferState> = new Set<HistoryTransferState>([
  "completed",
  "declined",
  "cancelled",
  "expired",
  "failed",
]);

export function isTerminal(state: HistoryTransferState): boolean {
  return TERMINAL.has(state);
}

/**
 * Whether this device should drive the transfer with `run_history_transfer` (after unlock, or on a hint).
 * The receiver waits on the source's answer inside the run, so its unanswered request runs too; an
 * unanswered request on the source is the approval page's, never a run.
 */
export function needsRun(transfer: HistoryTransfer): boolean {
  if (isTerminal(transfer.state)) {
    return false;
  }
  if (transfer.role === "source" && transfer.state === "awaiting_approval") {
    return false;
  }
  return true;
}

export type TransferPhase =
  | "waiting"
  | "transferring"
  | "paused"
  | "done"
  | "deferred"
  | "declined"
  | "cancelled"
  | "expired"
  | "failed";

/** What the screen shows: a completed copy that left conversations behind reads as "deferred", with a retry. */
export function transferPhase(transfer: HistoryTransfer): TransferPhase {
  switch (transfer.state) {
    case "awaiting_approval":
      return "waiting";
    case "completed":
      return transfer.deferred_conversations > 0 ? "deferred" : "done";
    default:
      return transfer.state;
  }
}

/** `resume` runs the same transfer again (it continues from its cursor); `request` asks the same device again (receiver only; re-running is idempotent). */
export function retryKind(transfer: HistoryTransfer, running: boolean): "resume" | "request" | null {
  const phase = transferPhase(transfer);
  if (phase === "paused") {
    return running ? null : "resume";
  }
  if (transfer.role !== "receiver") {
    return null;
  }
  // As on mobile: a decline is the other device's answer, so asking again is a deliberate new pick.
  if (phase === "failed" || phase === "expired" || phase === "deferred" || phase === "cancelled") {
    return "request";
  }
  return null;
}

export function isSupported(source: TransferSource): boolean {
  return source.supports_transfer !== false;
}

function time(iso: string | null | undefined): number {
  if (!iso) {
    return 0;
  }
  const ms = Date.parse(iso);
  return Number.isNaN(ms) ? 0 : ms;
}

/** Online first, then most recently active; devices that cannot send history go last. */
export function sortSources(sources: readonly TransferSource[]): TransferSource[] {
  return [...sources].sort((a, b) => {
    const supported = Number(isSupported(b)) - Number(isSupported(a));
    if (supported !== 0) {
      return supported;
    }
    const online = Number(b.online) - Number(a.online);
    if (online !== 0) {
      return online;
    }
    return time(b.last_active_at) - time(a.last_active_at);
  });
}

/**
 * The device the link-time offer names: the one that approved this enrollment when the core says so,
 * else the only device that could send. Null when the user has to pick (or there is nothing to offer).
 */
export function linkOfferSource(sources: readonly TransferSource[]): TransferSource | null {
  const usable = sources.filter(isSupported);
  const approver = usable.find((s) => s.approved_this_device === true);
  if (approver) {
    return approver;
  }
  return usable.length === 1 ? usable[0] : null;
}

/** Newest first; the transfer a receiver screen should show, preferring one still in flight. */
export function currentReceiverTransfer(transfers: readonly HistoryTransfer[]): HistoryTransfer | null {
  const mine = transfers
    .filter((t) => t.role === "receiver")
    .sort((a, b) => time(b.updated_at) - time(a.updated_at));
  return mine.find((t) => !isTerminal(t.state)) ?? null;
}

/** Replace a transfer in a list by id, or put it first. */
export function upsertTransfer(
  list: readonly HistoryTransfer[] | undefined,
  next: HistoryTransfer,
): HistoryTransfer[] {
  const rest = (list ?? []).filter((t) => t.transfer_id !== next.transfer_id);
  return [next, ...rest];
}
