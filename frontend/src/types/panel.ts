// The URL carries WHICH THREAD is being read and nothing else; whether the panel
// is open is device-local state in `stores/rightPanelStore` (#904).

/** The shape the panel is currently rendering. Derived, never parsed. */
export type PanelKind = "members" | "thread" | "pins" | "safety" | "none";

export interface PanelSearch {
  /**
   * ULID of the open thread's root message. Route-scoped on purpose: a thread
   * only means anything inside its conversation, so navigating drops it.
   */
  thread?: string;
  /**
   * `"1"` when the panel is showing the conversation's pinned messages (#99).
   * Route-scoped like `thread`; a thread in the same URL wins.
   */
  pins?: "1";
  /**
   * User id of the peer whose safety number with you is showing (#1563). A
   * safety number belongs to a pair, so the param names the other person, never
   * the conversation. Route-scoped; a thread or pins in the same URL wins.
   */
  safety?: string;
  /**
   * ULID of a message to scroll to and flash on arrival (#850), consumed once.
   *
   * Must be declared here: this is the root route's `validateSearch`, and
   * TanStack Router silently drops every key it does not return. Search also
   * queues the jump through `messageJumpStore` for in-place jumps; the param
   * survives navigation and reload. `MessageList.claimMessage` fires once,
   * whichever arrives first.
   */
  message?: string;
  /** A search query handed off from the Cmd+K navigator to `/search` (#850). */
  q?: string;
}

/**
 * Parse the panel search params off an arbitrary URL. Anything unrecognised is
 * dropped rather than thrown, so a stale or hand-edited link falls back to a
 * plain conversation view instead of a router error.
 */
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
  const safety = search.safety;
  if (typeof safety === "string" && safety.length > 0) {
    out.safety = safety;
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
