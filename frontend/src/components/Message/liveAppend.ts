// Which rows of a log update arrived live at its end (#2125): those slide in when the reader is at
// the bottom, and count toward "N new messages" when they have scrolled up. Pure, so
// `tests/live-append.test.ts` pins it; mobile's twin is `mobile/lib/followNewest.ts`.

export interface LogSnapshot {
  /** Which conversation the rows belong to. */
  logKey: string;
  /** Row keys, oldest first. */
  keys: readonly string[];
}

export interface AppendContext {
  /** The reader was at the bottom just before this update. */
  atBottom: boolean;
  /** The log holds the live tail, not an anchored window on older history (#1039). */
  liveTail: boolean;
  /** Rows the reader wrote themselves never count as unseen. */
  isOwn: (key: string) => boolean;
}

export interface AppendResult {
  /** Rows to slide in. */
  animate: string[];
  /** Rows the reader has not seen yet (scrolled up). */
  unseen: number;
  /** The reader just sent one of them while scrolled up: take them to it, as Slack does. */
  jumpToEnd: boolean;
}

const NONE: AppendResult = { animate: [], unseen: 0, jumpToEnd: false };

/** The keys appended after the last row both snapshots share, minus rows that replace a vanished one. */
export function appendedKeys(prev: readonly string[], next: readonly string[]): string[] {
  if (prev.length === 0) {
    return [];
  }
  const before = new Set(prev);
  let anchor = -1;
  for (let i = next.length - 1; i >= 0; i -= 1) {
    if (before.has(next[i])) {
      anchor = i;
      break;
    }
  }
  // Nothing in common: the log was replaced (a jump, a re-open), not appended to.
  if (anchor < 0) {
    return [];
  }
  const tail = next.slice(anchor + 1).filter((k) => !before.has(k));
  // A row that vanished in the same update was swapped for one of these (an optimistic stub for
  // its confirmed row): the replacement already showed, so it does not arrive again.
  const after = new Set(next);
  let swapped = prev.filter((k) => !after.has(k)).length;
  const fresh: string[] = [];
  for (const key of tail) {
    if (swapped > 0) {
      swapped -= 1;
      continue;
    }
    fresh.push(key);
  }
  return fresh;
}

/** What a log update means for the reader. */
export function classifyAppend(
  prev: LogSnapshot | null,
  next: LogSnapshot,
  ctx: AppendContext,
): AppendResult {
  if (!prev || prev.logKey !== next.logKey || !ctx.liveTail) {
    return NONE;
  }
  const fresh = appendedKeys(prev.keys, next.keys);
  if (fresh.length === 0) {
    return NONE;
  }
  if (ctx.atBottom) {
    return { animate: fresh, unseen: 0, jumpToEnd: false };
  }
  if (fresh.some(ctx.isOwn)) {
    return { animate: [], unseen: 0, jumpToEnd: true };
  }
  return { animate: [], unseen: fresh.length, jumpToEnd: false };
}

/** At the bottom: within `thresholdPx` of the end. */
export function isAtBottom(
  el: { scrollHeight: number; scrollTop: number; clientHeight: number },
  thresholdPx: number,
): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= thresholdPx;
}
