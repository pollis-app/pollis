// Keeping optimistic sends on screen across a refetch (#1291) and the durable outbox's unsent rows (#1335). Pure.

export interface StubLike {
  id: string;
  pending?: boolean;
  failed?: boolean;
  /** The core's id for this send (#1335), named by Retry and Discard. */
  outboxId?: string;
  retryable?: boolean;
}

export function isLocalStub(m: StubLike): boolean {
  return m.id.startsWith("pending-") && (m.pending === true || m.failed === true);
}

export type Order = "newestFirst" | "oldestFirst";

/** A refetch's rows with cached stubs carried over; a FAILED stub the store lacks gets `retryable: false`. */
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

/** The rows once a send settled, whatever a refetch did in between. */
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

/** A pending row looks sent but offers no actions until delivered. */
export function unsentFlags(
  id: string,
  sendState: SendState | null | undefined,
): Pick<StubLike, "pending" | "failed" | "outboxId"> {
  if (sendState === "pending") {
    return { pending: true, outboxId: id };
  }
  // `rejected` is shown exactly like `failed`.
  if (sendState === "failed" || sendState === "rejected") {
    return { failed: true, outboxId: id };
  }
  return {};
}

/** The id for `retry_message`, or null when the core never stored the send. */
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
