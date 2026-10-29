// Optimistic sends and the durable outbox (#1335); desktop half of
// `mobile/hooks/queries/pendingMessages.ts`. A `pending-…` stub renders at
// once; the core stores the message first under the stub's `outbox_id`, so a
// refetched stored row replaces the stub. A stub the store never got (failed
// upload) is carried across refetches so the failure stays visible. Pure
// (`pending-messages.test.ts`).

export interface OutgoingLike {
  id: string;
  status?: "pending" | "sending" | "sent" | "failed" | "cancelled";
  outbox_id?: string;
  retryable?: boolean;
}

export type SendState = "pending" | "failed" | "rejected";

export function isLocalStub(m: OutgoingLike): boolean {
  return m.id.startsWith("pending-");
}

/** A stored row's `send_state` as fields: pending reads as `sending` (drawn as sent); failed offers Retry / Discard. */
export function outboxFields(
  id: string,
  sendState: SendState | null | undefined,
): Pick<OutgoingLike, "status" | "outbox_id"> & { delivered: boolean } {
  if (sendState === "pending") {
    return { status: "sending", outbox_id: id, delivered: false };
  }
    // `rejected` (no retry fixes it) is shown like `failed`; the user decides.
  if (sendState === "failed" || sendState === "rejected") {
    return { status: "failed", outbox_id: id, delivered: false };
  }
  return { status: "sent", delivered: true };
}

/**
 * Refetched rows plus the cache's stubs, appended. A stub whose `outbox_id` the
 * store holds is dropped. A FAILED stub it doesn't hold never reached the core,
 * so it returns `retryable: false` (Discard only).
 */
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

/** Rows once a send settled: the confirmed row replaces the stored copy or stub, else is appended. */
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

/** The id for `retry_message`, or null when not failed or never stored (Discard only). */
export function retryTarget(m: OutgoingLike): string | null {
  if (m.status !== "failed" || !m.outbox_id || m.retryable === false) {
    return null;
  }
  return m.outbox_id;
}

export function mapOutboxRows<M extends OutgoingLike>(
  rows: ReadonlyArray<M>,
  outboxId: string,
  fn: (m: M) => M,
): M[] {
  return rows.map((m) => (m.outbox_id === outboxId || m.id === outboxId ? fn(m) : m));
}

export function withoutOutboxRows<M extends OutgoingLike>(
  rows: ReadonlyArray<M>,
  outboxId: string,
): M[] {
  return rows.filter((m) => m.outbox_id !== outboxId && m.id !== outboxId);
}
