/**
 * Idle auto-lock (#851) — the renderer's half.
 *
 * The deadline itself lives in Rust (`pollis-core/src/commands/autolock.rs`)
 * because a WebView throttles its own timers once the window is hidden or
 * minimized, which is exactly when an auto-lock most needs to fire. This module
 * owns only the *choice*: which window the user picked on this device, and the
 * throttle that keeps activity reporting off the IPC hot path.
 *
 * The choice is deliberately **device-local**, like font size and the call
 * ringtone — auto-lock describes where a machine physically sits (a laptop in a
 * cafe wants 1 minute; a desktop in a locked room wants Off), so syncing it
 * would force one answer onto every device.
 */

/** The global event Rust emits when the idle deadline expires. Must match
 *  `AUTO_LOCK_EVENT` in `pollis-core/src/commands/autolock.rs`. */
export const AUTO_LOCK_EVENT = "auto-lock";

export interface AutoLockOption {
  /** Minutes of inactivity, or `null` for "never lock". */
  minutes: number | null;
}

/**
 * The offered windows. Rust validates against the same list
 * (`AUTO_LOCK_OPTIONS_MINUTES`), so a value that isn't here can't be installed
 * even by a caller that bypasses this UI.
 */
// `minutes` only — the human label is derived from it by `autoLockLabel` in
// `pages/SecurityPage.tsx`, so the copy lives in the translation catalogue
// rather than in a util (#855).
export const AUTO_LOCK_OPTIONS: readonly AutoLockOption[] = [
  { minutes: null },
  { minutes: 1 },
  { minutes: 5 },
  { minutes: 15 },
  { minutes: 60 },
] as const;

/**
 * Smallest gap between two activity reports. Typing must not turn into one IPC
 * call per keystroke; 15s is far below every offered window (the shortest is
 * 60s), so the deadline can never expire inside one throttle gap.
 */
export const ACTIVITY_REPORT_THROTTLE_MS = 15_000;

const AUTO_LOCK_KEY_PREFIX = "pollis-auto-lock:";

function autoLockKey(userId: string | null | undefined): string {
  return `${AUTO_LOCK_KEY_PREFIX}${userId ?? "anon"}`;
}

/**
 * The device-local auto-lock window for this user in minutes, or `null` for
 * Off. Off is also the default: an unset, unreadable, or corrupt entry all mean
 * "the user has not asked for this", and silently locking someone out of their
 * own app is worse than not locking.
 */
export function loadDeviceAutoLockMinutes(
  userId: string | null | undefined,
): number | null {
  try {
    const raw = localStorage.getItem(autoLockKey(userId));
    if (!raw) {
      return null;
    }
    const minutes = parseInt(raw, 10);
    if (!AUTO_LOCK_OPTIONS.some((o) => o.minutes === minutes)) {
      return null;
    }
    return minutes;
  } catch {
    return null;
  }
}

/** Persist the device-local auto-lock window. `null` clears it (Off). */
export function saveDeviceAutoLockMinutes(
  userId: string | null | undefined,
  minutes: number | null,
): void {
  try {
    if (minutes === null) {
      localStorage.removeItem(autoLockKey(userId));
      return;
    }
    localStorage.setItem(autoLockKey(userId), String(minutes));
  } catch {
    // localStorage unavailable / quota exceeded — fall through silently
  }
}
