// The stale-node detection predicate (reconciler/staleness.mjs) — #703.
// Pure logic over a parsed /version body + the pool's expected identity. This is
// the thing that MUST NOT misfire: a false positive cycles a healthy node or
// hides it from clients, so the tests lean hard on the "positive identification,
// never absence of evidence" invariant.
//
// Run: node --test   (from infra/relay-hydra/)

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  parseVersion,
  protocolMismatch,
  directoryEligible,
  isBuildStale,
  selectCycleTargets,
} from "../reconciler/staleness.mjs";

const PROTO = "pollis-relay/3";

// --- parseVersion ------------------------------------------------------------

test("parses a full /version body", () => {
  const v = parseVersion('{"service":"pollis-relay","sha":"abc123","protocol":"pollis-relay/3","protocol_version":3}');
  assert.deepEqual(v, { sha: "abc123", protocol: "pollis-relay/3", protocolVersion: 3 });
});

test("parses an OLD body with no protocol field (pre-#703 node)", () => {
  const v = parseVersion('{"service":"pollis-relay","sha":"old999"}');
  assert.equal(v.sha, "old999");
  assert.equal(v.protocol, undefined);
  assert.equal(v.protocolVersion, undefined);
});

test("returns null for anything unreadable — UNKNOWN, never stale", () => {
  for (const junk of ["", "not json", "[1,2,3]", "null", "42", '"a string"']) {
    assert.equal(parseVersion(junk), null, `expected ${JSON.stringify(junk)} -> null`);
  }
  assert.equal(parseVersion(undefined), null);
});

test("tolerates wrong-typed fields by dropping them, not throwing", () => {
  const v = parseVersion('{"sha":123,"protocol":true,"protocol_version":"3"}');
  assert.deepEqual(v, { sha: undefined, protocol: undefined, protocolVersion: undefined });
});

// --- protocolMismatch / directoryEligible ------------------------------------

test("a positive protocol mismatch excludes the node", () => {
  const v = { sha: "x", protocol: "pollis-relay/2" };
  assert.equal(protocolMismatch(v, PROTO), true);
  assert.equal(directoryEligible(v, PROTO), false);
});

test("a matching protocol is eligible", () => {
  const v = { sha: "x", protocol: PROTO };
  assert.equal(protocolMismatch(v, PROTO), false);
  assert.equal(directoryEligible(v, PROTO), true);
});

test("ABSENT protocol fails OPEN — keeps the node (migration case)", () => {
  // A node from before /version carried a protocol answers 200 with just {sha}.
  // Excluding it would empty the directory pool-wide the instant this reconciler
  // ships, before a single node has rolled. It's kept here and cycled on BUILD.
  const v = { sha: "old999" };
  assert.equal(protocolMismatch(v, PROTO), false);
  assert.equal(directoryEligible(v, PROTO), true);
});

test("an unreadable node (null version) is not a positive mismatch", () => {
  assert.equal(protocolMismatch(null, PROTO), false);
});

test("no expected protocol configured never gates membership", () => {
  assert.equal(protocolMismatch({ protocol: "anything" }, ""), false);
  assert.equal(directoryEligible({ protocol: "anything" }, ""), true);
});

// --- isBuildStale ------------------------------------------------------------

test("a concrete sha that differs from the intended sha is stale", () => {
  assert.equal(isBuildStale({ sha: "old000" }, "new111"), true);
});

test("a node on the intended sha is not stale", () => {
  assert.equal(isBuildStale({ sha: "new111" }, "new111"), false);
});

test("UNKNOWN intended sha never makes anything stale (fresh pool)", () => {
  // No intended build recorded => cannot positively identify staleness => cycle
  // nothing. Guards a fresh pool from churning before CI has published.
  for (const intended of [null, undefined, "", "unknown"]) {
    assert.equal(isBuildStale({ sha: "whatever" }, intended), false, `intended=${JSON.stringify(intended)}`);
  }
});

test("an unreachable / sha-less node is never stale (unreachable != stale)", () => {
  assert.equal(isBuildStale(null, "new111"), false);
  assert.equal(isBuildStale({}, "new111"), false);
  assert.equal(isBuildStale({ sha: "" }, "new111"), false);
  assert.equal(isBuildStale({ sha: "unknown" }, "new111"), false);
});

// --- selectCycleTargets: bounded + floor-guarded -----------------------------

const stale = (id, sha = "old") => ({ instanceId: id, region: "us-west-2", version: { sha, protocol: PROTO } });
const current = (id) => ({ instanceId: id, region: "us-west-2", version: { sha: "new", protocol: PROTO } });

