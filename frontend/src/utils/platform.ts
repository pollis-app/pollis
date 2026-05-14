export const isMac =
  typeof navigator !== "undefined" &&
  navigator.platform.toUpperCase().indexOf("MAC") >= 0;

export const isWindows =
  typeof navigator !== "undefined" &&
  navigator.userAgent.toLowerCase().includes("windows");

export function shortcutLabel(key: string): string {
  return isMac ? `⌘${key}` : `Ctrl+${key}`;
}
