import { useState } from "react";
import { ActivityIndicator, ScrollView } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Header, Button, BottomAction } from "../components/ui";
import { Note } from "../components/self/SettingsField";
import { TransferApproval } from "../components/self/TransferApproval";
import { HistoryTransferStatus } from "../components/self/HistoryTransferStatus";
import { semantic, space } from "../theme/tokens";
import { useHistoryTransfers, usePendingTransferRequests } from "../hooks/queries";
import { useThemeRerender } from "../components/theme";

/**
 * Another of this account's devices asks for this one's message history (#1735). Opened like approve-device:
 * from the nudge, on landing and on every foreground return. Approve, then follow the send with Cancel.
 */
export default function ApproveHistoryTransfer() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const { transferId } = useLocalSearchParams<{ transferId?: string }>();
  const pending = usePendingTransferRequests();
  const { data: transfers = [] } = useHistoryTransfers();
  const [answered, setAnswered] = useState<"approved" | "declined" | null>(null);

  const request = (pending.data ?? []).find((r) => r.transfer_id === transferId) ?? null;
  const sending = transfers.find((tr) => tr.transfer_id === transferId && tr.role === "source") ?? null;
  const close = () => {
    if (router.canGoBack()) {
      router.back();
    } else {
      router.replace("/(tabs)/groups");
    }
  };

  const showStatus = !!sending && (answered === "approved" || !request);
  const showRequest = !showStatus && !!request && answered === null;
  const loading = !showStatus && !showRequest && (pending.isPending || answered === "approved");

  const showClose = !request && !sending && !pending.isPending && answered !== "approved";

  // The PIN keypad is the only input: never avoid a keyboard (#1642).
  return (
    <Screen testID="screen-approve-history-transfer" avoidKeyboard={false}>
      <Header title={t("historyTransfer.approve.title")} />
      <ScrollView
        style={{ flex: 1 }}
        contentContainerStyle={{
          paddingHorizontal: space.xxl,
          paddingTop: space.xl,
          paddingBottom: space.xxxl,
          gap: space.lg,
        }}
      >
        {showStatus && sending ? <HistoryTransferStatus transfer={sending} onDone={close} /> : null}
        {showRequest && request ? (
          <TransferApproval
            request={request}
            onApproved={() => setAnswered("approved")}
            onDeclined={() => {
              setAnswered("declined");
              close();
            }}
          />
        ) : null}
        {loading ? <ActivityIndicator color={semantic.accent} /> : null}
        {!showStatus && !showRequest && !loading ? (
          <Note testID="history-transfer-approval-gone">{t("historyTransfer.approve.gone")}</Note>
        ) : null}
      </ScrollView>
      {showClose ? (
        <BottomAction>
          <Button testID="btn-history-approval-close" variant="primary" full onPress={close}>
            {t("historyTransfer.status.done")}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}
