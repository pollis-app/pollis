// Pure colour math, free of react-native so `node --test` can load it.

/** "r, g, b" triplet → [r, g, b]. */
function channels(triplet: string): number[] {
  return triplet.split(",").map((n) => parseInt(n.trim(), 10));
}

/**
 * An accent tier at `alpha`, pre-composited over the opaque background
 * `bgTriplet`. Same colour as `rgba(accent, alpha)` drawn on that background,
 * but opaque — so a surface painted with it hides whatever is beneath (#1193).
 */
export function compositeOver(accentTriplet: string, alpha: number, bgTriplet: string): string {
  const bg = channels(bgTriplet);
  const mix = channels(accentTriplet).map((c, i) => Math.round(bg[i] * (1 - alpha) + c * alpha));
  return `rgb(${mix.join(", ")})`;
}
