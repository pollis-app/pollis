/**
 * Geometry for the two draggable side panels — the left `Sidebar` and the
 * right `RightPanel`.
 *
 * Pure on purpose. The interesting part of a resize is not the pointer
 * plumbing, it is the answer to "what width is this allowed to be, given the
 * window and the other panel" — so that answer lives here as arithmetic and is
 * covered by `frontend/tests/panel-width.test.ts` without a browser. The DOM
 * half (`ResizeHandle`) only measures and applies.
 *
 * ## Why the widths are device-local `localStorage`, not user preferences
 *
 * A panel width answers "how much of THIS screen is worth spending on the
 * channel list". A 13" laptop and a 34" ultrawide want different numbers, and
 * syncing would force one machine's answer onto the other — the same reasoning
 * that keeps the right panel's open/closed state out of the synced blob
 * (`stores/rightPanelStore.ts`), the device language (`i18n/storage.ts`) and
 * the device font size. It is emphatically not a database row: nothing on the
 * server has any use for it, and a width arriving over the network could move
 * a panel the user is looking at.
 *
 * Unlike the right panel's open/closed state this is NOT keyed by user id.
 * That state is seeded from a synced preference, so it already waits on a
 * query and on a signed-in user; a width has to be on the element at FIRST
 * PAINT or the user watches the layout jump from the default to their own
 * measure. At first paint there is no user id yet, so the key is per device
 * only.
 */

/**
 * Smallest width a panel may hold. Dragging narrower does not produce a
 * useless sliver — it collapses the panel to the same hidden state `mod+b` /
 * `mod+shift+b` produce.
 *
 * px, and deliberately not `rem`: this is a "can a human still hit it with a
 * mouse" bound, which does not scale with the reader's font preference.
 */
export const MIN_PANEL_PX = 50;

/**
 * Pixels the centre column always keeps. With one panel open this makes the
 * maximum exactly `window width − 50`; with BOTH open each panel's maximum is
 * reduced by the width the other actually occupies, so the two together can
 * never squeeze the conversation to nothing. Whoever is dragging is bounded by
 * the space that is genuinely free — no arbitrary "half the window each" split
 * that would stop a user widening one panel while the other is a stub.
 */
export const MIN_CONTENT_PX = 50;

/** `localStorage` keys. Namespaced like the sidebar's other device-local keys. */
export const SIDEBAR_WIDTH_KEY = "pollis.layout.sidebarWidthPx";
export const RIGHT_PANEL_WIDTH_KEY = "pollis.layout.rightPanelWidthPx";

/** What a drag settles on: a concrete width, or the collapsed state. */
export type DragOutcome = { kind: "collapse" } | { kind: "width"; px: number };

/**
 * The widest `windowPx` allows for one panel while the other holds
 * `otherPanelPx` (0 when it is closed) and the centre column keeps its floor.
 *
 * Never returns less than `MIN_PANEL_PX`: a window too small to satisfy
 * everyone should leave a panel at its minimum, not collapse something the
 * user opened on purpose.
 */
export function maxPanelPx(windowPx: number, otherPanelPx: number): number {
  return Math.max(MIN_PANEL_PX, Math.round(windowPx - otherPanelPx - MIN_CONTENT_PX));
}

/**
 * Resolve a pointer's requested width against the bounds.
 *
 * Below the minimum is the collapse gesture, not a clamp — that is the whole
 * reason this returns a union rather than a number.
 */
export function resolveDrag(requestedPx: number, maxPx: number): DragOutcome {
  if (!Number.isFinite(requestedPx) || requestedPx < MIN_PANEL_PX) {
    return { kind: "collapse" };
  }
  return { kind: "width", px: Math.min(Math.round(requestedPx), maxPx) };
}

/**
 * Clamp an already-committed width into the bounds — used while dragging (so
 * the panel visibly stops instead of running off the window) and when the
 * window itself shrinks. Never collapses: a window resize is not a gesture the
 * user aimed at this panel.
 */
export function clampPanelPx(px: number, maxPx: number): number {
  return Math.min(Math.max(Math.round(px), MIN_PANEL_PX), maxPx);
}

/**
 * A stored width, or null when this device has none / the entry is junk.
 *
 * Null is a real third value, not "zero": it means "no answer yet", and the
 * panel then renders at the `--side-w` design token, which is a per-skin rem
 * measure this module cannot and should not compute.
 */
export function parseStoredWidth(raw: string | null): number | null {
  if (raw === null) {
    return null;
  }
  const px = Number(raw);
  if (!Number.isFinite(px) || px < MIN_PANEL_PX) {
    return null;
  }
  return Math.round(px);
}
