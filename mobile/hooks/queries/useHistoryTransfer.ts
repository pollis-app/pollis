// #1735: copy message history between this account's own devices — typed bridge wrappers, queries, the runner.

import { useMutation, useQuery } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { queryClient } from "../../lib/queryClient";
import { appStore } from "../../stores/appStore";
import { createTransferRunner } from "../../lib/historyTransferRunner";
import {
  mergeTransfer,
  type HistoryTransfer,
  type PendingTransferRequest,
  type TransferSource,
  type TransferVia,
} from "../../lib/historyTransfer";
import { securityEventQueryKeys } from "./useSecurityEvents";

export type { HistoryTransfer, PendingTransferRequest, TransferSource } from "../../lib/historyTransfer";

export const historyTransferQueryKeys = {
  all: ["historyTransfer"] as const,
  transfers: (userId: string | null) => ["historyTransfer", "list", userId] as const,
  sources: (userId: string | null) => ["historyTransfer", "sources", userId] as const,
  pending: (userId: string | null) => ["historyTransfer", "pending", userId] as const,
};

// ── Bridge wrappers (pollis-core `commands::history_transfer`) ──

export async function listHistoryTransferSources(): Promise<TransferSource[]> {
  return (await invoke<TransferSource[]>("list_history_transfer_sources")) ?? [];
}

export async function requestHistoryTransfer(
  sourceDeviceId: string,
  via: TransferVia,
): Promise<HistoryTransfer> {
  return await invoke<HistoryTransfer>("request_history_transfer", { sourceDeviceId, via });
}

export async function listPendingHistoryTransferRequests(): Promise<PendingTransferRequest[]> {
  return (await invoke<PendingTransferRequest[]>("list_pending_history_transfer_requests")) ?? [];
}

/** A wrong PIN fails like every other PIN screen and counts toward the lockout. */
export async function approveHistoryTransfer(transferId: string, pin?: string): Promise<HistoryTransfer> {
  return await invoke<HistoryTransfer>(
    "approve_history_transfer",
    pin ? { transferId, pin } : { transferId },
  );
}

export async function declineHistoryTransfer(transferId: string): Promise<void> {
  await invoke("decline_history_transfer", { transferId });
}

export async function listHistoryTransfers(): Promise<HistoryTransfer[]> {
  return (await invoke<HistoryTransfer[]>("list_history_transfers")) ?? [];
}

/** One awaited call that returns at a terminal state or "paused". Progress shows through re-reads. */
export async function runHistoryTransfer(transferId: string): Promise<HistoryTransfer> {
  return await invoke<HistoryTransfer>("run_history_transfer", { transferId });
}

export async function cancelHistoryTransfer(transferId: string): Promise<void> {
  await invoke("cancel_history_transfer", { transferId });
}

// ── Cache ──

function currentUserId(): string | null {
  return appStore.currentUser?.id ?? null;
}

export function putTransfer(transfer: HistoryTransfer): void {
  queryClient.setQueryData<HistoryTransfer[]>(
    historyTransferQueryKeys.transfers(currentUserId()),
    (prev) => mergeTransfer(prev, transfer),
  );
}

export function refreshHistoryTransfers(): Promise<void> {
  return queryClient.invalidateQueries({ queryKey: historyTransferQueryKeys.all });
}

// One runner per process: both roles, every screen and the background service share it.
export const transferRunner = createTransferRunner({
  run: runHistoryTransfer,
  onResult: (transfer) => {
    putTransfer(transfer);
    if (transfer.state === "completed") {
      void queryClient.invalidateQueries({ queryKey: securityEventQueryKeys.all });
    }
  },
  onError: (transferId, e) => {
    console.warn(`[history-transfer] run ${transferId} failed:`, e);
    void refreshHistoryTransfers();
  },
});

/** Read the list, cache it, and run whatever is unfinished. */
export async function resumeHistoryTransfers(): Promise<HistoryTransfer[]> {
  const list = await listHistoryTransfers();
  queryClient.setQueryData(historyTransferQueryKeys.transfers(currentUserId()), list);
  void transferRunner.kickAll(list);
  return list;
}

// ── Link-time offer (new device) ──
// Set when this device has just been enrolled and finalized; the offer page reads and clears it.

let linkOffer: { approverDeviceId: string | null } | null = null;
let approverSeen: string | null = null;

/** The enrollment poll saw who approved this device. */
export function noteEnrollmentApprover(deviceId: string | null | undefined): void {
  if (deviceId) {
    approverSeen = deviceId;
  }
}

/** Finalize ran for a newly linked device: offer its history before entering the app. */
export function markLinkOffer(): void {
  linkOffer = { approverDeviceId: approverSeen };
}

export function linkOfferPending(): boolean {
  return linkOffer !== null;
}

export function takeLinkOffer(): { approverDeviceId: string | null } | null {
  const offer = linkOffer;
  linkOffer = null;
  approverSeen = null;
  return offer;
}

/** Where setup lands: the history offer when one is due, else the app. */
export function afterSetupRoute(): "/(auth)/history-offer" | "/(tabs)/groups" {
  return linkOfferPending() ? "/(auth)/history-offer" : "/(tabs)/groups";
}

// ── Hooks ──

export function useHistoryTransfers() {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  return useQuery({
    queryKey: historyTransferQueryKeys.transfers(userId),
    queryFn: listHistoryTransfers,
    enabled: !!userId,
  });
}

export function useTransferSources(enabled = true) {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  return useQuery({
    queryKey: historyTransferQueryKeys.sources(userId),
    queryFn: listHistoryTransferSources,
    enabled: !!userId && enabled,
    staleTime: 15_000,
  });
}

export function usePendingTransferRequests() {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  return useQuery({
    queryKey: historyTransferQueryKeys.pending(userId),
    queryFn: listPendingHistoryTransferRequests,
    enabled: !!userId,
  });
}

/** Receiver: ask a device, then start running (it waits for the approval in Rust). */
export function useRequestHistoryTransfer() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { sourceDeviceId: string; via: TransferVia }) =>
      await requestHistoryTransfer(vars.sourceDeviceId, vars.via),
    onSuccess: (transfer) => {
      putTransfer(transfer);
      void transferRunner.kick(transfer.transfer_id);
    },
  });
}

/** Source: approve (PIN unless the link-time shortcut applies), then send. */
export function useApproveHistoryTransfer() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { transferId: string; pin?: string }) =>
      await approveHistoryTransfer(vars.transferId, vars.pin),
    onSuccess: (transfer) => {
      putTransfer(transfer);
      void queryClient.invalidateQueries({ queryKey: historyTransferQueryKeys.pending(currentUserId()) });
      void queryClient.invalidateQueries({ queryKey: securityEventQueryKeys.all });
      void transferRunner.kick(transfer.transfer_id);
    },
  });
}

export function useDeclineHistoryTransfer() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (transferId: string) => await declineHistoryTransfer(transferId),
    onSuccess: () => {
      void refreshHistoryTransfers();
    },
  });
}

export function useCancelHistoryTransfer() {
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (transferId: string) => await cancelHistoryTransfer(transferId),
    onSettled: () => {
      void refreshHistoryTransfers();
    },
  });
}
