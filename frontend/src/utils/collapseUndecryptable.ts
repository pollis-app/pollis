/** Merge runs of undecryptable placeholders (#1312) into the first, with `undecryptable_count`. Input in display order. */
export function collapseUndecryptable<T extends { undecryptable?: boolean; undecryptable_count?: number }>(
  messages: T[],
): T[] {
  const out: T[] = [];
  for (const m of messages) {
    const prev = out[out.length - 1];
    if (m.undecryptable && prev?.undecryptable) {
      out[out.length - 1] = { ...prev, undecryptable_count: (prev.undecryptable_count ?? 1) + 1 };
      continue;
    }
    out.push(m.undecryptable ? { ...m, undecryptable_count: 1 } : m);
  }
  return out;
}
