/**
 * Idle auto-lock (#851), the renderer's half: the deadline lives in Rust
 * (`commands/autolock.rs`) because a WebView throttles timers when hidden. This
 * holds the device-local choice (never synced) and the activity throttle.
 */

/** Must match `AUTO_LOCK_EVENT` in `pollis-core/src/commands/autolock.rs`. */
export const AUTO_LOCK_EVENT = "auto-lock";

export interface AutoLockOption {
  minutes: number | null;
}

/**
 * The offered windows; Rust validates against `AUTO_LOCK_OPTIONS_MINUTES`.
 * Labels come from `autoLockLabel` in `pages/SecurityPage.tsx` (#855).
 */
export const AUTO_LOCK_OPTIONS: readonly AutoLockOption[] = [
  { minutes: null },
  { minutes: 1 },
  { minutes: 5 },
  { minutes: 15 },
  { minutes: 60 },
] as const;

/** Min gap between activity reports; far below the shortest window (60s). */
export const ACTIVITY_REPORT_THROTTLE_MS = 15_000;

const AUTO_LOCK_KEY_PREFIX = "pollis-auto-lock:";

function autoLockKey(userId: string | null | undefined): string {
  return `${AUTO_LOCK_KEY_PREFIX}${userId ?? "anon"}`;
}

/**
 * This user's auto-lock minutes, or `null` (Off). Unset or corrupt means Off:
 * silently locking someone out is worse than not locking.
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
    // localStorage unavailable / quota exceeded: ignore
  }
}
