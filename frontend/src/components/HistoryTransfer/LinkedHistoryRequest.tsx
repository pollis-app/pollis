import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { HistoryApprovalCard } from "./HistoryApprovalCard";
import { TransferStatus } from "./TransferStatus";
import {
  useHistoryTransfers,
  usePendingHistoryTransferRequests,
} from "../../hooks/queries/useHistoryTransfers";

interface LinkedHistoryRequestProps {
  /** The device just linked; null when the link did not say. */
  deviceId: string | null;
  deviceName: string;
}

/**
 * #1735: on the "Linked" result, the new device's request for this device's history, answered inline
 * when it arrives (the realtime nudge refreshes the pending list), then its progress.
 */
export const LinkedHistoryRequest: React.FC<LinkedHistoryRequestProps> = ({ deviceId, deviceName }) => {
  const { t } = useTranslation("settings");
  const pending = usePendingHistoryTransferRequests();
  const transfers = useHistoryTransfers();
  const [approvedId, setApprovedId] = useState<string | null>(null);
  const [declined, setDeclined] = useState(false);

  if (declined) {
    return (
      <p className="text-sm text-muted" data-testid="linked-history-declined">
        {t("historyTransfer.youDeclined")}
      </p>
    );
  }
  const approved = approvedId ? transfers.data?.find((tr) => tr.transfer_id === approvedId) ?? null : null;
  if (approved) {
    return <TransferStatus transfer={approved} />;
  }
  const request =
    (pending.data ?? []).find((r) =>
      deviceId ? r.requester_device_id === deviceId : r.via === "link",
    ) ?? null;
  if (!request) {
    return null;
  }
  return (
    <div data-testid="linked-history-request">
      <HistoryApprovalCard
        request={request}
        deviceName={deviceName}
        onApproved={(transfer) => setApprovedId(transfer.transfer_id)}
        onDeclined={() => setDeclined(true)}
      />
    </div>
  );
};
