/**
 * The delivery / read tick on your own DM messages (#857).
 *
 * Renders nothing at all unless there is something true to say, so a group
 * channel, a message nobody has fetched yet, and a conversation where receipts
 * are switched off all look identical — empty. That matters: an indicator that
 * renders a "not delivered" state would leak the difference between "receipts
 * are off" and "they haven't read it".
 *
 * Multi-participant DMs are the general case, not an afterthought. With more
 * than one other member the tick is accompanied by a count ("2/4"), because a
 * single check mark cannot honestly summarise five people. A 1:1 DM is the
 * degenerate case and shows the bare tick.
 */
import { Check, CheckCheck } from "lucide-react";
import type { MessageReceipts } from "../../types";

interface ReceiptIndicatorProps {
  /** This message's receipts, or undefined when none have arrived. */
  receipts?: MessageReceipts;
  /**
   * How many OTHER members are in this DM. Drives the "2/4" summary and, when
   * it is 1, suppresses the count entirely.
   */
  peerCount: number;
  /**
   * False for group channels and for a viewer who is not the message's author —
   * receipts are shown only to the person who sent the message.
   */
  visible: boolean;
}

export function ReceiptIndicator({ receipts, peerCount, visible }: ReceiptIndicatorProps) {
  if (!visible || !receipts || peerCount < 1) {
    return null;
  }

  const readCount = receipts.read_by.length;
  const deliveredCount = receipts.delivered_by.length;
  if (deliveredCount === 0) {
    return null;
  }

  // Read wins over delivered: the schema guarantees a reader who has read has
  // also received, so "everyone read it" is the strongest true statement.
  const allRead = readCount >= peerCount;
  const anyRead = readCount > 0;

  const label = allRead
    ? "Read by everyone"
    : anyRead
      ? `Read by ${readCount} of ${peerCount}`
      : `Delivered to ${deliveredCount} of ${peerCount}`;

  // Accent only once EVERY peer has read it; a partial read stays muted so the
  // strong colour keeps meaning one specific thing.
  const tone = allRead ? "text-accent" : "text-muted";

  return (
    <span
      className={`ml-1 inline-flex items-center gap-0.5 align-middle ${tone}`}
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
