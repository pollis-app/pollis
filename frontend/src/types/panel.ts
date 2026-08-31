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
  /**
   * ULID of a message to scroll to and flash on arrival (#850).
   *
   * Route-scoped for the same reason `thread` is, and consumed once: the log
   * clears it as soon as it has jumped, so a back-navigation does not re-fire
   * the flash. This param has to be declared HERE or it does not exist at all
   * — this validator is the root route's `validateSearch`, and TanStack Router
   * drops every key it does not return, silently.
   *
   * Search also queues the same jump through `messageJumpStore` before it
   * navigates. The store is what makes an in-place jump work (no navigation, so
   * no new URL); the param is what makes the jump survive a real navigation and
   * a reload. Whichever arrives first, `MessageList.claimMessage` fires exactly
   * once.
   */
  message?: string;
  /**
   * A search query handed off from the Cmd+K navigator to `/search` (#850).
   *
   * Cmd+K stays a navigator; when what someone typed looks like words rather
   * than a name, its first row hands the query to the full-text page. Carrying
   * it on the URL is what stops that handoff from asking them to type it twice.
   */
  q?: string;
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
