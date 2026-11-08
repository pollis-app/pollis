import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
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
  const navigate = useNavigate();
  const { transferId } = useParams({ from: "/security/history-transfer/$transferId" });
  const pending = usePendingHistoryTransferRequests();
  const transfers = useHistoryTransfers();
  const [declined, setDeclined] = useState(false);

  const request = pending.data?.find((r) => r.transfer_id === transferId) ?? null;
  const transfer = transfers.data?.find((tr) => tr.transfer_id === transferId) ?? null;
  const leave = () => navigate({ to: "/security" });

  let body: React.ReactNode;
  if (declined) {
    body = (
      <>
        <p className="text-lg text-fg" data-testid="history-approval-declined">
          {t("historyTransfer.youDeclined")}
        </p>
        <div>
          <Button onClick={leave}>{t("historyTransfer.done")}</Button>
        </div>
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
        <p className="text-lg text-fg" data-testid="history-approval-gone">
          {t("historyTransfer.notOpen")}
        </p>
        <div>
          <Button onClick={leave}>{t("historyTransfer.back")}</Button>
        </div>
      </>
    );
  }

  return (
    <PageShell title={t("historyTransfer.heading")} scrollable>
      <div className="flex justify-center px-6 py-12">
        <div className="flex flex-col gap-8 w-full max-w-md" data-testid="history-approval-page">
          {body}
        </div>
      </div>
    </PageShell>
  );
};
