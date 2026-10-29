/**
 * Merge each run of consecutive lost-message placeholders (#1312) into its
 * FIRST row, counting the run in `undecryptableCount`; a copy of desktop's
 * `collapseUndecryptable.ts`. `messages` must be in display order.
 */
export function collapseUndecryptable<T extends { undecryptable?: boolean; undecryptableCount?: number }>(
  messages: T[],
): T[] {
  const out: T[] = [];
  for (const m of messages) {
    const prev = out[out.length - 1];
    if (m.undecryptable && prev?.undecryptable) {
      out[out.length - 1] = { ...prev, undecryptableCount: (prev.undecryptableCount ?? 1) + 1 };
      continue;
    }
    out.push(m.undecryptable ? { ...m, undecryptableCount: 1 } : m);
  }
  return out;
}
