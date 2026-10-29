function channels(triplet: string): number[] {
  return triplet.split(",").map((n) => parseInt(n.trim(), 10));
}

/** Opaque equivalent of `rgba(accent, alpha)` over the background (#1193). */
export function compositeOver(accentTriplet: string, alpha: number, bgTriplet: string): string {
  const bg = channels(bgTriplet);
  const mix = channels(accentTriplet).map((c, i) => Math.round(bg[i] * (1 - alpha) + c * alpha));
  return `rgb(${mix.join(", ")})`;
}
