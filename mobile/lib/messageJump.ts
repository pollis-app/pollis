// Jumping the open conversation to one message (pin, saved, permalink): the
// decision the chat screen makes each render while `stores/messageJumpStore.ts`
// holds a target, as on desktop. Pure, for the node tests.

/** Older pages to walk before giving up — 20 × 50 = the newest 1000 messages. */
export const JUMP_MAX_PAGES = 20;

export type JumpStep =
  | { kind: "scroll"; index: number }
  | { kind: "loadOlder" }
  // A page is in flight: decide on the next render.
  | { kind: "wait" }
  // Loaded but not a timeline row (a thread reply), or history (or the page
  // cap) ran out without it.
  | { kind: "giveUp" };

export function nextJumpStep(s: {
  // Among the rendered timeline rows; -1 if absent.
  index: number;
  // Whether the loaded pages hold it at all, rendered or not.
  loaded: boolean;
  isLoading: boolean;
  isFetchingNextPage: boolean;
  hasNextPage: boolean;
  pageCount: number;
}): JumpStep {
  if (s.index >= 0) {
    return { kind: "scroll", index: s.index };
  }
  if (s.loaded) {
    return { kind: "giveUp" };
  }
  if (s.isLoading || s.isFetchingNextPage) {
    return { kind: "wait" };
  }
  if (s.hasNextPage && s.pageCount < JUMP_MAX_PAGES) {
    return { kind: "loadOlder" };
  }
  return { kind: "giveUp" };
}
