/** Suffix duplicate display names ` (1)`, ` (2)`… in join order (#140); identity → label. */
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
