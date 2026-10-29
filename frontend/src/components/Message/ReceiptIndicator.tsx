/**
 * The delivery / read tick on your own DM messages (#857). Renders nothing
 * unless there is something true to say: a "not delivered" state would reveal
 * whether receipts are off. With several peers it adds a count ("2/4").
 */
import { useTranslation } from "react-i18next";
import { Check, CheckCheck } from "lucide-react";
import type { MessageReceipts } from "../../types";

interface ReceiptIndicatorProps {
  receipts?: MessageReceipts;
  /** OTHER members in this DM; 1 suppresses the count. */
  peerCount: number;
  /** Receipts are shown only to the sender, never in group channels. */
  visible: boolean;
}

export function ReceiptIndicator({ receipts, peerCount, visible }: ReceiptIndicatorProps) {
  const { t } = useTranslation("chat");
  if (!visible || !receipts || peerCount < 1) {
    return null;
  }

  const readCount = receipts.read_by.length;
  const deliveredCount = receipts.delivered_by.length;
  if (deliveredCount === 0) {
    return null;
  }

  // The schema guarantees a reader has also received.
  const allRead = readCount >= peerCount;
  const anyRead = readCount > 0;

  const label = allRead
    ? t("receipts.readByEveryone")
    : anyRead
      ? t("receipts.readByCount", { count: readCount, total: peerCount })
      : t("receipts.deliveredCount", { count: deliveredCount, total: peerCount });

  // Accent only once EVERY peer has read it.
  const tone = allRead ? "text-accent" : "text-muted";

  return (
    <span
      className={`ms-1 inline-flex items-center gap-0.5 align-middle ${tone}`}
      title={label}
      aria-label={label}
      data-testid={`receipt-${receipts.message_id}`}
      data-receipt-state={allRead ? "read-all" : anyRead ? "read-some" : "delivered"}
    >
      {anyRead ? (
        <CheckCheck className="h-3.5 w-3.5" aria-hidden="true" />
      ) : (
        <Check className="h-3.5 w-3.5" aria-hidden="true" />
      )}
      {peerCount > 1 && (
        <span className="font-machine text-xs">
          {anyRead ? readCount : deliveredCount}/{peerCount}
        </span>
      )}
    </span>
  );
}
