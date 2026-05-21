export const isMac =
  typeof navigator !== "undefined" &&
  navigator.platform.toUpperCase().indexOf("MAC") >= 0;

export const isWindows =
  typeof navigator !== "undefined" &&
  navigator.userAgent.toLowerCase().includes("windows");

// macOS condenses ⌘ tight against the next glyph; a thin space (U+2009)
// gives the kbd badge a little air without affecting Ctrl+ layout. Matches
// the convention used by formatCombo() in keyboard/keyCombo.ts.
export function shortcutLabel(key: string): string {
  return isMac ? `⌘ ${key}` : `Ctrl+${key}`;
}
