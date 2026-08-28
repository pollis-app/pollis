// Right-hand context panel (#824), extended with threads (#825), open state
// moved off the URL in #904.
//
// The URL carries WHICH THREAD is being read and nothing else. Whether the
// panel is open at all is device-local state (`stores/rightPanelStore`),
// because it is a property of this screen and this person, not of a location —
// see that file for the full argument. MobX also holds the ephemeral per-panel
// UI (scroll offset, drafts).

/** The shape the panel is currently rendering. Derived, never parsed. */
export type PanelKind = "members" | "thread" | "pins" | "none";

export interface PanelSearch {
  /**
   * ULID of the open thread's root message.
   *
   * Route-scoped on purpose: a thread only means anything inside the
   * conversation it hangs off, so leaving the conversation should leave the
   * thread behind. Navigating drops it, which is the correct behaviour here
   * and precisely the wrong one for the open state that used to sit beside it.
   */
  thread?: string;
  /**
   * `"1"` when the panel is showing the conversation's pinned messages (#99).
   *
   * Route-scoped for the same reason as `thread`: pins belong to the
   * conversation being read, so navigating away drops back to the roster.
   * A thread in the same URL wins — reading a thread is the more specific
   * intent.
   */
  pins?: "1";
}

/**
 * Parse the panel search params off an arbitrary URL.
 *
 * Anything unrecognised is dropped rather than thrown: a hand-edited, stale,
 * or future-version link should fall back to a plain conversation view, never
 * blank the app with a router error.
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
  return out;
}
