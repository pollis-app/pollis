import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { Button } from "../ui/Button";
import {
  useHistoryTransfers,
  usePendingHistoryTransferRequests,
} from "../../hooks/queries/useHistoryTransfers";
import { isTerminal } from "../../utils/historyTransfer";

const sectionHeaderClass =
  "text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line";

/** #1735, on Security beside Devices: start a copy of message history, and get back to any request or copy under way. */
export const HistoryTransferSection: React.FC = () => {
  const { t } = useTranslation("settings");
  const navigate = useNavigate();
  const pending = usePendingHistoryTransferRequests();
  const transfers = useHistoryTransfers();
  const active = (transfers.data ?? []).filter(
    (tr) => !isTerminal(tr.state) && !(tr.role === "source" && tr.state === "awaiting_approval"),
  );
  const unnamed = t("historyTransfer.unnamedDevice");

  return (
    <section className="flex flex-col gap-4 mb-12" data-testid="history-transfer-section">
      <h2 className={sectionHeaderClass}>{t("historyTransfer.heading")}</h2>
      <p className="text-xs text-muted leading-normal">{t("historyTransfer.securityDescription")}</p>

      {(pending.data ?? []).map((request) => (
        <div
          key={request.transfer_id}
          className="flex items-center justify-between gap-3 rounded-lg bg-surface-raised px-4 py-3"
          data-testid={`history-pending-${request.transfer_id}`}
        >
          <span className="min-w-0 text-xs text-fg">
            {t("historyTransfer.approveTitle", { device: request.requester_device_name ?? unnamed })}
          </span>
          <Button
            size="sm"
            onClick={() =>
              navigate({ to: "/security/history-transfer/$transferId", params: { transferId: request.transfer_id } })
            }
          >
            {t("historyTransfer.view")}
          </Button>
        </div>
      ))}

      {active.map((tr) => (
        <div
          key={tr.transfer_id}
          className="flex items-center justify-between gap-3 rounded-lg bg-surface-raised px-4 py-3"
          data-testid={`history-active-${tr.transfer_id}`}
        >
          <span className="min-w-0 flex flex-col text-xs">
            <span className="text-fg">
              {tr.role === "receiver"
                ? t("historyTransfer.activeReceiving", { device: tr.peer_device_name ?? unnamed })
                : t("historyTransfer.activeSending", { device: tr.peer_device_name ?? unnamed })}
            </span>
            <span className="text-dim">
              {tr.role === "receiver"
                ? t("historyTransfer.rowsCopied", { count: tr.rows_done })
                : t("historyTransfer.rowsSent", { count: tr.rows_done })}
            </span>
          </span>
          <Button
            size="sm"
            variant="ghost"
            onClick={() =>
              tr.role === "receiver"
                ? navigate({ to: "/security/copy-history" })
                : navigate({ to: "/security/history-transfer/$transferId", params: { transferId: tr.transfer_id } })
            }
          >
            {t("historyTransfer.view")}
          </Button>
        </div>
      ))}

      <div>
        <Button
          data-testid="copy-history-button"
          variant="secondary"
          activateOn="press"
          onClick={() => navigate({ to: "/security/copy-history" })}
        >
          {t("historyTransfer.copyButton")}
        </Button>
      </div>
    </section>
  );
};
