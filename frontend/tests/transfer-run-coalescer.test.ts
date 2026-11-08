import { test } from "node:test";
import assert from "node:assert/strict";
import { createRunCoalescer } from "../src/utils/transferRunCoalescer.ts";

type R = { state: string };
const finished = (r: R) => r.state === "completed";

test("a wake-up during a run that ends paused runs it once more", async () => {
  const results: R[] = [{ state: "paused" }, { state: "completed" }];
  let calls = 0;
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  const c = createRunCoalescer<R>(async () => {
    calls += 1;
    if (calls === 1) {
      await gate;
    }
    return results[calls - 1];
  }, finished);
  const first = c.run("T");
  // The nudge lands while Rust is still deciding to pause.
  const joined = c.run("T");
  c.run("T");
  release();
  assert.deepEqual(await first, { state: "completed" });
  assert.equal(await joined, await first);
  assert.equal(calls, 2, "a burst of wake-ups costs one extra run");
  assert.equal(c.isRunning("T"), false);
});

test("no wake-up: a paused run is not re-run; a finished one never is", async () => {
  let calls = 0;
  const c = createRunCoalescer<R>(async () => {
    calls += 1;
    return { state: "paused" };
  }, finished);
  assert.deepEqual(await c.run("T"), { state: "paused" });
  assert.equal(calls, 1);

  let done = 0;
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  const f = createRunCoalescer<R>(async () => {
    done += 1;
    await gate;
    return { state: "completed" };
  }, finished);
  const p = f.run("U");
  f.run("U");
  release();
  await p;
  assert.equal(done, 1, "a finished transfer is not run again");
});

test("a failed run (null) with a pending wake-up runs again", async () => {
  let calls = 0;
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  const c = createRunCoalescer<R>(async () => {
    calls += 1;
    if (calls === 1) {
      await gate;
      return null;
    }
    return { state: "completed" };
  }, finished);
  const p = c.run("T");
  c.run("T");
  release();
  assert.deepEqual(await p, { state: "completed" });
  assert.equal(calls, 2);
});
