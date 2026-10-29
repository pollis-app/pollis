/** Auto-lock (#851), renderer half: Rust owns the deadline (hidden WebViews throttle timers); the choice is device-local. */

/** Must match `AUTO_LOCK_EVENT` in `pollis-core/src/commands/autolock.rs`. */
export const AUTO_LOCK_EVENT = "auto-lock";

export interface AutoLockOption {
  /** Minutes of inactivity, or `null` for "never lock". */
  minutes: number | null;
}

/** Offered windows; Rust validates the same list. Labels come from `autoLockLabel` (#855). */
export const AUTO_LOCK_OPTIONS: readonly AutoLockOption[] = [
  { minutes: null },
  { minutes: 1 },
  { minutes: 5 },
  { minutes: 15 },
  { minutes: 60 },
] as const;

/** Activity report throttle; far below the shortest (60s) window. */
export const ACTIVITY_REPORT_THROTTLE_MS = 15_000;

const AUTO_LOCK_KEY_PREFIX = "pollis-auto-lock:";

function autoLockKey(userId: string | null | undefined): string {
  return `${AUTO_LOCK_KEY_PREFIX}${userId ?? "anon"}`;
}

/** This user's window in minutes, or `null` (Off, also the default for unset or corrupt). */
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
  }
}
