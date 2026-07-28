// Random region placement for the relay pool.
//
// The pool's desired-state is a single POOL-WIDE node count; which regions those
// nodes live in is drawn at random, uniformly and WITH REPLACEMENT, so two nodes
// may legitimately land in the same region. The draw is persisted so it stays
// stable between rotations — re-randomizing on every 2-minute reconcile would
// terminate and relaunch the whole pool continuously.
//
// Pure functions, no AWS: the reconciler injects the current state and an rng,
// which is what makes the draw and the rotation policy unit-testable
// (test/placement.test.mjs).

// Parse the desired-state SSM parameter into a pool-wide node count.
//
// Accepts both shapes:
//   {"total": 3}          — current
//   {"us-west-2": 3}      — legacy per-region map, from before placement was
//                           randomized. Summed, because Terraform's
//                           ignore_changes leaves the live parameter untouched
//                           across the upgrade; without this the pool would read
//                           as 0 nodes on the first post-upgrade reconcile.
// THROWS rather than falling back on anything it cannot interpret. A desired
// total that silently defaults is worse than no answer at all: the rotation
// predicate compares it against the recorded draw, so a wrong-but-plausible
// number reads as "the operator resized the pool" and re-randomizes the entire
// pool. A throw stops the reconcile, leaves the pool exactly as it is, and trips
// the Lambda Errors alarm.
export function readDesiredTotal(raw) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new Error(`desired-state is not valid JSON: ${err.message}`);
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`desired-state must be a JSON object, got ${JSON.stringify(parsed)}`);
  }
  if ("total" in parsed) {
    return requirePoolCount(parsed.total, "desired-state total");
  }
  const keys = Object.keys(parsed);
  if (keys.length === 0) {
    throw new Error('desired-state is an empty object — expected {"total": N}');
  }
  // Legacy per-region map, from before placement was randomized. Summed, because
  // Terraform's ignore_changes leaves the live parameter untouched across the
  // upgrade. Logged loudly: the two shapes are easy to confuse, and reading
  // {"us-west-2":3,"us-east-1":3} as a pool of 6 rather than "3 each" is a
  // mistake an operator should be told about, not left to infer from pool size.
  const total = keys.reduce(
    (sum, key) => sum + requirePoolCount(parsed[key], `desired-state legacy entry ${key}`),
    0,
  );
  console.warn(
    `desired-state is in the legacy per-region shape ${JSON.stringify(parsed)}; ` +
      `reading it as a POOL-WIDE total of ${total}. Rewrite it as {"total": ${total}} to silence this.`,
  );
  return total;
}

// Node counts are whole, non-negative and small. Rejecting everything else here
// is what keeps a fractional, NaN or stringified total out of `drawPlacement`,
// where it would make the drawn total permanently unequal to the desired total —
// re-randomizing the pool every two minutes, forever.
function requirePoolCount(value, what) {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) {
    throw new Error(`${what} must be a non-negative integer, got ${JSON.stringify(value)}`);
  }
  return value;
}

// Draw `total` nodes uniformly at random across `regions`, with replacement.
// Returns a region -> count map containing only regions that won at least one
// node, so the shape stays small and readable in SSM.
export function drawPlacement(regions, total, rng = Math.random) {
  if (regions.length === 0) {
    throw new Error("cannot draw placement: no allowed regions");
  }
  const placement = {};
  for (let i = 0; i < total; i += 1) {
    const region = regions[Math.floor(rng() * regions.length) % regions.length];
    placement[region] = (placement[region] ?? 0) + 1;
  }
  return placement;
}

export function placementTotal(placement) {
  return Object.values(placement ?? {}).reduce((a, b) => a + b, 0);
}

// Decide whether the persisted draw still holds. Returns a human-readable reason
// to re-draw, or null to keep the current placement.
//
// Rotation is the ONLY thing that moves a node between regions, so each of these
// is a deliberate trigger:
//   - the interval elapsed        — the scheduled rotation itself
//   - the pool size changed       — an operator scaled it; re-draw to place the
//                                   new total rather than patch the old draw
//   - a region left the allowed set — jurisdiction policy changed under us; the
//                                   nodes there must move immediately
export function placementNeedsRedraw(current, { allowedRegions, total, now, rotationIntervalSeconds }) {
  if (!current || typeof current !== "object" || !current.placement) {
    return "no placement recorded yet";
  }
  const placement = current.placement;
  // Per-region sanity, not just the sum. README tells operators to hand-edit this
  // parameter to force a rotation, so a typo here is expected rather than
  // hypothetical — and a placement whose entries are negative or above an ASG's
  // max_size makes UpdateAutoScalingGroup throw on every reconcile. Without this
  // check the sum could still match, the predicate would return "keep it", and
  // the reconciler would wedge with no way to draw itself back out.
  const malformed = Object.entries(placement).filter(
    ([, n]) => typeof n !== "number" || !Number.isInteger(n) || n < 0,
  );
  if (malformed.length > 0) {
    return `placement has malformed count(s): ${malformed.map(([r, n]) => `${r}=${JSON.stringify(n)}`).join(", ")}`;
  }
  if (placementTotal(placement) !== total) {
    return `pool size changed (placed ${placementTotal(placement)}, desired ${total})`;
  }
  const allowed = new Set(allowedRegions);
  const stranded = Object.keys(placement).filter((r) => placement[r] > 0 && !allowed.has(r));
  if (stranded.length > 0) {
    return `placement holds no-longer-allowed region(s): ${stranded.join(", ")}`;
  }
  // A drawn_at in the future (clock skew, or a typo'd hand-edit) would make the
  // elapsed check negative and freeze rotation permanently — silently, since
  // nothing else here depends on the timestamp. Treat it as due instead: a draw
  // that claims to have happened after now is not a draw we can age.
  const drawnAt = typeof current.drawn_at === "number" ? current.drawn_at : 0;
  if (drawnAt > now) {
    return `placement drawn_at ${drawnAt} is in the future (now ${now}) — re-drawing rather than never rotating`;
  }
  if (now - drawnAt >= rotationIntervalSeconds) {
    return `rotation interval elapsed (${now - drawnAt}s since last draw)`;
  }
  return null;
}

export function clamp(n, lo, hi) {
  return Math.max(lo, Math.min(hi, n));
}

// May the regions in `draining` be scaled down yet?
//
// A rotation moves nodes between regions, and with a small pool over four regions
// a draw disjoint from the current placement is routine — so scaling the losers to
// zero in the same pass that the winners scale up from zero cold-starts the ENTIRE
// pool. Nodes take minutes to boot and pull an image, so that is an outage on a
// timer. Hold the old nodes until enough new ones are actually answering.
//
// Kept here, pure, because it is the part that decides whether the pool briefly
// costs double or briefly serves nothing — index.mjs can't be imported under test
// (its AWS SDK deps only exist in the Lambda runtime).
export function mayDrain({ draining, perRegionHealthy, poolTotal, nodeFloor }) {
  if (draining.length === 0) {
    return { ok: true, retainedHealthy: 0, required: 0 };
  }
  const leaving = new Set(draining);
  const retainedHealthy = Object.entries(perRegionHealthy)
    .filter(([region]) => !leaving.has(region))
    .reduce((sum, [, count]) => sum + count, 0);
  // Never demand more than the pool is even meant to hold, or a pool smaller than
  // the floor could never drain at all.
  const required = Math.min(poolTotal, nodeFloor);
  return { ok: retainedHealthy >= required, retainedHealthy, required };
}
