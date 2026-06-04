/**
 * Disambiguate display names for a voice participant list. When two or more
 * participants share a display name — the multi-device case (#140), e.g. one
 * user present from two devices — the first keeps the bare name and each
 * subsequent one gets a ` (1)`, ` (2)`, … suffix in list (join) order.
 *
 * Purely presentational: the underlying participant `identity` is always the
 * stable `voice-{userId}:{deviceId}` and is never touched. Returns a map keyed
 * by participant identity → the label to render.
 */
export function disambiguateVoiceNames(
  participants: ReadonlyArray<{ identity: string; name: string }>,
): Map<string, string> {
  const seen = new Map<string, number>();
  const labels = new Map<string, string>();
  for (const p of participants) {
    const count = seen.get(p.name) ?? 0;
    labels.set(p.identity, count === 0 ? p.name : `${p.name} (${count})`);
    seen.set(p.name, count + 1);
  }
  return labels;
}
