// Keeping optimistic sends on screen across a refetch (#1291).
//
// A send puts a local-only stub (`pending-…` id) at the front of the newest
// page, and swaps it for the confirmed row when `send_message` resolves. The
// row reaches the local store only at that point — so any refetch of the
// conversation in between (a realtime ingest, a resume catch-up, focus) reads
// the store, finds no such row, and the cache it writes has no stub: the
// message vanished ("No messages yet") until some later refetch, and the
// success handler then had nothing to swap. Both halves are fixed here, pure
// so tests/pending-messages.test.ts pins them.

/** The fields these rules read. */
export interface StubLike {
  id: string;
  pending?: boolean;
  failed?: boolean;
}

/** A local-only optimistic row: still sending, or labelled failed. */
export function isLocalStub(m: StubLike): boolean {
  return m.id.startsWith("pending-") && (m.pending === true || m.failed === true);
}

/**
 * Where new rows sit: the conversation pages are newest-first (stubs in
 * front), a thread's replies oldest-first (stubs at the end).
 */
export type Order = "newestFirst" | "oldestFirst";

/**
 * The rows a refetch read, with the stubs the cache held carried over at the
 * new end, in their cached order. A stub is local-only and never in the
 * store, so it is never a duplicate of a fetched row.
 */
export function carryStubs<M extends StubLike>(
  fetched: M[],
  cached: ReadonlyArray<M> | undefined,
  order: Order = "newestFirst",
): M[] {
  const stubs = (cached ?? []).filter(isLocalStub);
  if (stubs.length === 0) {
    return fetched;
  }
  return order === "newestFirst" ? [...stubs, ...fetched] : [...fetched, ...stubs];
}

/**
 * The rows once a send settled: the stub swapped for the confirmed row; if
 * the stub is gone, the confirmed row added at the new end unless a refetch
 * already brought it in.
 */
export function settleSend<M extends StubLike>(
  rows: ReadonlyArray<M>,
  stubId: string,
  confirmed: M,
  order: Order = "newestFirst",
): M[] {
  if (rows.some((m) => m.id === stubId)) {
    return rows.map((m) => (m.id === stubId ? confirmed : m));
  }
  if (rows.some((m) => m.id === confirmed.id)) {
    return [...rows];
  }
  return order === "newestFirst" ? [confirmed, ...rows] : [...rows, confirmed];
}
