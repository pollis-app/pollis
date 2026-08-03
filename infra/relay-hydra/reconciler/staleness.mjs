// Stale-node detection for the relay pool (#703 / #677).
//
// The pool is split-brained when it holds two relay generations at once: a
// rolling push to a mutable image tag never updates a running node (Docker caches
// by content hash, `--restart=always` never re-pulls), so nodes carry whatever
// image existed when their instance launched. Because the relay bumps its ALPN
// alongside PROTOCOL_VERSION, two generations means QUIC ALPN negotiation simply
// fails against the wrong-generation node — the client can SEE it in the directory
// but cannot USE it.
//
// The relay's GET /version now reports BOTH identities:
//   { service, sha, protocol, protocol_version }
// and this module turns that body into the two decisions the reconciler makes:
//
//   1. Directory membership — keyed on PROTOCOL identity. A node whose protocol
//      does not match the pool's expected one is excluded from the signed
//      directory IMMEDIATELY. Exclusion is instant, free and reversible; the
//      client then never learns the wrong-generation node's address, which is the
//      whole of the split-brain symptom. (Termination takes minutes and costs
//      capacity — so we exclude first and cycle second.)
//
//   2. Cycling — keyed on BUILD identity (sha). A node whose sha does not match
//      the intended build is replaced so the fleet converges on the intended
//      image. An unrelated docs commit changes the sha without changing the
//      protocol, so a build-stale node is NOT necessarily a directory-membership
//      problem — it keeps serving clients until it is cycled.
//
// THE LOAD-BEARING INVARIANT: staleness is a POSITIVE identification of a wrong
// build/protocol, NEVER an absence of evidence. A node we cannot read (no
// /version, unparseable body, no sha) is UNKNOWN, not stale — treating "I can't
// tell" as "wrong build" would cycle healthy nodes on a transient blip and could
// churn the whole pool during any read failure. Every predicate below fails toward
// "keep the node".
//
// Pure functions, no AWS, no network — the reconciler injects the parsed /version
// body, the expected protocol, and the intended sha. This is the exact logic that
// must not misfire, so it lives here where it is unit-testable
// (test/staleness.test.mjs); index.mjs can't be imported under test (its AWS SDK
// deps only exist in the Lambda runtime).

// Parse a /version HTTP body into { sha, protocol, protocolVersion } or null.
//
// Returns null (UNKNOWN, never "stale") for anything we cannot read: a node that
// answers 200 with a body we can't parse is indistinguishable, for staleness
// purposes, from one that didn't answer at all. Old nodes (before /version carried
// a protocol) parse fine but leave `protocol` undefined — see protocolMismatch,
// which fails open on an absent protocol precisely so the /version rollout itself
// doesn't empty the directory.
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
//   - node reports no protocol field          -> false. This is the migration case:
//     a node running a build from BEFORE /version carried a protocol answers 200
//     with just {service, sha}. Excluding every such node the moment this
//     reconciler ships — before a single node has rolled to a protocol-reporting
//     build — would empty the directory pool-wide. Those nodes are instead
//     replaced by BUILD cycling (their sha != intended), and once the fleet
//     reports a protocol a genuine future mismatch (v3 vs a future v4) is a
//     positive disagreement and IS excluded here, instantly.
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

  // The pool must retain at least this many healthy nodes. Never demand more than
  // the pool is even meant to hold, or a pool at/under the floor could never cycle
  // at all (mirrors mayDrain()).
  const required = Math.min(poolTotal, nodeFloor);
  const floorBudget = Math.max(0, healthyTotal - required);
  const budget = Math.max(0, Math.min(maxPerRun, floorBudget));

  return { targets: buildStale.slice(0, budget), buildStale, budget };
}
