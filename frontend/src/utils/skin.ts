/** The desktop skin: its names, default, and the value it is stored under. No imports, so node tests load it. */

/** UI skin. Modern is the base look; Terminal is the opt-in variant (`docs/design/`). Synced, unlike font size. */
export type Skin = "terminal" | "modern";

/** The skin a user has not chosen yet. */
export const DEFAULT_SKIN: Skin = "modern";

/**
 * Modern's stored value. It stays `"refined"` on the wire: shipped desktop builds map any other
 * value to terminal, which would flip a user's not-yet-updated devices.
 */
const MODERN_WIRE = "refined";

/** Coerce a stored value to a Skin: `refined` (the wire name) and `modern` read as Modern; absent → the default. */
export function normalizeSkin(v: unknown): Skin {
  if (v === "terminal") {
    return "terminal";
  }
  if (v === MODERN_WIRE || v === "modern") {
    return "modern";
  }
  return DEFAULT_SKIN;
}

/** The value a save writes for `skin`. */
export function skinToWire(skin: Skin): string {
  return skin === "modern" ? MODERN_WIRE : "terminal";
}

/** The `data-skin` attribute on <html>; `colorUtils.applySkin` also repaints the colours. */
export function setSkinAttribute(skin: Skin): void {
  document.documentElement.dataset.skin = skin;
}
