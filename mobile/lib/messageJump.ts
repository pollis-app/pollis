// Jumping the open conversation to one message: the chat screen's per-render decision while a jump is pending. Pure.

export const JUMP_MAX_PAGES = 20;

export type JumpStep =
  | { kind: "scroll"; index: number }
  | { kind: "loadOlder" }
  | { kind: "wait" }
  // Loaded but not a timeline row, or history ran out: drop the jump.
  | { kind: "giveUp" };

export function nextJumpStep(s: {
  // -1 if absent.
  index: number;
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
