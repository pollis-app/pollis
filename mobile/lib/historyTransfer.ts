// #1735: copy message history between this account's own devices. Wire types and the rules the screens share. Pure.

export type TransferVia = "link" | "sibling";
export type TransferRole = "source" | "receiver";
export type TransferState =
  | "awaiting_approval"
  | "transferring"
  | "paused"
  | "completed"
  | "declined"
  | "cancelled"
  | "expired"
  | "failed";

export interface HistoryTransfer {
  transfer_id: string;
  role: TransferRole;
  peer_device_id: string;
  peer_device_name: string | null;
  via: TransferVia;
  state: TransferState;
  chunks_done: number;
  rows_done: number;
  rows_skipped: number;
  deferred_conversations: number;
  // Rows the source expects to send, when it knows; drives a percentage.
  estimated_total?: number | null;
  error?: string | null;
  updated_at: string;
}

export interface TransferSource {
  device_id: string;
  device_name: string | null;
  platform?: string | null;
  last_active_at?: string | null;
  online: boolean;
  // Absent means an older core that did not say; only `false` disables the row.
  supports_transfer?: boolean | null;
  // The device that approved this one's enrollment, when the core knows it.
  approved_this_device?: boolean | null;
}

export interface PendingTransferRequest {
  transfer_id: string;
  requester_device_id: string;
  requester_device_name: string | null;
  requester_added_at: string | null;
  via: TransferVia;
  pin_required: boolean;
  created_at: string;
}

const TERMINAL: ReadonlySet<TransferState> = new Set<TransferState>([
  "completed",
  "declined",
  "cancelled",
  "expired",
  "failed",
]);

export function isTerminal(state: TransferState): boolean {
  return TERMINAL.has(state);
}

/** Everything not finished is driven by `run_history_transfer`, on both sides. */
export function needsRun(transfer: HistoryTransfer): boolean {
  return !isTerminal(transfer.state);
}

export function canCancel(transfer: HistoryTransfer): boolean {
  return !isTerminal(transfer.state);
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

/** What the screen shows. A completed copy that left conversations behind reads as "deferred", with a retry. */
export function transferPhase(transfer: HistoryTransfer): TransferPhase {
  switch (transfer.state) {
    case "awaiting_approval":
      return "waiting";
    case "transferring":
      return "transferring";
    case "paused":
      return "paused";
    case "completed":
      return transfer.deferred_conversations > 0 ? "deferred" : "done";
    default:
      return transfer.state;
  }
}

/**
 * The one retry a phase offers. `resume` runs the same transfer again (it continues from its cursor);
 * `request` asks the same device again, which only the receiver can do (re-running is idempotent).
 */
export function retryKind(transfer: HistoryTransfer): "resume" | "request" | null {
  const phase = transferPhase(transfer);
  if (phase === "paused") {
    return "resume";
  }
  if (transfer.role !== "receiver") {
    return null;
  }
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
  // Server timestamps may be zone-less UTC ("YYYY-MM-DD HH:MM:SS").
  const normal = /[zZ]|[+-]\d\d:?\d\d$/.test(iso) ? iso : `${iso.replace(" ", "T")}Z`;
  const ms = Date.parse(normal);
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
    const active = time(b.last_active_at) - time(a.last_active_at);
    if (active !== 0) {
      return active;
    }
    return (a.device_name ?? a.device_id).localeCompare(b.device_name ?? b.device_id);
  });
}

/** Link time: the device that approved this one, else the only candidate; null means "let the user pick". */
export function preferredSource(
  sources: readonly TransferSource[],
  approverDeviceId: string | null,
): TransferSource | null {
  const usable = sources.filter(isSupported);
  const approver =
    (approverDeviceId ? usable.find((s) => s.device_id === approverDeviceId) : undefined) ??
    usable.find((s) => s.approved_this_device === true);
  if (approver) {
    return approver;
  }
  return usable.length === 1 ? usable[0] : null;
}

function newestFirst(a: HistoryTransfer, b: HistoryTransfer): number {
  return time(b.updated_at) - time(a.updated_at);
}

/** The transfer a page follows for one role: the unfinished one, else the newest. */
export function currentTransfer(
  transfers: readonly HistoryTransfer[],
  role: TransferRole,
  peerDeviceId?: string | null,
): HistoryTransfer | null {
  const mine = transfers
    .filter((t) => t.role === role && (!peerDeviceId || t.peer_device_id === peerDeviceId))
    .sort(newestFirst);
  return mine.find((t) => !isTerminal(t.state)) ?? mine[0] ?? null;
}

/** Fold one fresh row into a cached list (a run's result, a request's reply). */
export function mergeTransfer(
  list: readonly HistoryTransfer[] | undefined,
  next: HistoryTransfer,
): HistoryTransfer[] {
  const rest = (list ?? []).filter((t) => t.transfer_id !== next.transfer_id);
  return [next, ...rest];
}

/** 0–100 when the source told us how much is coming, else null (an indeterminate bar). */
export function progressPercent(transfer: HistoryTransfer): number | null {
  const total = transfer.estimated_total;
  if (!total || total <= 0) {
    return null;
  }
  const done = transfer.rows_done + transfer.rows_skipped;
  return Math.max(0, Math.min(100, Math.round((done / total) * 100)));
}

/** How long a finished copy's outcome stays on the picker page until "Done" (e.g. after a relaunch). */
const OUTCOME_SHOWN_MS = 24 * 60 * 60 * 1000;

/**
 * Whether the picker page leads with this receiver transfer: always while it runs; once finished, until the
 * user dismisses it, so a copy that ended while the app was closed or killed still reports how it ended.
 */
export function showsOutcome(
  transfer: HistoryTransfer | null,
  dismissed: ReadonlySet<string>,
  now: number,
): boolean {
  if (!transfer || dismissed.has(transfer.transfer_id)) {
    return false;
  }
  if (!isTerminal(transfer.state)) {
    return true;
  }
  return now - time(transfer.updated_at) <= OUTCOME_SHOWN_MS;
}
