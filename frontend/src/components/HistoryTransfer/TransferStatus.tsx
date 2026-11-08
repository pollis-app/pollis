import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "../ui/Button";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import * as api from "../../services/api";
import { historyTransferStore } from "../../stores/historyTransferStore";
import { retryKind, transferPhase, isTerminal } from "../../utils/historyTransfer";
import { errorMessage } from "../../utils/errorMessage";
import {
  requestTransfer,
  runTransfer,
  storeTransfer,
} from "../../hooks/queries/useHistoryTransfers";

interface TransferStatusProps {
  transfer: api.HistoryTransfer;
  /** Shown on a finished transfer; omit to show none. */
  onDone?: () => void;
  /** A retry that asked again starts a new transfer; the screen follows it. */
  onRetried?: (next: api.HistoryTransfer) => void;
}

/** One history transfer, either side: waiting, progress, paused, and every way it can end, with cancel and retry. */
export const TransferStatus: React.FC<TransferStatusProps> = observer(({ transfer, onDone, onRetried }) => {
  const { t } = useTranslation("settings");
  const queryClient = useQueryClient();
  const running = historyTransferStore.isRunning(transfer.transfer_id);
  const [busy, setBusy] = useState<"cancel" | "retry" | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const device = transfer.peer_device_name ?? t("historyTransfer.unnamedDevice");
  const receiving = transfer.role === "receiver";
  let phase = transferPhase(transfer);
  // Nothing is driving a transfer that is still mid-way (a run that threw): it waits like a pause.
  if (!running && (phase === "transferring" || (phase === "waiting" && receiving))) {
    phase = "paused";
  }
  const retry = retryKind({ ...transfer, state: phase === "paused" ? "paused" : transfer.state }, running);

  const cancel = async () => {
    setBusy("cancel");
    setActionError(null);
    try {
      await api.cancelHistoryTransfer(transfer.transfer_id);
      storeTransfer(queryClient, { ...transfer, state: "cancelled" });
    } catch (err) {
      setActionError(errorMessage(err, t("historyTransfer.cancelFailed")));
    } finally {
      setBusy(null);
    }
  };

  const doRetry = async () => {
    setBusy("retry");
    setActionError(null);
    try {
      if (retry === "resume") {
        void runTransfer(queryClient, transfer.transfer_id);
      } else if (retry === "request") {
        const next = await requestTransfer(queryClient, transfer.peer_device_id, transfer.via);
        onRetried?.(next);
      }
    } catch (err) {
      setActionError(errorMessage(err, t("historyTransfer.requestFailed")));
    } finally {
      setBusy(null);
    }
  };

  let title: string;
  let body: string | null = null;
  let tone: "normal" | "danger" = "normal";
  switch (phase) {
    case "waiting":
      title = t("historyTransfer.waitingTitle", { device });
      body = t("historyTransfer.waitingBody");
      break;
    case "transferring":
      title = receiving
        ? t("historyTransfer.receivingTitle", { device })
        : t("historyTransfer.sendingTitle", { device });
      break;
    case "paused":
      title = t("historyTransfer.pausedTitle", { device });
      body = t("historyTransfer.pausedBody");
      break;
    case "done":
    case "deferred":
      title = receiving
        ? t("historyTransfer.doneTitle", { count: transfer.rows_done })
        : t("historyTransfer.sentTitle", { count: transfer.rows_done, device });
      if (phase === "deferred" && receiving) {
        body = t("historyTransfer.deferred", { count: transfer.deferred_conversations });
      }
      break;
    case "declined":
      title = t("historyTransfer.declinedTitle", { device });
      body = t("historyTransfer.declinedBody");
      break;
    case "cancelled":
      title = t("historyTransfer.cancelledTitle");
      break;
    case "expired":
      title = t("historyTransfer.expiredTitle", { device });
      body = t("historyTransfer.expiredBody", { device });
      break;
    default:
      title = receiving ? t("historyTransfer.failedTitle") : t("historyTransfer.sendFailedTitle");
      body = transfer.error ?? null;
      tone = "danger";
      break;
  }

  const showCounts = phase === "transferring" || phase === "paused" || (phase === "failed" && transfer.rows_done > 0);
  const total = transfer.estimated_total ?? null;
  const percent = total && total > 0 ? Math.min(100, Math.round((transfer.rows_done / total) * 100)) : null;
  const finished = isTerminal(transfer.state);

  return (
    <div
      className="flex flex-col gap-5"
      data-testid="history-transfer-status"
      data-state={transfer.state}
      data-phase={phase}
    >
      <div className="flex flex-col gap-2">
        <h2 className={`text-lg ${tone === "danger" ? "text-danger" : "text-fg"}`} data-testid="history-transfer-title">
          {title}
        </h2>
        {body && (
          <p className="text-sm text-muted leading-relaxed" data-testid="history-transfer-body">
            {body}
          </p>
        )}
      </div>

      {(phase === "waiting" || (phase === "transferring" && percent === null)) && <LoadingSpinner size="sm" />}

      {phase === "transferring" && percent !== null && (
        <div
          role="progressbar"
          aria-label={t("historyTransfer.progressAriaLabel")}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent}
          className="w-full h-1 rounded-sm overflow-hidden bg-line"
        >
          <div className="h-full bg-accent transition-[width] duration-200" style={{ width: `${percent}%` }} />
        </div>
      )}

      {showCounts && (
        <p className="text-sm font-mono text-dim" data-testid="history-transfer-counts">
          {receiving
            ? t("historyTransfer.rowsCopied", { count: transfer.rows_done })
            : t("historyTransfer.rowsSent", { count: transfer.rows_done })}
          {" · "}
          {t("historyTransfer.chunks", { count: transfer.chunks_done })}
        </p>
      )}

      {actionError && (
        <p className="text-sm text-danger" data-testid="history-transfer-action-error">
          {actionError}
        </p>
      )}

      <div className="flex flex-wrap gap-2">
        {retry && (
          <Button data-testid="history-transfer-retry" isLoading={busy === "retry"} disabled={busy !== null} onClick={doRetry}>
            {retry === "resume" ? t("historyTransfer.resume") : t("historyTransfer.tryAgain")}
          </Button>
        )}
        {finished && onDone && (
          <Button
            data-testid="history-transfer-done"
            variant={retry ? "ghost" : "primary"}
            disabled={busy !== null}
            onClick={onDone}
          >
            {t("historyTransfer.done")}
          </Button>
        )}
        {!finished && (
          <Button
            data-testid="history-transfer-cancel"
            variant="ghost"
            isLoading={busy === "cancel"}
            disabled={busy !== null}
            onClick={cancel}
          >
            {t("historyTransfer.cancel")}
          </Button>
        )}
      </div>
    </div>
  );
});
