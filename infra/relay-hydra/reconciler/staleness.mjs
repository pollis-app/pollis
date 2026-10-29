// Stale-node detection for the relay pool (#703 / #677).
//
// Split brain: running nodes never re-pull a mutable tag, and a wrong-generation node fails ALPN.
//
// GET /version reports { service, sha, protocol, protocol_version }; this makes two decisions:
//
//   1. Directory membership, by PROTOCOL: a mismatch is excluded immediately (instant, reversible).
//   2. Cycling, by BUILD sha: a build-stale node keeps serving until replaced.
//
// INVARIANT: staleness is a POSITIVE identification, never absence of evidence; unreadable means keep.
//
// Pure functions, no AWS (test/staleness.test.mjs).

// Parse a /version HTTP body into { sha, protocol, protocolVersion } or null.
// Returns null (UNKNOWN, never "stale") for anything unreadable; old nodes leave `protocol` undefined.
export function parseVersion(body) {
  if (typeof body !== "string" || body.length === 0) {
    return null;
  }
  let parsed;
  try {
    parsed = JSON.parse(body);
  } catch {
    return null;
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return null;
  }
  return {
    sha: typeof parsed.sha === "string" ? parsed.sha : undefined,
    protocol: typeof parsed.protocol === "string" ? parsed.protocol : undefined,
    protocolVersion:
      typeof parsed.protocol_version === "number" ? parsed.protocol_version : undefined,
  };
}

// A sha that names an actual build. The relay reports "unknown" for a local
// `cargo` build with no baked GIT_SHA; that is not a build we can compare against,
// so it can never make a node stale (nor be the intended target).
function isConcreteSha(sha) {
  return typeof sha === "string" && sha.length > 0 && sha !== "unknown";
}

// Does this node's PROTOCOL identity positively DISAGREE with the pool's expected
// one? Only a positive mismatch excludes a node from the directory.
//
// Fails open on every "can't tell":
//   - expectedProtocol not configured        -> false (don't gate membership)
//   - node unreadable (version === null)      -> false (it also failed the health
//                                                 check, so it isn't in the healthy
//                                                 set being assembled anyway)
//   - node reports no protocol field          -> false: pre-protocol builds are replaced by BUILD cycling.
export function protocolMismatch(version, expectedProtocol) {
  if (!expectedProtocol) {
    return false;
  }
  if (!version || version.protocol === undefined) {
    return false;
  }
  return version.protocol !== expectedProtocol;
}

// Membership is the inverse: a healthy node is directory-eligible unless its
// protocol positively mismatches.
export function directoryEligible(version, expectedProtocol) {
  return !protocolMismatch(version, expectedProtocol);
}

// Is this node positively running the WRONG build? Keyed on sha (build identity).
//
// Fails toward "not stale" on every absence of evidence:
//   - intendedSha unknown (fresh pool, param unset) -> false: with no intended
//     build recorded we cannot positively identify anything as stale, so we cycle
//     nothing. (The directory-membership guard still protects clients.)
//   - node unreadable / no concrete sha             -> false: unreachable != stale.
//   - node sha === intended sha                      -> false: current.
// Only a concrete sha that differs from a known intended sha is stale.
export function isBuildStale(version, intendedSha) {
  if (!isConcreteSha(intendedSha)) {
    return false;
  }
  if (!version || !isConcreteSha(version.sha)) {
    return false;
  }
  return version.sha !== intendedSha;
}

// From the pool's node list, choose which build-stale nodes to cycle THIS run,
// bounded so a roll can never empty the pool.
//
// `nodes` is [{ instanceId, region, version }] over the HEALTHY set (every node
// here answered /version this cycle). Cycling replaces a healthy node, so it must:
//   - only ever target POSITIVELY build-stale nodes (isBuildStale) — never an
//     unreachable one (that is the ASG/self-heal path, and unreachable != stale);
//   - never drop the healthy pool below what the desired count requires. Cycling K
//     nodes drops healthy by K until the ASG relaunches them, so we require
//     healthy-after >= min(poolTotal, nodeFloor);
//   - never cycle more than `maxPerRun` at once, so convergence is gradual.
//
// Returns { targets, buildStale, budget } — `targets` is what to mark Unhealthy,
// `buildStale` is every stale node seen (for metrics/logging), `budget` is how many
// the floor allowed this run.
export function selectCycleTargets({ nodes, intendedSha, healthyTotal, poolTotal, nodeFloor, maxPerRun }) {
  const buildStale = (nodes ?? []).filter((n) => isBuildStale(n.version, intendedSha));

  // Never demand more than the pool is meant to hold (mirrors mayDrain()).
  const required = Math.min(poolTotal, nodeFloor);
  const floorBudget = Math.max(0, healthyTotal - required);
  const budget = Math.max(0, Math.min(maxPerRun, floorBudget));

  return { targets: buildStale.slice(0, budget), buildStale, budget };
}
