import { useState } from "react";
import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Button } from "../ui";
import { Heading } from "../auth/Heading";
import { ProgressBar } from "../auth/ProgressBar";
import { ErrorText } from "./SettingsField";
import { semantic, type as ty, space } from "../../theme/tokens";
import {
  canCancel,
  progressPercent,
  retryKind,
  transferPhase,
  type HistoryTransfer,
} from "../../lib/historyTransfer";
import {
  transferRunner,
  useCancelHistoryTransfer,
  useRequestHistoryTransfer,
} from "../../hooks/queries/useHistoryTransfer";

/** One transfer's state on either side (#1735): waiting, progress, paused, done/deferred, or ended, with its one action. */
export function HistoryTransferStatus({
  transfer,
  onDone,
  doneLabel,
}: {
  transfer: HistoryTransfer;
  // A finished transfer's primary button; omitted, it shows none.
  onDone?: () => void;
  doneLabel?: string;
}) {
  const { t } = useTranslation("mobile");
  const cancel = useCancelHistoryTransfer();
  const request = useRequestHistoryTransfer();
  const [resuming, setResuming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const phase = transferPhase(transfer);
  const device = transfer.peer_device_name?.trim() || t("historyTransfer.approve.unknownDevice");
  const receiver = transfer.role === "receiver";
  const rows = transfer.rows_done;
  const percent = progressPercent(transfer);

  let title: string;
  let body: string | null = null;
  switch (phase) {
    case "waiting":
      title = receiver
        ? t("historyTransfer.status.waitingTitle", { device })
        : t("historyTransfer.status.waitingSourceTitle", { device });
      body = receiver
        ? t("historyTransfer.status.waitingBody", { device })
        : t("historyTransfer.status.waitingSourceBody", { device });
      break;
    case "transferring":
      title = receiver ? t("historyTransfer.status.receivingTitle") : t("historyTransfer.status.sendingTitle");
      body = receiver
        ? t("historyTransfer.status.receivingBody", { device })
        : t("historyTransfer.status.sendingBody", { device });
      break;
    case "paused":
      title = t("historyTransfer.status.pausedTitle");
      body = t("historyTransfer.status.pausedBody", { device });
      break;
    case "done":
    case "deferred":
      title = receiver ? t("historyTransfer.status.doneTitle") : t("historyTransfer.status.sentTitle");
      body = receiver
        ? t("historyTransfer.status.doneBody", { count: rows })
        : t("historyTransfer.status.sentBody", { count: rows, device });
      break;
    case "declined":
      title = t("historyTransfer.status.declinedTitle", { device });
      body = t("historyTransfer.status.declinedBody");
      break;
    case "cancelled":
      title = t("historyTransfer.status.cancelledTitle");
      body = t("historyTransfer.status.cancelledBody");
      break;
    case "expired":
      title = t("historyTransfer.status.expiredTitle");
      body = t("historyTransfer.status.expiredBody", { device });
      break;
    default:
      title = t("historyTransfer.status.failedTitle");
      body = t("historyTransfer.status.failedBody");
      break;
  }

  const active = phase === "transferring" || phase === "paused";
  const retry = retryKind(transfer);
  const finished = !canCancel(transfer);

  const onCancel = () => {
    if (cancel.isPending) {
      return;
    }
    setError(null);
    cancel.mutate(transfer.transfer_id, {
      onError: (e) => setError((e as Error).message || t("historyTransfer.status.actionFailed")),
    });
  };

  const onRetry = () => {
    setError(null);
    if (retry === "resume") {
      if (resuming) {
        return;
      }
      setResuming(true);
      void transferRunner.kick(transfer.transfer_id).finally(() => setResuming(false));
      return;
    }
    if (retry === "request" && !request.isPending) {
      request.mutate(
        { sourceDeviceId: transfer.peer_device_id, via: "sibling" },
        { onError: (e) => setError((e as Error).message || t("historyTransfer.picker.requestFailed")) },
      );
    }
  };

  return (
    <View style={{ gap: space.xl }} testID={`history-transfer-${phase}`}>
      <Heading testID="history-transfer-status" title={title} subtitle={body ?? undefined} />
      {phase === "waiting" || active ? (
        <View style={{ gap: space.sm }}>
          {active ? (
            <Text testID="history-transfer-rows" accessibilityLiveRegion="polite" style={ty.section}>
              {t("historyTransfer.status.rows", { count: rows })}
              {transfer.chunks_done > 0
                ? ` · ${t("historyTransfer.status.chunks", { count: transfer.chunks_done })}`
                : ""}
            </Text>
          ) : null}
          <ProgressBar
            percent={percent ?? 0}
            indeterminate={percent === null && phase !== "paused"}
            label={title}
          />
        </View>
      ) : null}
      {phase === "deferred" ? (
        <Text testID="history-transfer-deferred" style={[ty.body, { color: semantic.dim }]}>
          {t("historyTransfer.status.deferred", { count: transfer.deferred_conversations })}
        </Text>
      ) : null}
      {phase === "failed" && transfer.error ? (
        <Text style={[ty.meta, { color: semantic.dim }]}>{transfer.error}</Text>
      ) : null}
      {error ? <ErrorText testID="history-transfer-error">{error}</ErrorText> : null}
      <View style={{ gap: space.sm }}>
        {retry ? (
          <Button
            testID={retry === "resume" ? "btn-history-resume" : "btn-history-retry"}
            variant="primary"
            full
            loading={resuming || request.isPending}
            onPress={onRetry}
          >
            {retry === "resume" ? t("historyTransfer.status.resume") : t("historyTransfer.status.retry")}
          </Button>
        ) : null}
        {finished && onDone ? (
          <Button testID="btn-history-done" variant={retry ? "secondary" : "primary"} full onPress={onDone}>
            {doneLabel ?? t("historyTransfer.status.done")}
          </Button>
        ) : null}
        {!finished ? (
          <Button testID="btn-history-cancel" full loading={cancel.isPending} onPress={onCancel}>
            {cancel.isPending ? t("historyTransfer.status.cancelling") : t("historyTransfer.status.cancel")}
          </Button>
        ) : null}
      </View>
    </View>
  );
}
