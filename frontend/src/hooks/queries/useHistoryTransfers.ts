import { useQuery, type QueryClient } from "@tanstack/react-query";
import { Channel } from "../../bridge";
import * as api from "../../services/api";
import { historyTransferStore } from "../../stores/historyTransferStore";
import { needsRun, upsertTransfer } from "../../utils/historyTransfer";
import { errorMessage } from "../../utils/errorMessage";

/** #1735: copy message history between this account's own devices. The cache is cleared on lock and sign-out, so keys need no user. */
export const historyTransferKeys = {
  all: ["history-transfers"] as const,
  transfers: ["history-transfers", "list"] as const,
  pending: ["history-transfers", "pending"] as const,
  sources: ["history-transfers", "sources"] as const,
};

export function useHistoryTransfers(enabled = true) {
  return useQuery({
    queryKey: historyTransferKeys.transfers,
    queryFn: () => api.listHistoryTransfers(),
    enabled,
  });
}

export function usePendingHistoryTransferRequests(enabled = true) {
  return useQuery({
    queryKey: historyTransferKeys.pending,
    queryFn: () => api.listPendingHistoryTransferRequests(),
    enabled,
  });
}

export function useHistoryTransferSources(enabled = true) {
  return useQuery({
    queryKey: historyTransferKeys.sources,
    queryFn: () => api.listHistoryTransferSources(),
    enabled,
  });
}

/** Write one transfer's newest state into the list the screens read. */
export function storeTransfer(queryClient: QueryClient, transfer: api.HistoryTransfer): void {
  queryClient.setQueryData<api.HistoryTransfer[]>(historyTransferKeys.transfers, (list) =>
    upsertTransfer(list, transfer),
  );
}

const inFlight = new Map<string, Promise<api.HistoryTransfer | null>>();

/**
 * Drive one transfer with `run_history_transfer` until it finishes or pauses. One run per transfer per
 * window: a second call joins the first. Progress lands in the transfers query as it streams.
 */
export function runTransfer(queryClient: QueryClient, transferId: string): Promise<api.HistoryTransfer | null> {
  const existing = inFlight.get(transferId);
  if (existing) {
    return existing;
  }
  const channel = new Channel<api.HistoryTransfer>();
  channel.onmessage = (progress) => {
    storeTransfer(queryClient, progress);
  };
  historyTransferStore.setRunning(transferId, true);
  const run = api
    .runHistoryTransfer(transferId, channel)
    .then((result) => {
      storeTransfer(queryClient, result);
      return result;
    })
    .catch((err: unknown) => {
      console.warn("[history-transfer] run failed:", err);
      // Keep the reason on screen, then re-read what the core holds: a run that threw can still be resumed.
      const known = queryClient
        .getQueryData<api.HistoryTransfer[]>(historyTransferKeys.transfers)
        ?.find((t) => t.transfer_id === transferId);
      if (known) {
        storeTransfer(queryClient, { ...known, error: errorMessage(err) });
      }
      void queryClient.invalidateQueries({ queryKey: historyTransferKeys.transfers });
      return null;
    })
    .finally(() => {
      inFlight.delete(transferId);
      historyTransferStore.setRunning(transferId, false);
    });
  inFlight.set(transferId, run);
  return run;
}

/** After unlock, or on a hint from the other device: run every transfer that is not finished and not already running. */
export async function resumeTransfers(queryClient: QueryClient, onlyId?: string): Promise<void> {
  let transfers: api.HistoryTransfer[];
  try {
    transfers = await queryClient.fetchQuery({
      queryKey: historyTransferKeys.transfers,
      queryFn: () => api.listHistoryTransfers(),
      staleTime: 0,
    });
  } catch (err) {
    console.warn("[history-transfer] list failed:", err);
    return;
  }
  for (const transfer of transfers) {
    if (onlyId && transfer.transfer_id !== onlyId) {
      continue;
    }
    if (needsRun(transfer) && !inFlight.has(transfer.transfer_id)) {
      void runTransfer(queryClient, transfer.transfer_id);
    }
  }
}

/** Ask a sibling for its history and start driving the request. */
export async function requestTransfer(
  queryClient: QueryClient,
  sourceDeviceId: string,
  via: api.HistoryTransferVia,
): Promise<api.HistoryTransfer> {
  const transfer = await api.requestHistoryTransfer(sourceDeviceId, via);
  storeTransfer(queryClient, transfer);
  void runTransfer(queryClient, transfer.transfer_id);
  return transfer;
}

/** Exactly the DS's `history_transfer` nudge events (pollis-delivery `history_transfer::nudge`). */
export type HistoryTransferHint =
  | "requested"
  | "accepted"
  | "chunk"
  | "acked"
  | "declined"
  | "cancelled"
  | "completed";

/** The nudges after which the transfer is over (the local run learns it on its next status read). */
export function isTerminalHint(event: HistoryTransferHint): boolean {
  return event === "declined" || event === "cancelled" || event === "completed";
}

/**
 * The DS's content-free `history_transfer` nudge: re-read what changed, and pick up a transfer the other
 * device just moved forward if nothing here is driving it (a paused one resumes on its own). Every
 * non-terminal step re-runs it; a terminal one re-runs it too so the local row learns how it ended.
 */
export function onHistoryTransferHint(queryClient: QueryClient, transferId: string, event: HistoryTransferHint): void {
  if (event === "requested" || isTerminalHint(event)) {
    void queryClient.invalidateQueries({ queryKey: historyTransferKeys.pending });
    void queryClient.invalidateQueries({ queryKey: historyTransferKeys.transfers });
  }
  if (event === "requested" || inFlight.has(transferId)) {
    return;
  }
  void resumeTransfers(queryClient, transferId);
}
