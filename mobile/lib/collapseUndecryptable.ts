/**
 * Merge each run of consecutive lost-message placeholders (#1312) into one row
 * — desktop's `frontend/src/utils/collapseUndecryptable.ts`, copied because
 * mobile imports no frontend TypeScript.
 *
 * The run keeps its FIRST placeholder (id, time, place in the timeline) and
 * records how many it stands for in `undecryptableCount`. `messages` must be in
 * display order; anything else between two placeholders ends the run.
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
