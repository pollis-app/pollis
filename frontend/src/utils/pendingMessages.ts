// Optimistic stubs and the durable outbox (#1335); desktop half of `mobile/hooks/queries/pendingMessages.ts`.

/** The fields these rules read. */
export interface OutgoingLike {
  id: string;
  status?: "pending" | "sending" | "sent" | "failed" | "cancelled";
  outbox_id?: string;
  retryable?: boolean;
}

/** The core's `send_state` on our own not-yet-delivered message. */
export type SendState = "pending" | "failed" | "rejected";

/** A local-only optimistic row. */
export function isLocalStub(m: OutgoingLike): boolean {
  return m.id.startsWith("pending-");
}

/** `send_state` → row fields: pending reads as sending; failed offers Retry / Discard. */
export function outboxFields(
  id: string,
  sendState: SendState | null | undefined,
): Pick<OutgoingLike, "status" | "outbox_id"> & { delivered: boolean } {
  if (sendState === "pending") {
    return { status: "sending", outbox_id: id, delivered: false };
  }
  // `rejected` shows as `failed`.
  if (sendState === "failed" || sendState === "rejected") {
    return { status: "failed", outbox_id: id, delivered: false };
  }
  return { status: "sent", delivered: true };
}

/** Refetched rows plus carried stubs; a stored stub is dropped, an unstored failed one is discard-only. */
export function carryStubs<M extends OutgoingLike>(
  fetched: M[],
  cached: ReadonlyArray<M> | undefined,
): M[] {
  const stored = new Set(fetched.map((m) => m.id));
  const stubs = (cached ?? [])
    .filter((m) => isLocalStub(m) && !(m.outbox_id && stored.has(m.outbox_id)))
    .map((m) => (m.status === "failed" ? { ...m, retryable: false } : m));
  return stubs.length === 0 ? fetched : [...fetched, ...stubs];
}

/** The confirmed row replaces the stored row or stub, else appends. */
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

/** The id to retry, or null if not failed or never stored. */
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
