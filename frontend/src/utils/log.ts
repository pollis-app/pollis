/**
 * Swallow an expected, non-actionable failure (SFX, presence pings, badges).
 * Real IPC calls should `console.warn` instead.
 */
export function logIgnored(e: unknown): void {
  console.debug("ignored:", e);
}
