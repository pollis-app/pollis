// Right-hand context panel (#824), extended with threads (#825), open state
// moved off the URL in #904.
//
// The URL carries WHICH THREAD is being read and nothing else. Whether the
// panel is open at all is device-local state (`stores/rightPanelStore`),
// because it is a property of this screen and this person, not of a location —
// see that file for the full argument. MobX also holds the ephemeral per-panel
// UI (scroll offset, drafts).

/** The shape the panel is currently rendering. Derived, never parsed. */
export type PanelKind = "members" | "thread" | "none";

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
  const thread = search.thread;
  if (typeof thread !== "string" || thread.length === 0) {
    return {};
  }
  return { thread };
}
