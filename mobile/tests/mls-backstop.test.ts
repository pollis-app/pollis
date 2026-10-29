import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { catchUpThenBackstop } from "../lib/mlsBackstop.ts";

const targets = [
  { id: "ch1", kind: "channel" as const },
  { id: "dm1", kind: "dm" as const },
];

test("the backstop runs once, after every ingest has settled", async () => {
  const log: string[] = [];
  let releaseDm: () => void = () => {};
  const ingest = (id: string) => {
    log.push(`start ${id}`);
    if (id === "dm1") {
      return new Promise<void>((resolve) => {
        releaseDm = () => {
          log.push(`done ${id}`);
          resolve();
        };
      });
    }
    log.push(`done ${id}`);
    return Promise.resolve();
  };
  const pass = catchUpThenBackstop(targets, ingest, async () => {
    log.push("backstop");
  });
  await new Promise((r) => setTimeout(r, 0));
  assert.ok(!log.includes("backstop"), "not before the slow ingest finished");
  releaseDm();
  await pass;
  assert.deepEqual(log.slice(-2), ["done dm1", "backstop"]);
  assert.equal(log.filter((l) => l === "backstop").length, 1);
});

test("a failed ingest does not cost the others their backstop, and a failed backstop never throws", async () => {
  let ran = false;
  await catchUpThenBackstop(
    targets,
    (id) => (id === "ch1" ? Promise.reject(new Error("offline")) : Promise.resolve()),
    async () => {
      ran = true;
      throw new Error("DS down");
    },
  );
  assert.equal(ran, true);
});

test("a pass already running is joined, not doubled", async () => {
  let backstops = 0;
  let release: () => void = () => {};
  const gate = new Promise<void>((r) => {
    release = r;
  });
  const first = catchUpThenBackstop(targets, () => gate, async () => {
    backstops += 1;
  });
  const second = catchUpThenBackstop(targets, () => Promise.resolve(), async () => {
    backstops += 1;
  });
  assert.equal(first, second);
  release();
  await first;
  assert.equal(backstops, 1);
  await catchUpThenBackstop(targets, () => Promise.resolve(), async () => {
    backstops += 1;
  });
  assert.equal(backstops, 2);
});

test("landing and resume both end their catch-up with the backstop", () => {
  const src = readFileSync(new URL("../hooks/useInboxRealtime.ts", import.meta.url), "utf8");
  const calls = src.match(/catchUpThenBackstop\(/g) ?? [];
  assert.equal(calls.length, 2, "one call on resume, one on landing");
  assert.match(src, /invoke\("reconcile_backstop_all"/);
});
