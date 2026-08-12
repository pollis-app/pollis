// Right-hand context panel (#824), extended with threads (#825).
//
// The URL is the source of truth for WHICH panel is open; MobX holds only
// ephemeral per-panel UI (scroll offset, drafts). That keeps the panel
// linkable, restorable across a reload, and closable with the back button,
// and it keeps the state flow deterministic as more panel kinds land.

/** Panel kinds that may appear in the URL. */
export const PANEL_KINDS = ["members", "thread", "none"] as const;

export type PanelKind = (typeof PANEL_KINDS)[number];

export interface PanelSearch {
  /**
   * Which panel is open.
   *
   * Absent is meaningful and distinct from `"none"`. Absent means "use this
   * user's default for the current skin"; `"none"` is an explicit close that
   * survives a reload and travels in a shared link.
   */
  panel?: PanelKind;
  /**
   * ULID of the open thread's root message. Required iff `panel === "thread"`
   * — [`validatePanelSearch`] refuses to produce one without the other, so
   * "thread panel open with no thread" is unrepresentable downstream.
   */
  thread?: string;
}

function isPanelKind(value: unknown): value is PanelKind {
  return (
    typeof value === "string" &&
    (PANEL_KINDS as readonly string[]).includes(value)
  );
}

/**
 * Parse the panel search params off an arbitrary URL.
 *
 * Anything unrecognised is dropped rather than thrown: a hand-edited, stale,
 * or future-version link should fall back to the user's default panel, never
 * blank the app with a router error.
 *
 * The two halves of the thread variant are validated together, so neither can
 * appear without the other:
 *
 * - `?panel=thread` with no `thread` → dropped entirely (falls back to default)
 * - `?thread=…` with no `panel=thread` → the stray id is dropped
 */
export function validatePanelSearch(
  search: Record<string, unknown>,
): PanelSearch {
  if (!isPanelKind(search.panel)) {
    return {};
  }
  if (search.panel !== "thread") {
    return { panel: search.panel };
  }
  const thread = search.thread;
  if (typeof thread !== "string" || thread.length === 0) {
    return {};
  }
  return { panel: "thread", thread };
}
