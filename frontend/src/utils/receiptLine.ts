/*
 * What the desktop DM status line says (#2140). Pure, so frontend/tests/receipt-line.test.ts
 * pins it. The line only ever sits on the message `receiptTargetId` picks.
 */

export interface ReceiptLineInput {
  delivered_by: readonly string[];
  read_by: readonly string[];
  read_at?: Readonly<Record<string, string>>;
}

export type ReceiptLineState = "delivered" | "read-some" | "read";

type T = (key: string, opts?: Record<string, unknown>) => string;

/**
 * "Delivered" until a peer reads it, then "Read <time>" (the latest reader's time once everyone
 * in the DM has read it), or "Read by n of m" while only some of a larger DM have. Null when no
 * peer's device has it yet: the line shows nothing rather than a state that would tell whether
 * receipts are off (#892).
 */
export function receiptLineText(
  receipt: ReceiptLineInput | undefined,
  peerCount: number,
  t: T,
  formatTime: (at: string) => string,
): { state: ReceiptLineState; text: string } | null {
  if (!receipt || receipt.delivered_by.length === 0 || peerCount < 1) {
    return null;
  }
  const readCount = receipt.read_by.length;
  if (readCount === 0) {
    return { state: "delivered", text: t("chat:receipts.delivered") };
  }
  if (readCount < peerCount) {
    return {
      state: "read-some",
      text: t("chat:receipts.readByCount", { count: readCount, total: peerCount }),
    };
  }
  let latest: string | null = null;
  let latestMs = -Infinity;
  for (const reader of receipt.read_by) {
    const at = receipt.read_at?.[reader];
    const ms = at ? Date.parse(at) : Number.NaN;
    if (at && !Number.isNaN(ms) && ms > latestMs) {
      latest = at;
      latestMs = ms;
    }
  }
  return {
    state: "read",
    text: latest ? t("chat:receipts.readAt", { time: formatTime(latest) }) : t("chat:receipts.read"),
  };
}
