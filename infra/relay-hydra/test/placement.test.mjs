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
  mayDrain,
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
  assert.equal(readDesiredTotal('{"total":5}'), 5);
});

test("sums the LEGACY per-region shape so the upgrade needs no SSM edit", () => {
  // Terraform's ignore_changes leaves the live parameter at its pre-multi-region
  // value. Misreading this as 0 would scale the running pool to nothing.
  assert.equal(readDesiredTotal('{"us-west-2":3}'), 3);
  assert.equal(readDesiredTotal('{"us-west-2":2,"us-east-1":1}'), 3);
});

test("THROWS on junk rather than quietly returning a plausible number", () => {
  // The whole reason this can't fall back: the caller compares the result against
  // the recorded draw, so any wrong-but-plausible total reads as "the operator
  // resized the pool" and re-randomizes every node. A throw leaves the pool alone.
  for (const junk of ["not json", "[1,2,3]", "null", "{}", '{"total":"3"}', '{"total":2.5}', '{"total":-1}']) {
    assert.throws(() => readDesiredTotal(junk), Error, `expected ${junk} to throw`);
  }
});

test("total wins over stray sibling keys", () => {
  assert.equal(readDesiredTotal('{"total":4,"us-west-2":99}'), 4);
});

test("a quoted total is rejected, not silently read as the legacy shape", () => {
  // `aws ssm put-parameter --value '{"total":"3"}'` is an easy mistake. The old
  // code fell through to the legacy branch, found no numbers, and returned the
  // floor — a silent downscale of the live pool.
  assert.throws(() => readDesiredTotal('{"total":"3"}'), /non-negative integer/);
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

test("a fractional total can never make the draw disagree with the desired total", () => {
  // The permanent-thrash shape: drawPlacement loops `i < 2.5` and draws 3, the
  // predicate compares 3 !== 2.5, and every reconcile re-randomizes the pool
  // forever. readDesiredTotal is the chokepoint that makes it unrepresentable.
  assert.throws(() => readDesiredTotal('{"total":2.5}'), /non-negative integer/);
  for (const total of [1, 2, 3, 7]) {
    const placement = drawPlacement(REGIONS, total, Math.random);
    assert.equal(placementNeedsRedraw({ drawn_at: base.now, placement }, { ...base, total }), null);
  }
});

test("re-draws over a hand-edited placement with impossible counts", () => {
  // The README tells operators to edit this parameter to force a rotation, so a
  // typo is expected. A negative or over-max count that still SUMS correctly used
  // to satisfy the keep condition forever while UpdateAutoScalingGroup threw on
  // every reconcile — wedged, with no way to draw back out.
  const current = { drawn_at: base.now, placement: { "us-east-1": -1, "us-west-2": 4 } };
  assert.match(placementNeedsRedraw(current, base), /malformed count/);
});

test("a drawn_at in the future re-draws instead of freezing rotation forever", () => {
  // now - drawn_at goes negative, so the elapsed check could never fire again and
  // the pool would silently stop rotating.
  const current = { drawn_at: base.now + 86_400, placement: { "us-west-2": 3 } };
  assert.match(placementNeedsRedraw(current, base), /in the future/);
});

// --- mayDrain: a rotation must hand over, not cut over -----------------------

test("does not drain the old regions until the new ones are actually serving", () => {
  // The full-pool-rotation case: the draw moved every node, the incoming regions
  // are still booting. Draining here takes the pool to zero for the several
  // minutes a node needs to pull its image.
  const decision = mayDrain({
    draining: ["us-west-2"],
    perRegionHealthy: { "us-west-2": 3, "us-east-1": 0 },
    poolTotal: 3,
    nodeFloor: 2,
  });
  assert.equal(decision.ok, false);
  assert.equal(decision.retainedHealthy, 0);
});

test("drains once enough replacement capacity is healthy elsewhere", () => {
  const decision = mayDrain({
    draining: ["us-west-2"],
    perRegionHealthy: { "us-west-2": 3, "us-east-1": 2 },
    poolTotal: 3,
    nodeFloor: 2,
  });
  assert.equal(decision.ok, true);
});

test("a pool smaller than the floor can still drain", () => {
  // Otherwise `required` would exceed anything the pool could ever have and the
  // losing regions would never scale down — the pool would only ever grow.
  const decision = mayDrain({
    draining: ["us-west-2"],
    perRegionHealthy: { "us-west-2": 1, "us-east-1": 1 },
    poolTotal: 1,
    nodeFloor: 2,
  });
  assert.equal(decision.ok, true);
  assert.equal(decision.required, 1);
});

test("nothing to drain is never blocked", () => {
  assert.equal(mayDrain({ draining: [], perRegionHealthy: {}, poolTotal: 3, nodeFloor: 2 }).ok, true);
});

test("health inside a region that is leaving does not count as retained", () => {
  // The bug this pins: counting the departing region's own healthy nodes would
  // always satisfy the check and make the guard a no-op.
  const decision = mayDrain({
    draining: ["us-west-2", "us-west-1"],
    perRegionHealthy: { "us-west-2": 2, "us-west-1": 1, "us-east-1": 0 },
    poolTotal: 3,
    nodeFloor: 2,
  });
  assert.equal(decision.ok, false);
  assert.equal(decision.retainedHealthy, 0);
});

// --- clamp -------------------------------------------------------------------

test("clamps desired-state into [floor, max]", () => {
  assert.equal(clamp(0, 2, 3), 2);
  assert.equal(clamp(99, 2, 3), 3);
  assert.equal(clamp(3, 2, 3), 3);
});
