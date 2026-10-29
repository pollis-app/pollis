// Right-panel search params (#824, #825). The URL carries only route-scoped
// shape (thread, pins, jump target); open/closed is device-local
// (`stores/rightPanelStore`, #904).

export type PanelKind = "members" | "thread" | "pins" | "none";

export interface PanelSearch {
  /** ULID of the open thread's root; route-scoped, so leaving the conversation drops it. */
  thread?: string;
  /** `"1"` when showing pinned messages (#99); route-scoped. A thread wins. */
  pins?: "1";
  /**
   * ULID of a message to scroll to and flash (#850), consumed once. Must be
   * declared here: TanStack drops any key this root validator doesn't return.
   * Search also queues the jump in `messageJumpStore` (in-place jumps); the
   * param survives navigation and reload. `claimMessage` fires once either way.
   */
  message?: string;
  /** A query handed from Cmd+K to `/search` (#850). */
  q?: string;
}

/** Parse the panel params; unknown values are dropped, never thrown, so a stale link still renders. */
export function validatePanelSearch(
  search: Record<string, unknown>,
): PanelSearch {
  const out: PanelSearch = {};
  const thread = search.thread;
  if (typeof thread === "string" && thread.length > 0) {
    out.thread = thread;
  }
  if (search.pins === "1") {
    out.pins = "1";
  }
  const message = search.message;
  if (typeof message === "string" && message.length > 0) {
    out.message = message;
  }
  const q = search.q;
  if (typeof q === "string" && q.length > 0) {
    out.q = q;
  }
  return out;
}
