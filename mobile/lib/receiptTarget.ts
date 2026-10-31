/*
 * Which message carries the DM receipt tick (#1637). One rule, two copies:
 * frontend/src/utils/receiptTarget.ts and this file
 * (frontend/tests/receipt-target.test.ts pins both and that they match).
 */

export interface ReceiptCandidate {
  id: string;
  sender_id: string;
}

export interface ReceiptState {
  delivered_by: readonly string[];
}

/**
 * iMessage-style: only the viewer's newest own message that has a receipt to show.
 * An own message no peer has acknowledged yet is skipped, so the tick never
 * vanishes while the next send is in flight; it moves forward once that one lands.
 * `newestFirst` is the timeline as rendered, newest message first.
 */
export function receiptTargetId(
  newestFirst: Iterable<ReceiptCandidate>,
  selfId: string | null | undefined,
  receipts: ReadonlyMap<string, ReceiptState> | undefined,
): string | null {
  if (!selfId || !receipts || receipts.size === 0) {
    return null;
  }
  for (const m of newestFirst) {
    if (m.sender_id !== selfId) {
      continue;
    }
    const r = receipts.get(m.id);
    if (r && r.delivered_by.length > 0) {
      return m.id;
    }
  }
  return null;
}
