// Following new messages in a chat log (#2125). Pure, so the rules are pinned by tests/chat-follow.test.ts.
// Desktop follows the same rules in its own message list.

/** Within this many points of the newest message, the reader is "at the bottom". */
export const NEAR_BOTTOM_PT = 64;

/** More arrivals than this at once is a catch-up (a reconnect, a resume), not a live message: none rise. */
export const MAX_RISING = 3;

export interface ArrivalInput {
  /** The log's message ids before the change, NEWEST FIRST; null on the first render of a log. */
  prev: readonly string[] | null;
  /** The ids now, newest first. */
  next: readonly string[];
  /** The reader was at the bottom when the change landed. */
  atBottom: boolean;
  /** Whether an id is the reader's own message. */
  isOwn: (id: string) => boolean;
}

export interface Arrivals {
  /** Rows that rise and fade in. */
  rise: string[];
  /** Messages from others the reader has not seen: they add to the "N new messages" count. */
  unseen: number;
  /** Scroll to the bottom: the reader was there, or just sent a message. */
  follow: boolean;
}

const NONE: Arrivals = { rise: [], unseen: 0, follow: false };

/**
 * What new rows at the newest end of the log do. The first load, an older page (added at the
 * oldest end) and a sent message's stub swapping for its stored row (one id out, one in) never
 * count as arrivals.
 */
export function classifyArrivals({ prev, next, atBottom, isOwn }: ArrivalInput): Arrivals {
  if (prev === null || prev.length === 0) {
    return NONE;
  }
  const before = new Set(prev);
  const now = new Set(next);
  // The newest row both lists still hold: everything newer than it in `next` is new.
  const anchor = next.findIndex((id) => before.has(id));
  if (anchor < 0) {
    return NONE;
  }
  const fresh = next.slice(0, anchor).filter((id) => !before.has(id));
  if (fresh.length === 0) {
    return NONE;
  }
  // Rows newer than the anchor that left: each one is a stub its stored row replaced.
  let swaps = prev.slice(0, prev.indexOf(next[anchor])).filter((id) => !now.has(id)).length;
  // Oldest fresh row first, the order a swap's replacement lands in.
  const arrived: string[] = [];
  for (const id of [...fresh].reverse()) {
    if (swaps > 0) {
      swaps -= 1;
      continue;
    }
    arrived.push(id);
  }
  if (arrived.length === 0) {
    return NONE;
  }
  const own = arrived.some(isOwn);
  const follow = atBottom || own;
  const rise = atBottom && arrived.length <= MAX_RISING ? arrived : [];
  const unseen = atBottom || own ? 0 : arrived.filter((id) => !isOwn(id)).length;
  return { rise, unseen, follow };
}

/** The reader's distance from the newest message, in an inverted list (its offset) or a plain one. */
export function distanceFromBottom(
  offset: number,
  inverted: boolean,
  contentHeight = 0,
  viewportHeight = 0,
): number {
  if (inverted) {
    return Math.max(0, offset);
  }
  return Math.max(0, contentHeight - viewportHeight - offset);
}

export function isNearBottom(distance: number): boolean {
  return distance <= NEAR_BOTTOM_PT;
}
