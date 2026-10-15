// Optimistic sends and the durable outbox (#1335) — the desktop half of
// `mobile/hooks/queries/pendingMessages.ts`.
//
// A send renders a local stub (`pending-…` id) at once and swaps it for the
// confirmed row when `send_message` resolves. Since #1335 the core stores the
// message before any network work, under the id the stub carries as
// `outbox_id`, so the stored row can reach the cache (any refetch) while the
// stub is still there; the stub then steps aside rather than showing the
// message twice. A stub the store never got — an attachment upload failed
// before the send — is carried across refetches so a failed send stays on
// screen, labelled, instead of vanishing with the next read.
//
// A stored row that is unsent reads back with a `send_state`: pending (being
// delivered — drawn exactly like a sent row) or failed (Retry / Discard).
//
// Pure, so `tests/pending-messages.test.ts` pins it.

/** The fields these rules read. */
export interface OutgoingLike {
  id: string;
  status?: "pending" | "sending" | "sent" | "failed" | "cancelled";
  outbox_id?: string;
  retryable?: boolean;
}

/** The core's `send_state` on our own not-yet-delivered message. */
export type SendState = "pending" | "failed";

/** A local-only optimistic row. */
export function isLocalStub(m: OutgoingLike): boolean {
  return m.id.startsWith("pending-");
}

/**
 * The fields a stored row's `send_state` maps to. Pending reads as `sending`
 * (rendered like a sent row — only failure is labelled); failed is labelled
 * and offers Retry / Discard.
 */
export function outboxFields(
  id: string,
  sendState: SendState | null | undefined,
): Pick<OutgoingLike, "status" | "outbox_id"> & { delivered: boolean } {
  if (sendState === "pending") {
    return { status: "sending", outbox_id: id, delivered: false };
  }
  if (sendState === "failed") {
    return { status: "failed", outbox_id: id, delivered: false };
  }
  return { status: "sent", delivered: true };
}

/**
 * The rows a refetch read, with the cached stubs it could not have read
 * carried over at the end (the log is oldest-first). A stub whose message the
 * store now holds (by `outbox_id`) is dropped: the stored row is that message.
 */
export function carryStubs<M extends OutgoingLike>(
  fetched: M[],
  cached: ReadonlyArray<M> | undefined,
): M[] {
  const stored = new Set(fetched.map((m) => m.id));
  const stubs = (cached ?? []).filter(
    (m) => isLocalStub(m) && !(m.outbox_id && stored.has(m.outbox_id)),
  );
  return stubs.length === 0 ? fetched : [...fetched, ...stubs];
}

/**
 * The rows once a send settled: the confirmed row replaces whichever copy is
 * there — the stored row a refetch brought in (dropping the stub), or the stub
 * — or is appended if neither is.
 */
export function settleSend<M extends OutgoingLike>(
  rows: ReadonlyArray<M>,
  stubId: string | undefined,
  confirmed: M,
): M[] {
  if (rows.some((m) => m.id === confirmed.id)) {
    return rows
      .filter((m) => m.id !== stubId)
      .map((m) => (m.id === confirmed.id ? confirmed : m));
  }
  if (stubId && rows.some((m) => m.id === stubId)) {
    return rows.map((m) => (m.id === stubId ? confirmed : m));
  }
  return [...rows, confirmed];
}

/**
 * The id Retry should hand `retry_message`, or null when there is nothing the
 * core could retry: the row is not failed, or the send failed before the core
 * stored it — Discard is the only way out of that one.
 */
export function retryTarget(m: OutgoingLike): string | null {
  if (m.status !== "failed" || !m.outbox_id || m.retryable === false) {
    return null;
  }
  return m.outbox_id;
}

/** Every row standing for message `outboxId`, with `fn` applied. */
export function mapOutboxRows<M extends OutgoingLike>(
  rows: ReadonlyArray<M>,
  outboxId: string,
  fn: (m: M) => M,
): M[] {
  return rows.map((m) => (m.outbox_id === outboxId || m.id === outboxId ? fn(m) : m));
}

/** The rows without any that stand for message `outboxId` (Discard). */
export function withoutOutboxRows<M extends OutgoingLike>(
  rows: ReadonlyArray<M>,
  outboxId: string,
): M[] {
  return rows.filter((m) => m.outbox_id !== outboxId && m.id !== outboxId);
}
