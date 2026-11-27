import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { PageColumn } from "../components/ui/PageColumn";
import { FormActions } from "../components/ui/FormActions";
import { LoadingSpinner } from "../components/ui/LoaderSpinner";
import { HistoryApprovalCard } from "../components/HistoryTransfer/HistoryApprovalCard";
import { TransferStatus } from "../components/HistoryTransfer/TransferStatus";
import {
  useHistoryTransfers,
  usePendingHistoryTransferRequests,
} from "../hooks/queries/useHistoryTransfers";

/** #1735: the source device's page for one history request: approve (with the PIN when asked), then its progress. */
export const HistoryTransferApprovalPage: React.FC = () => {
  const { t } = useTranslation("settings");
  const { finish } = useFinishFlow();
  const { transferId } = useParams({ from: "/security/history-transfer/$transferId" });
  const pending = usePendingHistoryTransferRequests();
  const transfers = useHistoryTransfers();
  const [declined, setDeclined] = useState(false);

  const request = pending.data?.find((r) => r.transfer_id === transferId) ?? null;
  const transfer = transfers.data?.find((tr) => tr.transfer_id === transferId) ?? null;
  const leave = () => finish({ to: "/security" });

  let body: React.ReactNode;
  if (declined) {
    body = (
      <>
        <p className="text-heading text-fg" data-testid="history-approval-declined">
          {t("historyTransfer.youDeclined")}
        </p>
        <FormActions>
          <Button onClick={leave}>{t("historyTransfer.done")}</Button>
        </FormActions>
      </>
    );
  } else if (request) {
    body = (
      <HistoryApprovalCard
        request={request}
        onApproved={() => undefined}
        onDeclined={() => setDeclined(true)}
      />
    );
  } else if (transfer && transfer.state !== "awaiting_approval") {
    body = <TransferStatus transfer={transfer} onDone={leave} />;
  } else if (pending.isLoading || transfers.isLoading) {
    body = <LoadingSpinner size="sm" />;
  } else {
    body = (
      <>
        <p className="text-heading text-fg" data-testid="history-approval-gone">
          {t("historyTransfer.notOpen")}
        </p>
        <FormActions>
          <Button onClick={leave}>{t("historyTransfer.back")}</Button>
        </FormActions>
      </>
    );
  }

  return (
    <PageShell title={t("historyTransfer.heading")} scrollable>
      <PageColumn spacing="sections" data-testid="history-approval-page">
        {body}
      </PageColumn>
    </PageShell>
  );
};
