// Random region placement for the relay pool.
//
// Regions are drawn uniformly WITH replacement and persisted, so a reconcile does not relaunch the pool.
// Pure functions, no AWS (test/placement.test.mjs).

// Parse the desired-state SSM parameter into a pool-wide node count.
//
// Accepts both shapes:
//   {"total": 3}          — current
//   {"us-west-2": 3}      — legacy per-region map, summed (Terraform ignore_changes kept it).
// THROWS rather than falling back: a wrong-but-plausible total re-randomizes the whole pool.
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
  // Legacy per-region map, summed; logged loudly since the two shapes are easy to confuse.
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
  // Per-region sanity, not just the sum: a hand-edited negative or over-max entry would wedge the reconciler.
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
  // A future drawn_at (skew, typo) would freeze rotation; treat it as due.
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
// Scaling losers to zero while winners cold-start takes the pool out; hold old nodes until new ones answer.
//
// `draining` maps region -> POST-DRAIN TARGET: counting 2 -> 1 as a total loss made the guard unsatisfiable (2026-09-01).
//
// So each draining region contributes min(healthy there, its target) — the nodes
// that survive the drain — while a region leaving entirely (target 0) contributes
// nothing, which is the original, correct guard.
export function mayDrain({ draining, perRegionHealthy, poolTotal, nodeFloor }) {
  const targets = draining ?? {};
  const regions = Object.keys(targets);
  if (regions.length === 0) {
    return { ok: true, retainedHealthy: 0, required: 0 };
  }
  const retainedHealthy = Object.entries(perRegionHealthy).reduce((sum, [region, healthy]) => {
    if (!(region in targets)) {
      return sum + healthy;
    }
    // A drain never removes more than it has to: the region ends at its target, so
    // that many of its healthy nodes (at most) are still serving afterwards.
    return sum + Math.min(healthy, targets[region]);
  }, 0);
  // Never demand more than the pool is even meant to hold, or a pool smaller than
  // the floor could never drain at all.
  const required = Math.min(poolTotal, nodeFloor);
  return { ok: retainedHealthy >= required, retainedHealthy, required };
}
