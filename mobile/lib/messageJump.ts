// Jumping the open conversation to one message (a pin, a saved message, a
// permalink). Desktop queues the target on `messageJumpStore` and lets the
// list claim it once the message is rendered; mobile does the same
// (`stores/messageJumpStore.ts`), and this is the decision the chat screen
// makes on every render while a jump is pending. Pure, for the node tests.

/** Older pages to walk before giving up — 20 × 50 = the newest 1000 messages. */
export const JUMP_MAX_PAGES = 20;

export type JumpStep =
  // The row is in the timeline: scroll to it and claim the jump.
  | { kind: "scroll"; index: number }
  // Not loaded yet, and older history exists: fetch the next page.
  | { kind: "loadOlder" }
  // A page is in flight (or the first one is): decide on the next render.
  | { kind: "wait" }
  // Nowhere to go: loaded but not a timeline row (a thread reply), or the
  // history ran out (or past the page cap) without it. Drop the jump.
  | { kind: "giveUp" };

export function nextJumpStep(s: {
  // Index of the message among the rendered timeline rows, -1 if absent.
  index: number;
  // Whether the loaded pages hold the message at all (rendered or not).
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
