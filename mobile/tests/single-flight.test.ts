/* One ingest per conversation at a time, and nothing asked for mid-run is missed (#1610). */

import test from "node:test";
import assert from "node:assert/strict";

import { createSingleFlight } from "../lib/singleFlight.ts";

function gate() {
  let open!: () => void;
  const opened = new Promise<void>((r) => {
    open = r;
  });
  return { opened, open };
}

test("a burst during a run costs exactly one trailing run", async () => {
  const sf = createSingleFlight();
  const first = gate();
  let runs = 0;
  const task = async () => {
    runs += 1;
    if (runs === 1) {
      await first.opened;
    }
  };
  const a = sf.run("dm:1", task);
  // Let the run begin: from here on, a request may have missed what it fetches.
  await Promise.resolve();
  await Promise.resolve();
  // Requests while the first run is in flight: they must not start runs of their own.
  const b = sf.run("dm:1", task);
  const c = sf.run("dm:1", task);
  assert.equal(b, c, "every mid-run request shares the one trailing run");
  await Promise.resolve();
  assert.equal(runs, 1);
  first.open();
  await Promise.all([a, b, c]);
  assert.equal(runs, 2, "one run, then one trailing run for everything asked meanwhile");
  assert.deepEqual(sf.busy(), []);
});

test("requests in the same dispatch, before the run begins, share that one run", async () => {
  const sf = createSingleFlight();
  let runs = 0;
  const task = async () => {
    runs += 1;
  };
  // One realtime wake-up handled by two listeners in a row.
  const a = sf.run("dm:1", task);
  const b = sf.run("dm:1", task);
  assert.equal(a, b);
  await Promise.all([a, b]);
  assert.equal(runs, 1);
  assert.deepEqual(sf.busy(), []);
});

test("keys are independent, and an idle key starts at once", async () => {
  const sf = createSingleFlight();
  const seen: string[] = [];
  await Promise.all([
    sf.run("dm:1", async () => {
      seen.push("dm:1");
    }),
    sf.run("channel:2", async () => {
      seen.push("channel:2");
    }),
  ]);
  assert.deepEqual(seen.sort(), ["channel:2", "dm:1"]);
  await sf.run("dm:1", async () => {
    seen.push("again");
  });
  assert.equal(seen.length, 3);
  assert.deepEqual(sf.busy(), []);
});

test("a failed run still lets the trailing run go, and reports its own failure", async () => {
  const sf = createSingleFlight();
  const first = gate();
  let trailingRan = false;
  const a = sf.run("dm:1", async () => {
    await first.opened;
    throw new Error("offline");
  });
  await Promise.resolve();
  await Promise.resolve();
  const b = sf.run("dm:1", async () => {
    trailingRan = true;
  });
  first.open();
  await assert.rejects(a, /offline/);
  await b;
  assert.ok(trailingRan);
  assert.deepEqual(sf.busy(), []);
});
