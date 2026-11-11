/* The new device's wait for approval (#1809): one awaited call, a hard deadline, no poll. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  ENROLLMENT_DEADLINE_MARGIN_MS,
  ENROLLMENT_TTL_MS,
  enrollmentDeadlineMs,
  isEnrollmentRequestLive,
  waitForEnrollment,
  type EnrollmentStatus,
  type EnrollmentWaitOutcome,
} from "../lib/enrollmentWait.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

const T0 = Date.parse("2026-11-11T12:00:00Z");
const EXPIRES = new Date(T0 + ENROLLMENT_TTL_MS).toISOString();

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

// Flush the promise chain inside waitForEnrollment.
const settle = () => new Promise<void>((r) => setImmediate(r));

function start(
  t: test.TestContext,
  call: (id: string) => Promise<EnrollmentStatus>,
  expiresAt = EXPIRES,
) {
  t.mock.timers.enable({ apis: ["setTimeout", "Date"], now: T0 });
  const outcomes: EnrollmentWaitOutcome[] = [];
  const calls: string[] = [];
  const cancel = waitForEnrollment({
    requestId: "req-1",
    expiresAt,
    call: (id) => {
      calls.push(id);
      return call(id);
    },
    onSettle: (o) => outcomes.push(o),
  });
  return { outcomes, calls, cancel };
}

test("approved settles once with the approving device", async (t) => {
  const { outcomes, calls } = start(t, async () => ({
    status: "approved",
    approved_by_device_id: "dev-a",
  }));
  await settle();
  assert.deepEqual(calls, ["req-1"]);
  assert.deepEqual(outcomes, [{ kind: "approved", approvedByDeviceId: "dev-a" }]);
  // The deadline was cleared: nothing fires later.
  t.mock.timers.tick(ENROLLMENT_TTL_MS + ENROLLMENT_DEADLINE_MARGIN_MS + 1);
  assert.equal(outcomes.length, 1);
});

test("rejected and expired are reported as such", async (t) => {
  const rejected = start(t, async () => ({ status: "rejected" }));
  await settle();
  assert.deepEqual(rejected.outcomes, [{ kind: "rejected" }]);
  t.mock.timers.reset();

  const expired = start(t, async () => ({ status: "expired" }));
  await settle();
  assert.deepEqual(expired.outcomes, [{ kind: "expired" }]);
});

test("an error ends the wait with its message and is never retried", async (t) => {
  const { outcomes, calls } = start(t, async () => {
    throw new Error("DS unreachable");
  });
  await settle();
  assert.deepEqual(outcomes, [{ kind: "error", message: "DS unreachable" }]);
  t.mock.timers.tick(ENROLLMENT_TTL_MS + ENROLLMENT_DEADLINE_MARGIN_MS + 1);
  await settle();
  assert.equal(calls.length, 1, "no automatic retry");
  assert.equal(outcomes.length, 1);
});

test("a call that throws synchronously is still an error, not a crash", async (t) => {
  const { outcomes } = start(t, () => {
    throw new Error("bridge not ready");
  });
  await settle();
  assert.deepEqual(outcomes, [{ kind: "error", message: "bridge not ready" }]);
});

test("the deadline fires at expires_at + the margin when the call hangs", async (t) => {
  const pending = deferred<EnrollmentStatus>();
  const { outcomes } = start(t, () => pending.promise);
  t.mock.timers.tick(ENROLLMENT_TTL_MS + ENROLLMENT_DEADLINE_MARGIN_MS - 1);
  assert.equal(outcomes.length, 0);
  t.mock.timers.tick(1);
  assert.deepEqual(outcomes, [{ kind: "expired" }]);
  // A late answer after the deadline is ignored.
  pending.resolve({ status: "approved" });
  await settle();
  assert.equal(outcomes.length, 1);
});

test("cancel (unmount) ignores a late result and clears the deadline", async (t) => {
  const pending = deferred<EnrollmentStatus>();
  const { outcomes, cancel } = start(t, () => pending.promise);
  cancel();
  pending.resolve({ status: "approved", approved_by_device_id: "dev-a" });
  await settle();
  t.mock.timers.tick(ENROLLMENT_TTL_MS + ENROLLMENT_DEADLINE_MARGIN_MS + 1);
  assert.deepEqual(outcomes, []);
});

test("cancel also swallows a late error", async (t) => {
  const pending = deferred<EnrollmentStatus>();
  const { outcomes, cancel } = start(t, () => pending.promise);
  cancel();
  pending.reject(new Error("late"));
  await settle();
  assert.deepEqual(outcomes, []);
});

test("the deadline is bounded against a skewed clock or a bad timestamp", () => {
  const margin = ENROLLMENT_DEADLINE_MARGIN_MS;
  assert.equal(enrollmentDeadlineMs(EXPIRES, T0), ENROLLMENT_TTL_MS + margin);
  assert.equal(enrollmentDeadlineMs(EXPIRES, T0 + 60_000), ENROLLMENT_TTL_MS - 60_000 + margin);
  // Already past expires_at (or a clock running ahead): the margin still lets the core answer.
  assert.equal(enrollmentDeadlineMs(EXPIRES, T0 + ENROLLMENT_TTL_MS * 2), margin);
  // A clock running behind never stretches the wait past one TTL.
  assert.equal(enrollmentDeadlineMs(EXPIRES, T0 - 3_600_000), ENROLLMENT_TTL_MS + margin);
  assert.equal(enrollmentDeadlineMs("not a date", T0), ENROLLMENT_TTL_MS + margin);
});

test("Try again re-awaits only a request that is still open", () => {
  assert.ok(isEnrollmentRequestLive(EXPIRES, T0));
  assert.ok(!isEnrollmentRequestLive(EXPIRES, T0 + ENROLLMENT_TTL_MS));
  assert.ok(!isEnrollmentRequestLive("not a date", T0));
});

test("the TTL matches the core's ENROLLMENT_TTL_SECS", () => {
  const rust = readFileSync(
    join(ROOT, "..", "pollis-core", "src", "commands", "device_enrollment.rs"),
    "utf8",
  );
  // e.g. `const ENROLLMENT_TTL_SECS: i64 = 10 * 60;`
  const m = rust.match(/const ENROLLMENT_TTL_SECS: i64 = ([\d_ *]+);/);
  assert.ok(m, "ENROLLMENT_TTL_SECS not found");
  const secs = m[1]
    .split("*")
    .map((f) => Number(f.trim().replace(/_/g, "")))
    .reduce((a, b) => a * b, 1);
  assert.equal(secs * 1000, ENROLLMENT_TTL_MS);
});

test("the enrollment screen awaits once and never polls", () => {
  const hook = read("hooks/queries/useEnrollment.ts");
  assert.match(hook, /"await_enrollment_approval"/);
  assert.doesNotMatch(hook, /poll_enrollment_status/);
  assert.doesNotMatch(hook, /refetchInterval|setInterval/);
  const screen = read("app/(auth)/enrollment.tsx");
  assert.match(screen, /useAwaitEnrollmentApproval\(/);
  assert.doesNotMatch(screen, /setInterval|setTimeout|refetchInterval/);
  // Approved → PIN-create; anything else → the reason and a Try again button.
  assert.match(screen, /outcome\?\.kind === "approved"[\s\S]*?router\.replace\("\/\(auth\)\/pin"\)/);
  assert.match(screen, /testID="btn-enroll-try-again"[\s\S]*?onPress=\{onTryAgain\}/);
  assert.match(screen, /isEnrollmentRequestLive\(handle\.expires_at, Date\.now\(\)\)[\s\S]*?wait\.retry\(\)/);
});
