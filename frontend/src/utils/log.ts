/**
 * Swallow an expected, non-actionable failure while keeping the intent
 * explicit. Use for genuinely best-effort work — sound effects, presence
 * pings, badge/icon updates — where a failure should never surface and isn't
 * worth a warning. Real IPC calls should `.catch((e) => console.warn(...))`
 * instead so their failures stay visible.
 */
export function logIgnored(e: unknown): void {
  console.debug("ignored:", e);
}
