// Right-hand context panel (#824).
//
// The URL is the source of truth for WHICH panel is open; MobX holds only
// ephemeral per-panel UI (scroll offset, drafts). That keeps the panel
// linkable, restorable across a reload, and closable with the back button,
// and it keeps the state flow deterministic as more panel kinds land.
//
// `panel` is a discriminated union rather than a boolean from the start, so
// the thread panel (#825) arrives as an additive variant instead of a rewrite.

/** Panel kinds that may appear in the URL. */
export const PANEL_KINDS = ["members", "none"] as const;

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
 */
export function validatePanelSearch(
  search: Record<string, unknown>,
): PanelSearch {
  return isPanelKind(search.panel) ? { panel: search.panel } : {};
}
