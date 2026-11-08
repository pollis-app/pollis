import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { TitleBar } from "../Layout/TitleBar";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import { SourcePicker, defaultSource } from "./SourcePicker";
import { TransferStatus } from "./TransferStatus";
import * as api from "../../services/api";
import { linkOfferSource, isTerminal } from "../../utils/historyTransfer";
import { errorMessage } from "../../utils/errorMessage";
import { requestTransfer, useHistoryTransfers } from "../../hooks/queries/useHistoryTransfers";

interface HistoryOfferScreenProps {
  sources: api.TransferSource[];
  onContinue: () => void;
}

/**
 * #1735: right after this device is linked, before the app opens: offer to copy the message history from
 * the device that approved it. Copying asks that device; the user can go on into the app at any point.
 */
export const HistoryOfferScreen: React.FC<HistoryOfferScreenProps> = ({ sources, onContinue }) => {
  const { t } = useTranslation("settings");
  const queryClient = useQueryClient();
  const named = linkOfferSource(sources);
  const [picked, setPicked] = useState<string | null>(() => named?.device_id ?? defaultSource(sources));
  const [requesting, setRequesting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [transferId, setTransferId] = useState<string | null>(null);
  const transfers = useHistoryTransfers(transferId !== null);
  const transfer = transfers.data?.find((tr) => tr.transfer_id === transferId) ?? null;

  const copy = async () => {
    if (!picked || requesting) {
      return;
    }
    setRequesting(true);
    setError(null);
    try {
      const next = await requestTransfer(queryClient, picked, "link");
      setTransferId(next.transfer_id);
    } catch (err) {
      setError(errorMessage(err, t("historyTransfer.requestFailed")));
    } finally {
      setRequesting(false);
    }
  };

  let content: React.ReactNode;
  if (transfer) {
    const finished = isTerminal(transfer.state);
    content = (
      <>
        <TransferStatus transfer={transfer} onRetried={(next) => setTransferId(next.transfer_id)} />
        {!finished && <p className="text-xs font-mono text-muted leading-relaxed">{t("historyTransfer.continueHint")}</p>}
        <div>
          <Button data-testid="history-offer-continue" variant={finished ? "primary" : "secondary"} onClick={onContinue}>
            {t("historyTransfer.continue")}
          </Button>
        </div>
      </>
    );
  } else {
    content = (
      <>
        <div className="flex flex-col gap-2">
          <h2 className="text-lg text-fg" data-testid="history-offer-title">
            {named
              ? t("historyTransfer.offerTitle", { device: named.device_name ?? t("historyTransfer.unnamedDevice") })
              : t("historyTransfer.offerTitlePick")}
          </h2>
          <p className="text-sm text-muted leading-relaxed">{t("historyTransfer.offerSubtitle")}</p>
        </div>
        {!named && <SourcePicker sources={sources} value={picked} onChange={setPicked} />}
        {error && (
          <p className="text-sm text-danger" data-testid="history-offer-error">
            {error}
          </p>
        )}
        <div className="flex gap-2">
          <Button data-testid="history-offer-copy" isLoading={requesting} disabled={!picked} onClick={copy}>
            {t("historyTransfer.offerCopy")}
          </Button>
          <Button data-testid="history-offer-not-now" variant="ghost" disabled={requesting} onClick={onContinue}>
            {t("historyTransfer.offerNotNow")}
          </Button>
        </div>
      </>
    );
  }

  return (
    <div data-testid="history-offer-screen" className="flex flex-col h-full w-full bg-bg">
      <TitleBar />
      <div className="flex-1 flex justify-center overflow-y-auto px-4">
        <Card padding="lg" className="my-auto w-full max-w-md">
          <div className="flex flex-col gap-6">{content}</div>
        </Card>
      </div>
    </div>
  );
};
