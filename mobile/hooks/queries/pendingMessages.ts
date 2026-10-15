// Keeping optimistic sends on screen across a refetch (#1291), and showing
// the durable outbox's unsent rows (#1335).
//
// A send puts a local-only stub (`pending-…` id) at the front of the newest
// page, and swaps it for the confirmed row when `send_message` resolves. A
// refetch in between (a realtime ingest, a resume catch-up, focus) must not
// drop the stub, and the success handler must still find something to swap.
//
// Since #1335 the core stores the message (as `send_state: "pending"`) before
// any network work, under the id the stub carries as `outboxId` — so a refetch
// can now bring the stored row in while the stub is still on screen. The stub
// steps aside for it then, and is never a second copy. A stored row that is
// unsent survives restarts: it reads back pending (being delivered — drawn
// exactly like a sent one) or failed (Retry / Discard).
//
// Pure, so tests/pending-messages.test.ts pins it.

/** The fields these rules read. */
export interface StubLike {
  id: string;
  pending?: boolean;
  failed?: boolean;
  /** The core's message id for this send (#1335): the stored outbox row's
   *  id, which Retry and Discard name. Set on stubs and on unsent rows. */
  outboxId?: string;
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
 * new end, in their cached order. A stub whose message the store now holds
 * (by `outboxId`) is dropped: the stored row is the same message.
 */
export function carryStubs<M extends StubLike>(
  fetched: M[],
  cached: ReadonlyArray<M> | undefined,
  order: Order = "newestFirst",
): M[] {
  const stored = new Set(fetched.map((m) => m.id));
  const stubs = (cached ?? []).filter(
    (m) => isLocalStub(m) && !(m.outboxId && stored.has(m.outboxId)),
  );
  if (stubs.length === 0) {
    return fetched;
  }
  return order === "newestFirst" ? [...stubs, ...fetched] : [...fetched, ...stubs];
}

/**
 * The rows once a send settled: the stub swapped for the confirmed row; if a
 * refetch already brought the confirmed row in, that row is replaced (it may
 * still read pending) and the stub dropped; if both are gone, the confirmed
 * row is added at the new end.
 */
export function settleSend<M extends StubLike>(
  rows: ReadonlyArray<M>,
  stubId: string,
  confirmed: M,
  order: Order = "newestFirst",
): M[] {
  if (rows.some((m) => m.id === confirmed.id)) {
    return rows
      .filter((m) => m.id !== stubId)
      .map((m) => (m.id === confirmed.id ? confirmed : m));
  }
  if (rows.some((m) => m.id === stubId)) {
    return rows.map((m) => (m.id === stubId ? confirmed : m));
  }
  return order === "newestFirst" ? [confirmed, ...rows] : [...rows, confirmed];
}

/** The core's `send_state` for an unsent row of ours (#1335). */
export type SendState = "pending" | "failed" | "rejected";

/**
 * The flags a stored row's `send_state` maps to. A pending row is drawn like
 * a sent one (no "sending" label), but like a stub it offers no actions until
 * it is delivered; a failed row is labelled and offers Retry / Discard.
 */
export function unsentFlags(
  id: string,
  sendState: SendState | null | undefined,
): Pick<StubLike, "pending" | "failed" | "outboxId"> {
  if (sendState === "pending") {
    return { pending: true, outboxId: id };
  }
  // `rejected` (a refusal no retry fixes — the outbox stops retrying it on its
  // own) is shown exactly like `failed`: the user's Retry or Discard decides.
  if (sendState === "failed" || sendState === "rejected") {
    return { failed: true, outboxId: id };
  }
  return {};
}

/**
 * The id Retry should hand `retry_message`, or null when there is nothing the
 * core could retry: the row is not failed, or the send failed before the core
 * stored it (an attachment upload) — Discard is the only way out of that one.
 */
export function retryTarget(m: StubLike & { retryable?: boolean }): string | null {
  if (m.failed !== true || !m.outboxId || m.retryable === false) {
    return null;
  }
  return m.outboxId;
}

/** Every row that stands for message `outboxId`, with `fn` applied. */
export function mapOutboxRows<M extends StubLike>(
  rows: ReadonlyArray<M>,
  outboxId: string,
  fn: (m: M) => M,
): M[] {
  return rows.map((m) => (m.outboxId === outboxId || m.id === outboxId ? fn(m) : m));
}

/** The rows without any that stand for message `outboxId` (Discard). */
export function withoutOutboxRows<M extends StubLike>(
  rows: ReadonlyArray<M>,
  outboxId: string,
): M[] {
  return rows.filter((m) => m.outboxId !== outboxId && m.id !== outboxId);
}
