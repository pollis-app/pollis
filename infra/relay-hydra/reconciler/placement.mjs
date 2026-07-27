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
export function readDesiredTotal(raw, fallback) {
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return fallback;
  }
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
    return fallback;
  }
  if (typeof parsed.total === "number" && Number.isFinite(parsed.total)) {
    return parsed.total;
  }
  const counts = Object.values(parsed).filter((v) => typeof v === "number" && Number.isFinite(v));
  if (counts.length === 0) {
    return fallback;
  }
  return counts.reduce((a, b) => a + b, 0);
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
  if (placementTotal(placement) !== total) {
    return `pool size changed (placed ${placementTotal(placement)}, desired ${total})`;
  }
  const allowed = new Set(allowedRegions);
  const stranded = Object.keys(placement).filter((r) => placement[r] > 0 && !allowed.has(r));
  if (stranded.length > 0) {
    return `placement holds no-longer-allowed region(s): ${stranded.join(", ")}`;
  }
  const drawnAt = typeof current.drawn_at === "number" ? current.drawn_at : 0;
  if (now - drawnAt >= rotationIntervalSeconds) {
    return `rotation interval elapsed (${now - drawnAt}s since last draw)`;
  }
  return null;
}

export function clamp(n, lo, hi) {
  return Math.max(lo, Math.min(hi, n));
}
