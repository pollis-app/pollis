// The DM list's live re-sort (#1838). The order itself is pollis-core's: `list_dm_channels`
// returns DMs newest first by their latest local message (else their creation time), ties
// broken by id. This only re-applies that order the moment a message is sent or received,
// before the next list read. Parity twin: mobile/lib/dmActivity.ts is this file, byte for byte.

/** The fields the order reads; `updated_at` is the latest activity in ms since the epoch. */
export interface ActivityRow {
  id: string;
  updated_at: number;
}

/** A stored timestamp as ms since the epoch; an unreadable one is 0 and sorts last, as in the core. */
export function activityMs(stamp: string | null | undefined): number {
  const ms = stamp ? Date.parse(stamp) : NaN;
  return Number.isFinite(ms) ? ms : 0;
}

/** `list_dm_channels`'s order: newest activity first, ties by id descending. */
export function byActivity(a: ActivityRow, b: ActivityRow): number {
  if (a.updated_at !== b.updated_at) {
    return b.updated_at - a.updated_at;
  }
  if (a.id === b.id) {
    return 0;
  }
  return a.id < b.id ? 1 : -1;
}

/**
 * `rows` with DM `id`'s activity raised to `at` and re-sorted. Activity only moves forward, so
 * an older stamp (an edit, a replay, a read of an old DM) returns `rows` itself, unchanged.
 */
export function bumpActivity<T extends ActivityRow>(
  rows: T[] | undefined,
  id: string,
  at: number,
): T[] | undefined {
  if (!rows) {
    return rows;
  }
  const i = rows.findIndex((r) => r.id === id);
  if (i < 0 || rows[i].updated_at >= at) {
    return rows;
  }
  const next = rows.slice();
  next[i] = { ...rows[i], updated_at: at };
  return next.sort(byActivity);
}
