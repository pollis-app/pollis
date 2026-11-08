import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { Button } from "../ui/Button";
import { InputOtp } from "../ui/InputOtp";
import * as api from "../../services/api";
import { errorMessage } from "../../utils/errorMessage";
import { formatDateTime } from "../../utils/format";
import {
  historyTransferKeys,
  runTransfer,
  storeTransfer,
} from "../../hooks/queries/useHistoryTransfers";

interface HistoryApprovalCardProps {
  request: api.PendingTransferRequest;
  /** Fallback when the request carries no device name (the link page knows it). */
  deviceName?: string;
  onApproved: (transfer: api.HistoryTransfer) => void;
  onDeclined: () => void;
}

/** The source device's answer to a history request: who is asking, what it means, the PIN when asked for, Approve / Decline. */
export const HistoryApprovalCard: React.FC<HistoryApprovalCardProps> = ({
  request,
  deviceName,
  onApproved,
  onDeclined,
}) => {
  const { t } = useTranslation("settings");
  const queryClient = useQueryClient();
  const [pin, setPin] = useState("");
  const [busy, setBusy] = useState<"approve" | "decline" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const device = request.requester_device_name ?? deviceName ?? t("historyTransfer.unnamedDevice");
  const pinReady = !request.pin_required || pin.length === 4;

  const approve = async () => {
    if (busy || !pinReady) {
      return;
    }
    setBusy("approve");
    setError(null);
    try {
      const transfer = await api.approveHistoryTransfer(request.transfer_id, request.pin_required ? pin : null);
      storeTransfer(queryClient, transfer);
      queryClient.setQueryData<api.PendingTransferRequest[]>(historyTransferKeys.pending, (list) =>
        (list ?? []).filter((r) => r.transfer_id !== request.transfer_id),
      );
      void runTransfer(queryClient, transfer.transfer_id);
      onApproved(transfer);
    } catch (err) {
      setError(errorMessage(err, t("historyTransfer.approveFailed")));
      setPin("");
    } finally {
      setBusy(null);
    }
  };

  const decline = async () => {
    if (busy) {
      return;
    }
    setBusy("decline");
    setError(null);
    try {
      await api.declineHistoryTransfer(request.transfer_id);
      queryClient.setQueryData<api.PendingTransferRequest[]>(historyTransferKeys.pending, (list) =>
        (list ?? []).filter((r) => r.transfer_id !== request.transfer_id),
      );
      onDeclined();
    } catch (err) {
      setError(errorMessage(err, t("historyTransfer.declineFailed")));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex flex-col gap-5" data-testid="history-approval-card">
      <div className="flex flex-col gap-2">
        <h2 className="text-lg text-fg" data-testid="history-approval-title">
          {t("historyTransfer.approveTitle", { device })}
        </h2>
        {request.requester_added_at && (
          <p className="text-xs font-mono text-dim" data-testid="history-approval-added">
            {t("historyTransfer.approveAdded", { time: formatDateTime(request.requester_added_at) })}
          </p>
        )}
        <p className="text-sm text-muted leading-relaxed">{t("historyTransfer.approveWarning")}</p>
      </div>

      {request.pin_required && (
        <div className="flex flex-col gap-3" data-testid="history-approval-pin">
          <p className="text-sm text-fg">{t("historyTransfer.approvePin")}</p>
          <InputOtp length={4} value={pin} onChange={setPin} disabled={busy !== null} mask autoFocus />
        </div>
      )}

      {error && (
        <p className="text-sm text-danger" data-testid="history-approval-error">
          {error}
        </p>
      )}

      <div className="flex gap-2">
        <Button
          data-testid="history-approval-approve"
          isLoading={busy === "approve"}
          disabled={busy !== null || !pinReady}
          onClick={approve}
        >
          {t("historyTransfer.approve")}
        </Button>
        <Button
          data-testid="history-approval-decline"
          variant="danger"
          isLoading={busy === "decline"}
          disabled={busy !== null}
          onClick={decline}
        >
          {t("historyTransfer.decline")}
        </Button>
      </div>
    </div>
  );
};
