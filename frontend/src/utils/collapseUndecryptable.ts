/**
 * Merge each run of consecutive lost-message placeholders (#1312) into one row.
 *
 * A device that falls out of step usually loses several messages at once, and
 * a column of identical "couldn't be decrypted" rows says nothing the first
 * one doesn't. The run keeps its FIRST placeholder (its id, its time, its place
 * in the log) and records how many it stands for in `undecryptable_count`.
 *
 * `messages` must already be in display order; anything else between two
 * placeholders ends the run.
 */
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
