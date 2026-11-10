import { useState } from "react";
import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Button } from "../ui";
import { ErrorText } from "./SettingsField";
import { PinBusy, PinCells, PinKeypad } from "../auth/PinPad";
import { semantic, type as ty, space } from "../../theme/tokens";
import { activeLocale } from "../../i18n";
import { parseServerTimestamp } from "../../lib/newMember";
import type { HistoryTransfer, PendingTransferRequest } from "../../lib/historyTransfer";
import { useApproveHistoryTransfer, useDeclineHistoryTransfer } from "../../hooks/queries/useHistoryTransfer";

const PIN_LENGTH = 4;

function addedDate(iso: string | null | undefined): string | null {
  const ms = parseServerTimestamp(iso ?? null);
  if (ms === null) {
    return null;
  }
  return new Date(ms).toLocaleDateString(activeLocale(), { year: "numeric", month: "short", day: "numeric" });
}

/**
 * The source side's choice (#1735), shared by the approval page and link-device's "Linked" page.
 * With `pin_required` the PIN keypad is the Approve control (4 digits submit); otherwise one tap.
 */
export function TransferApproval({
  request,
  onApproved,
  onDeclined,
}: {
  request: PendingTransferRequest;
  onApproved: (transfer: HistoryTransfer) => void;
  onDeclined: () => void;
}) {
  const { t } = useTranslation("mobile");
  const approve = useApproveHistoryTransfer();
  const decline = useDeclineHistoryTransfer();
  const [pin, setPin] = useState("");
  // The one-tap link shortcut can lapse (10 min, a lock, one use) while this page is open:
  // then the PIN is asked for in place, never a dead end.
  const [needPin, setNeedPin] = useState(request.pin_required);
  const [error, setError] = useState<string | null>(null);
  const busy = approve.isPending || decline.isPending;
  const device = request.requester_device_name?.trim() || t("historyTransfer.approve.unknownDevice");
  const date = addedDate(request.requester_added_at);

  const onApprove = (value?: string) => {
    if (busy) {
      return;
    }
    setError(null);
    approve.mutate(
      { transferId: request.transfer_id, pin: value },
      {
        onSuccess: (transfer) => onApproved(transfer),
        onError: (e) => {
          setPin("");
          setError((e as Error).message || t("historyTransfer.status.actionFailed"));
          if (!needPin) {
            setNeedPin(true);
          }
        },
      },
    );
  };

  const onDecline = () => {
    if (busy) {
      return;
    }
    setError(null);
    decline.mutate(request.transfer_id, {
      onSuccess: () => onDeclined(),
      onError: (e) => setError((e as Error).message || t("historyTransfer.status.actionFailed")),
    });
  };

  const pushDigit = (d: string) => {
    if (busy || pin.length >= PIN_LENGTH) {
      return;
    }
    const next = pin + d;
    setPin(next);
    if (next.length === PIN_LENGTH) {
      onApprove(next);
    }
  };

  return (
    <View style={{ gap: space.xl }} testID="history-transfer-approval">
      <Text testID="history-transfer-approval-body" style={ty.body}>
        {date
          ? t("historyTransfer.approve.body", { device, date })
          : t("historyTransfer.approve.bodyNoDate", { device })}
      </Text>
      {needPin ? (
        <View style={{ gap: space.lg }}>
          <Text style={[ty.body, { color: semantic.dim, textAlign: "center" }]}>
            {t("historyTransfer.approve.pinPrompt")}
          </Text>
          {approve.isPending ? null : <PinCells length={pin.length} />}
          {error ? (
            <View style={{ alignSelf: "center" }}>
              <ErrorText testID="history-transfer-approval-error">{error}</ErrorText>
            </View>
          ) : null}
          {approve.isPending ? (
            <PinBusy label={t("historyTransfer.approve.approving")} />
          ) : (
            <PinKeypad onDigit={pushDigit} onBackspace={() => setPin(pin.slice(0, -1))} disabled={busy} />
          )}
          <Button
            testID="btn-history-decline"
            variant="subtle"
            full
            disabled={approve.isPending}
            loading={decline.isPending}
            onPress={onDecline}
          >
            {decline.isPending ? t("historyTransfer.approve.declining") : t("historyTransfer.approve.decline")}
          </Button>
        </View>
      ) : (
        <View style={{ gap: space.sm }}>
          {error ? <ErrorText testID="history-transfer-approval-error">{error}</ErrorText> : null}
          <Button
            testID="btn-history-approve"
            variant="primary"
            full
            disabled={decline.isPending}
            loading={approve.isPending}
            onPress={() => onApprove()}
          >
            {approve.isPending ? t("historyTransfer.approve.approving") : t("historyTransfer.approve.approve")}
          </Button>
          <Button
            testID="btn-history-decline"
            full
            disabled={approve.isPending}
            loading={decline.isPending}
            onPress={onDecline}
          >
            {decline.isPending ? t("historyTransfer.approve.declining") : t("historyTransfer.approve.decline")}
          </Button>
        </View>
      )}
    </View>
  );
}
