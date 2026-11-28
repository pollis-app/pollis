/*
 * The DM status line (#2140, docs/design/README.md → "Read receipts"): one line in words under the
 * viewer's newest acknowledged message, Modern only. "Delivered", then "Read 11:03 AM".
 *
 * Every own DM row holds one, shut. When the line moves to a newer message the old one folds shut
 * while the new one opens, on the same timing, so the log's height never changes; a change of
 * state cross-fades in place at a fixed height. Under reduced motion both are instant
 * (`.motion-fold` and `.receipt-line-faces` in components/ui/motion.css). Nothing shows until a peer's device has the
 * message, so "receipts off" never leaks (#892).
 */
import { useState } from "react";
import { useRowT } from "../../i18n/useRowT";
import type { MessageReceipts } from "../../types";
import { receiptLineText, type ReceiptLineState } from "../../utils/receiptLine";
import { formatTimeOfDay } from "../../utils/format";

interface ReceiptLineProps {
  messageId: string;
  /** Set only on the message that carries the line (`receiptTargetId`). */
  receipt?: MessageReceipts;
  /** Other members of the DM. */
  peerCount: number;
}

interface Faces {
  /** Two stacked faces, so a new state fades in over the old one. */
  a: string | null;
  b: string | null;
  front: "a" | "b";
  open: boolean;
  state: ReceiptLineState | null;
}

export function ReceiptLine({ messageId, receipt, peerCount }: ReceiptLineProps) {
  const t = useRowT("chat");
  const line = receiptLineText(receipt, peerCount, t, (at) => formatTimeOfDay(Date.parse(at)));
  const label = line?.text ?? null;

  // Derived during render, so the opening frame already holds its text: an opening line takes the
  // front face as it is; a change while open goes to the back face, which becomes the front.
  const [faces, setFaces] = useState<Faces>({
    a: label,
    b: null,
    front: "a",
    open: label !== null,
    state: line?.state ?? null,
  });
  if (label === null && faces.open) {
    // Folding shut keeps its last words until it has closed.
    setFaces({ ...faces, open: false });
  } else if (label !== null && (!faces.open || label !== faces[faces.front])) {
    if (!faces.open) {
      setFaces({ ...faces, [faces.front]: label, open: true, state: line!.state });
    } else {
      const back = faces.front === "a" ? "b" : "a";
      setFaces({ ...faces, [back]: label, front: back, state: line!.state });
    }
  }

  return (
    // `ui/Fold`'s track (`.motion-fold`), without Fold itself: Fold opens a frame late and
    // unmounts when shut, and this line's fold and opening must start on the same frame.
    <div
      className="motion-fold"
      data-open={faces.open}
      aria-hidden={!faces.open}
      data-testid={`receipt-line-${messageId}`}
      data-receipt-state={faces.open ? faces.state ?? undefined : undefined}
    >
      <div className="overflow-hidden">
        <div className="receipt-line-faces text-meta tabular-nums text-muted">
          <span data-front={faces.front === "a"} aria-hidden={faces.front !== "a"}>
            {faces.a}
          </span>
          <span data-front={faces.front === "b"} aria-hidden={faces.front !== "b"}>
            {faces.b}
          </span>
        </div>
      </div>
    </div>
  );
}