test("cycles at most maxPerRun stale nodes", () => {
  const nodes = [stale("i-1"), stale("i-2"), stale("i-3")];
  const { targets, buildStale } = selectCycleTargets({
    nodes, intendedSha: "new", healthyTotal: 5, poolTotal: 5, nodeFloor: 2, maxPerRun: 1,
  });
  assert.equal(buildStale.length, 3);
  assert.equal(targets.length, 1);
});

test("never cycles below the floor (healthy - K >= min(poolTotal, floor))", () => {
  // 2 healthy, floor 2 => required 2 => budget 0. A stale node is SEEN but not
  // cycled this run: dropping to 1 healthy would breach the floor.
  const nodes = [stale("i-1"), current("i-2")];
  const { targets, buildStale } = selectCycleTargets({
    nodes, intendedSha: "new", healthyTotal: 2, poolTotal: 3, nodeFloor: 2, maxPerRun: 5,
  });
  assert.equal(buildStale.length, 1);
  assert.equal(targets.length, 0);
});

test("cycles once there is headroom above the floor", () => {
  // 3 healthy, floor 2 => required 2 => budget 1.
  const nodes = [stale("i-1"), current("i-2"), current("i-3")];
  const { targets } = selectCycleTargets({
    nodes, intendedSha: "new", healthyTotal: 3, poolTotal: 3, nodeFloor: 2, maxPerRun: 5,
  });
  assert.equal(targets.length, 1);
  assert.equal(targets[0].instanceId, "i-1");
});

test("a pool at/under the floor can still cycle (required capped at poolTotal)", () => {
  // poolTotal 1, floor 2 => required min(1,2)=1 => with 2 healthy, budget 1.
  const nodes = [stale("i-1"), current("i-2")];
  const { targets } = selectCycleTargets({
    nodes, intendedSha: "new", healthyTotal: 2, poolTotal: 1, nodeFloor: 2, maxPerRun: 5,
  });
  assert.equal(targets.length, 1);
});

test("nothing stale => no targets, even with budget", () => {
  const nodes = [current("i-1"), current("i-2"), current("i-3")];
  const { targets, buildStale } = selectCycleTargets({
    nodes, intendedSha: "new", healthyTotal: 3, poolTotal: 3, nodeFloor: 2, maxPerRun: 5,
  });
  assert.equal(buildStale.length, 0);
  assert.equal(targets.length, 0);
});

test("unreachable/unknown-sha nodes are never selected", () => {
  const nodes = [
    { instanceId: "i-dead", region: "us-west-2", version: null },
    { instanceId: "i-unknown", region: "us-west-2", version: { sha: "unknown", protocol: PROTO } },
    stale("i-stale"),
  ];
  const { targets, buildStale } = selectCycleTargets({
    nodes, intendedSha: "new", healthyTotal: 3, poolTotal: 3, nodeFloor: 1, maxPerRun: 5,
  });
  assert.deepEqual(buildStale.map((n) => n.instanceId), ["i-stale"]);
  assert.deepEqual(targets.map((n) => n.instanceId), ["i-stale"]);
});

test("no intended sha => nothing is stale => nothing cycled", () => {
  const nodes = [stale("i-1"), stale("i-2")];
  const { targets, buildStale } = selectCycleTargets({
    nodes, intendedSha: null, healthyTotal: 5, poolTotal: 5, nodeFloor: 2, maxPerRun: 5,
  });
  assert.equal(buildStale.length, 0);
  assert.equal(targets.length, 0);
});

// --- the two axes are independent: a wrong-protocol node is also build-stale --

test("wrong-generation node: excluded from directory AND build-stale", () => {
  // The real split-brain node — old build, old ALPN. Both axes fire: it is hidden
  // from clients immediately (protocol) and cycled out (build).
  const v = { sha: "old000", protocol: "pollis-relay/2" };
  assert.equal(directoryEligible(v, PROTO), false);
  assert.equal(isBuildStale(v, "new111"), true);
});

test("docs-only roll: same protocol, new sha — stays in directory, still cycled", () => {
  // Not a split-brain node (ALPN unchanged) so it keeps serving clients, but the
  // fleet must still converge onto the intended build, so it is cycled.
  const v = { sha: "old000", protocol: PROTO };
  assert.equal(directoryEligible(v, PROTO), true);
  assert.equal(isBuildStale(v, "new111"), true);
});
