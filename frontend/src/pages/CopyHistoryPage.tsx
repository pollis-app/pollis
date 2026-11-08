import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { LoadingSpinner } from "../components/ui/LoaderSpinner";
import { SourcePicker, defaultSource } from "../components/HistoryTransfer/SourcePicker";
import { TransferStatus } from "../components/HistoryTransfer/TransferStatus";
import {
  requestTransfer,
  useHistoryTransferSources,
  useHistoryTransfers,
} from "../hooks/queries/useHistoryTransfers";
import { currentReceiverTransfer, isSupported } from "../utils/historyTransfer";
import { errorMessage } from "../utils/errorMessage";

/** #1735: copy message history to this device from another of the account's devices, which must approve it. */
export const CopyHistoryPage: React.FC = () => {
  const { t } = useTranslation("settings");
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const sources = useHistoryTransferSources();
  const transfers = useHistoryTransfers();
  // The transfer this page is showing; null shows the picker.
  const [viewing, setViewing] = useState<string | null>(null);
  const [picked, setPicked] = useState<string | null>(null);
  const [requesting, setRequesting] = useState(false);
  const [requestError, setRequestError] = useState<string | null>(null);
  const [adopted, setAdopted] = useState(false);

  // Opening the page while a copy is under way shows that copy.
  useEffect(() => {
    if (adopted || !transfers.data) {
      return;
    }
    setAdopted(true);
    const active = currentReceiverTransfer(transfers.data);
    if (active) {
      setViewing(active.transfer_id);
    }
  }, [adopted, transfers.data]);

  useEffect(() => {
    if (picked === null && sources.data) {
      setPicked(defaultSource(sources.data));
    }
  }, [picked, sources.data]);

  const shown = viewing ? transfers.data?.find((tr) => tr.transfer_id === viewing) ?? null : null;
  const pickedSource = sources.data?.find((s) => s.device_id === picked) ?? null;

  const start = async () => {
    if (!pickedSource || !isSupported(pickedSource) || requesting) {
      return;
    }
    setRequesting(true);
    setRequestError(null);
    try {
      const transfer = await requestTransfer(queryClient, pickedSource.device_id, "sibling");
      setViewing(transfer.transfer_id);
    } catch (err) {
      setRequestError(errorMessage(err, t("historyTransfer.requestFailed")));
    } finally {
      setRequesting(false);
    }
  };

  let body: React.ReactNode;
  if (shown) {
    body = (
      <TransferStatus
        transfer={shown}
        onDone={() => setViewing(null)}
        onRetried={(next) => setViewing(next.transfer_id)}
      />
    );
  } else if (sources.isLoading || (viewing !== null && transfers.isLoading)) {
    body = <LoadingSpinner size="sm" />;
  } else {
    const list = sources.data ?? [];
    body = (
      <>
        <div className="flex flex-col gap-3">
          <h1 className="text-xl text-fg">{t("historyTransfer.pickTitle")}</h1>
          <p className="text-sm text-muted leading-relaxed">{t("historyTransfer.pickSubtitle")}</p>
        </div>
        {sources.isError && (
          <p className="text-sm text-danger" data-testid="history-sources-error">
            {errorMessage(sources.error, t("historyTransfer.sourcesFailed"))}
          </p>
        )}
        {!sources.isError && list.length === 0 && (
          <p className="text-sm text-dim" data-testid="history-sources-empty">
            {t("historyTransfer.noSources")}
          </p>
        )}
        {list.length > 0 && <SourcePicker sources={list} value={picked} onChange={setPicked} />}
        {requestError && (
          <p className="text-sm text-danger" data-testid="history-request-error">
            {requestError}
          </p>
        )}
        <div className="flex gap-2">
          <Button
            data-testid="history-request-start"
            isLoading={requesting}
            disabled={!pickedSource || !isSupported(pickedSource)}
            onClick={start}
          >
            {t("historyTransfer.start")}
          </Button>
          <Button variant="ghost" onClick={() => navigate({ to: "/security" })}>
            {t("common:actions.cancel")}
          </Button>
        </div>
      </>
    );
  }

  return (
    <PageShell title={t("historyTransfer.pageTitle")} scrollable>
      <div className="flex justify-center px-6 py-12">
        <div className="flex flex-col gap-8 w-full max-w-md" data-testid="copy-history-page">
          {body}
        </div>
      </div>
    </PageShell>
  );
};
