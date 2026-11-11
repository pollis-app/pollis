// The new device's wait for approval (#1809): ONE awaited `await_enrollment_approval`, never a poll.
// The core backs off 1 s -> 8 s and answers `expired` at the TTL + 5 s; nothing here ticks or retries.
// The single timer is a hard deadline in case the core call hangs, cleared when the wait ends.

export type EnrollmentStatus =
  | { status: "pending" | "rejected" | "expired" }
  // The device that approved this one (#1735), when the core reports it; the history offer's default source.
  | { status: "approved"; approved_by_device_id?: string | null };

export type EnrollmentWaitOutcome =
  | { kind: "approved"; approvedByDeviceId: string | null }
  | { kind: "rejected" }
  | { kind: "expired" }
  | { kind: "error"; message: string };

// `ENROLLMENT_TTL_SECS` in pollis-core's device_enrollment.rs.
export const ENROLLMENT_TTL_MS = 600_000;
// Past the core's own TTL + 5 s stop, so the core normally reports the expiry itself.
export const ENROLLMENT_DEADLINE_MARGIN_MS = 15_000;

/** Milliseconds until the hard stop: the request's `expires_at` + the margin, never more than a full TTL + margin. */
export function enrollmentDeadlineMs(expiresAt: string, now: number): number {
  const left = Date.parse(expiresAt) - now;
  // A skewed device clock must not end the wait at once or stretch it past one TTL.
  const bounded = Number.isFinite(left) ? Math.min(Math.max(left, 0), ENROLLMENT_TTL_MS) : ENROLLMENT_TTL_MS;
  return bounded + ENROLLMENT_DEADLINE_MARGIN_MS;
}

/** Whether a fresh await on the same request can still be answered, or a new request is needed. */
export function isEnrollmentRequestLive(expiresAt: string, now: number): boolean {
  const at = Date.parse(expiresAt);
  return Number.isFinite(at) && at > now;
}

function outcomeOf(status: EnrollmentStatus): EnrollmentWaitOutcome {
  if (status.status === "approved") {
    return { kind: "approved", approvedByDeviceId: status.approved_by_device_id ?? null };
  }
  if (status.status === "rejected") {
    return { kind: "rejected" };
  }
  // The core never answers `pending`; if it did, the wait still ended without an approval.
  return { kind: "expired" };
}

function messageOf(err: unknown): string {
  if (err instanceof Error) {
    return err.message;
  }
  return typeof err === "string" ? err : "";
}

/**
 * Start one wait. `onSettle` runs exactly once, unless the returned cancel runs first;
 * after cancel, a late answer is ignored and the deadline is cleared.
 */
export function waitForEnrollment(opts: {
  requestId: string;
  expiresAt: string;
  call: (requestId: string) => Promise<EnrollmentStatus>;
  onSettle: (outcome: EnrollmentWaitOutcome) => void;
}): () => void {
  let done = false;
  let deadline: ReturnType<typeof setTimeout> | undefined;
  const finish = (outcome: EnrollmentWaitOutcome | null) => {
    if (done) {
      return;
    }
    done = true;
    clearTimeout(deadline);
    if (outcome) {
      opts.onSettle(outcome);
    }
  };
  deadline = setTimeout(
    () => finish({ kind: "expired" }),
    enrollmentDeadlineMs(opts.expiresAt, Date.now()),
  );
  Promise.resolve()
    .then(() => opts.call(opts.requestId))
    .then(outcomeOf, (err: unknown) => ({ kind: "error", message: messageOf(err) }) as const)
    .then(finish);
  return () => finish(null);
}
