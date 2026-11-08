import { useEffect, useState } from "react";
import { View } from "react-native";
import { claimInline } from "../../lib/historyTransferPrompt";
import { space } from "../../theme/tokens";
import { useHistoryTransfers, usePendingTransferRequests } from "../../hooks/queries/useHistoryTransfer";
import { TransferApproval } from "./TransferApproval";
import { HistoryTransferStatus } from "./HistoryTransferStatus";

/**
 * Link-device's "Linked" page (#1735): the device just linked usually asks for this one's message history
 * moments later. Its request is answered here (single tap, the link-time shortcut) instead of opening a page.
 */
export function InlineTransferApproval({ deviceId }: { deviceId: string }) {
  const pending = usePendingTransferRequests();
  const { data: transfers = [] } = useHistoryTransfers();
  const [approvedId, setApprovedId] = useState<string | null>(null);
  const [declined, setDeclined] = useState(false);

  useEffect(() => claimInline(deviceId), [deviceId]);

  if (declined) {
    return null;
  }
  const sending = approvedId
    ? transfers.find((tr) => tr.transfer_id === approvedId && tr.role === "source") ?? null
    : null;
  if (sending) {
    return (
      <View style={{ gap: space.xl }}>
        <HistoryTransferStatus transfer={sending} />
      </View>
    );
  }
  const request = (pending.data ?? []).find((r) => r.requester_device_id === deviceId) ?? null;
  if (!request || approvedId) {
    return null;
  }
  return (
    <TransferApproval
      request={request}
      onApproved={(transfer) => setApprovedId(transfer.transfer_id)}
      onDeclined={() => setDeclined(true)}
    />
  );
}
