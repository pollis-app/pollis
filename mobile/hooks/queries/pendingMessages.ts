// Keeping optimistic `pending-…` stubs on screen across a refetch (#1291), and
// showing the durable outbox's unsent rows (#1335). A stub steps aside once a
// refetch brings in the stored row with its `outboxId`, so it is never a
// second copy.

export interface StubLike {
  id: string;
  pending?: boolean;
  failed?: boolean;
  /** The core's message id for this send (#1335). Set on stubs and on unsent rows. */
  outboxId?: string;
  /** False on a failed stub the core never stored: Discard only. */
  retryable?: boolean;
}

/** A local-only optimistic row: still sending, or labelled failed. */
export function isLocalStub(m: StubLike): boolean {
  return m.id.startsWith("pending-") && (m.pending === true || m.failed === true);
}

/** Conversation pages are newest-first; a thread's replies oldest-first. */
export type Order = "newestFirst" | "oldestFirst";

/**
 * The refetched rows with the cached stubs carried over at the new end. A stub
 * the store now holds (by `outboxId`) is dropped. A failed stub the store does
 * not hold failed before the core stored anything, so it becomes
 * `retryable: false` (Discard only).
 */
export function carryStubs<M extends StubLike>(
  fetched: M[],
  cached: ReadonlyArray<M> | undefined,
  order: Order = "newestFirst",
): M[] {
  const stored = new Set(fetched.map((m) => m.id));
  const stubs = (cached ?? [])
    .filter((m) => isLocalStub(m) && !(m.outboxId && stored.has(m.outboxId)))
    .map((m) => (m.failed === true ? { ...m, retryable: false } : m));
  if (stubs.length === 0) {
    return fetched;
  }
  return order === "newestFirst" ? [...stubs, ...fetched] : [...fetched, ...stubs];
}

/**
 * The rows once a send settled. If a refetch already brought the confirmed row
 * in, that row is replaced and the stub dropped; if both are gone, the
 * confirmed row is added at the new end.
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

export type SendState = "pending" | "failed" | "rejected";

export function unsentFlags(
  id: string,
  sendState: SendState | null | undefined,
): Pick<StubLike, "pending" | "failed" | "outboxId"> {
  if (sendState === "pending") {
    return { pending: true, outboxId: id };
  }
  // The outbox stops retrying `rejected` on its own; the user still decides.
  if (sendState === "failed" || sendState === "rejected") {
    return { failed: true, outboxId: id };
  }
  return {};
}

/** The id Retry should hand `retry_message`, or null when the core has nothing to retry. */
export function retryTarget(m: StubLike): string | null {
  if (m.failed !== true || !m.outboxId || m.retryable === false) {
    return null;
  }
  return m.outboxId;
}

export function mapOutboxRows<M extends StubLike>(
  rows: ReadonlyArray<M>,
  outboxId: string,
  fn: (m: M) => M,
): M[] {
  return rows.map((m) => (m.outboxId === outboxId || m.id === outboxId ? fn(m) : m));
}

export function withoutOutboxRows<M extends StubLike>(
  rows: ReadonlyArray<M>,
  outboxId: string,
): M[] {
  return rows.filter((m) => m.outboxId !== outboxId && m.id !== outboxId);
}
