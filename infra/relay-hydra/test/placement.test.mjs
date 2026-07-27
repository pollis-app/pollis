// The random region placement + rotation policy (reconciler/placement.mjs).
// Pure functions, no AWS, no network. The rng is injected so the "random" draw is
// deterministic under test.
//
// Run: node --test   (from infra/relay-hydra/)

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  readDesiredTotal,
  drawPlacement,
  placementNeedsRedraw,
  placementTotal,
  clamp,
} from "../reconciler/placement.mjs";

const REGIONS = ["us-east-1", "us-east-2", "us-west-1", "us-west-2"];

// An rng that walks a fixed list, so a draw is fully determined by the sequence.
function seqRng(values) {
  let i = 0;
  return () => values[i++ % values.length];
}

// --- readDesiredTotal --------------------------------------------------------

test("reads the current {total: N} shape", () => {
  assert.equal(readDesiredTotal('{"total":5}', 2), 5);
});

test("sums the LEGACY per-region shape so the upgrade needs no SSM edit", () => {
  // Terraform's ignore_changes leaves the live parameter at its pre-multi-region
  // value. Misreading this as 0 would scale the running pool to nothing.
  assert.equal(readDesiredTotal('{"us-west-2":3}', 2), 3);
  assert.equal(readDesiredTotal('{"us-west-2":2,"us-east-1":1}', 2), 3);
});

test("falls back rather than throwing on junk", () => {
  assert.equal(readDesiredTotal("not json", 2), 2);
  assert.equal(readDesiredTotal("[1,2,3]", 2), 2);
  assert.equal(readDesiredTotal("null", 2), 2);
  assert.equal(readDesiredTotal("{}", 2), 2);
});

test("total wins over stray sibling keys", () => {
  assert.equal(readDesiredTotal('{"total":4,"us-west-2":99}', 2), 4);
});

// --- drawPlacement -----------------------------------------------------------

test("draws exactly `total` nodes", () => {
  for (const total of [1, 2, 3, 7]) {
    const placement = drawPlacement(REGIONS, total, Math.random);
    assert.equal(placementTotal(placement), total);
  }
});

test("only ever draws allowed regions", () => {
  const placement = drawPlacement(REGIONS, 50, Math.random);
  for (const region of Object.keys(placement)) {
    assert.ok(REGIONS.includes(region), `drew ${region}, which is not allowed`);
  }
});

test("samples WITH replacement — two nodes may share a region", () => {
  // rng always picks index 0 => all three land in us-east-1. The user explicitly
  // accepted collisions; this pins that they're representable, not a bug.
  const placement = drawPlacement(REGIONS, 3, seqRng([0]));
  assert.deepEqual(placement, { "us-east-1": 3 });
});

test("spreads when the rng spreads", () => {
  const placement = drawPlacement(REGIONS, 3, seqRng([0, 0.3, 0.6]));
  assert.deepEqual(placement, { "us-east-1": 1, "us-east-2": 1, "us-west-1": 1 });
});

test("an rng returning values at the top of the range never indexes past the end", () => {
  // Math.random() is [0,1), but a sloppy rng returning exactly 1 must not produce
  // an undefined region key.
  const placement = drawPlacement(REGIONS, 2, seqRng([1]));
  assert.equal(placementTotal(placement), 2);
  for (const region of Object.keys(placement)) {
    assert.ok(REGIONS.includes(region));
  }
});

test("refuses to draw with no allowed regions", () => {
  assert.throws(() => drawPlacement([], 3, Math.random), /no allowed regions/);
});

// --- placementNeedsRedraw ----------------------------------------------------

const base = { allowedRegions: REGIONS, total: 3, now: 1_000_000, rotationIntervalSeconds: 86_400 };

test("keeps a fresh, still-valid placement", () => {
  const current = { drawn_at: base.now - 60, placement: { "us-west-2": 3 } };
  assert.equal(placementNeedsRedraw(current, base), null);
});

test("re-draws when the rotation interval has elapsed", () => {
  const current = { drawn_at: base.now - 86_400, placement: { "us-west-2": 3 } };
  assert.match(placementNeedsRedraw(current, base), /rotation interval elapsed/);
});

test("re-draws when the operator changed the pool size", () => {
  const current = { drawn_at: base.now, placement: { "us-west-2": 2 } };
  assert.match(placementNeedsRedraw(current, base), /pool size changed/);
});

test("re-draws immediately when a region leaves the allowed set", () => {
  // Jurisdiction policy tightened under us: nodes in the newly-denied region must
  // move now, not at the next scheduled rotation.
  const current = { drawn_at: base.now, placement: { "us-east-1": 3 } };
  const reason = placementNeedsRedraw(current, { ...base, allowedRegions: ["us-west-2"] });
  assert.match(reason, /no-longer-allowed region\(s\): us-east-1/);
});

test("re-draws when there is nothing recorded", () => {
  assert.match(placementNeedsRedraw(null, base), /no placement recorded/);
  assert.match(placementNeedsRedraw({}, base), /no placement recorded/);
});

test("a placement with no drawn_at is treated as infinitely stale", () => {
  const current = { placement: { "us-west-2": 3 } };
  assert.match(placementNeedsRedraw(current, base), /rotation interval elapsed/);
});

// --- the loop closes: a re-draw always produces a placement that then holds ---

test("a fresh draw immediately satisfies the keep condition", () => {
  // Guards against a rotation loop that re-draws on every single reconcile — which
  // would terminate and relaunch the whole pool every two minutes.
  const placement = drawPlacement(REGIONS, base.total, Math.random);
  const current = { drawn_at: base.now, placement };
  assert.equal(placementNeedsRedraw(current, base), null);
});

// --- clamp -------------------------------------------------------------------

test("clamps desired-state into [floor, max]", () => {
  assert.equal(clamp(0, 2, 3), 2);
  assert.equal(clamp(99, 2, 3), 3);
  assert.equal(clamp(3, 2, 3), 3);
});
