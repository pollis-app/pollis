/*
 * Where the read-up-to avatar sits in a one-to-one DM (#2140, docs/design/README.md → "Read
 * receipts"): level with the newest message of yours the other person has read. Delivery alone
 * never places it, so nothing shows until they have read something.
 */

export interface ReadUpToCandidate {
  id: string;
  sender_id: string;
}

export interface ReadUpToState {
  read_by: readonly string[];
}

/** `newestFirst` is the timeline as rendered, newest message first. */
export function readUpToId(
  newestFirst: Iterable<ReadUpToCandidate>,
  selfId: string | null | undefined,
  peerId: string | null | undefined,
  receipts: ReadonlyMap<string, ReadUpToState> | undefined,
): string | null {
  if (!selfId || !peerId || !receipts || receipts.size === 0) {
    return null;
  }
  for (const m of newestFirst) {
    if (m.sender_id !== selfId) {
      continue;
    }
    if (receipts.get(m.id)?.read_by.includes(peerId)) {
      return m.id;
    }
  }
  return null;
}
