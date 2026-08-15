import { useEffect } from "react";
import * as api from "../services/api";
import {
  ACTIVITY_REPORT_THROTTLE_MS,
  loadDeviceAutoLockMinutes,
} from "../utils/autoLock";

/**
 * Events that count as "a human is at this machine". Deliberately the same set
 * an OS screensaver watches: pointer movement and clicks, keys, scrolling, and
 * the app regaining focus. All passive and captured, so nothing here can
 * interfere with a component's own handling of the event.
 */
const ACTIVITY_EVENTS = [
  "pointerdown",
  "pointermove",
  "keydown",
  "wheel",
  "focus",
] as const;

/**
 * Arms the backend idle auto-lock (#851) for as long as the unlocked shell is
 * mounted, and feeds it throttled activity reports.
 *
 * The deadline itself is owned by Rust — see `utils/autoLock.ts` for why a
 * renderer-side timer would be unreliable in precisely the cases that matter.
 * All this hook does is:
 *
 * 1. push the device-local window on mount (covers cold start and every
 *    unlock, since the shell remounts after the PIN gate),
 * 2. report activity at most once every {@link ACTIVITY_REPORT_THROTTLE_MS} so
 *    typing doesn't become one IPC call per keystroke, and
 * 3. disarm on unmount, so the timer is live exactly while there is decrypted
 *    content to protect.
 *
 * Changing the setting re-pushes immediately from the Security page rather than
 * waiting for a remount.
 */
export function useAutoLock(userId: string | null | undefined): void {
  useEffect(() => {
    let disposed = false;

    api
      .setAutoLockTimeout(loadDeviceAutoLockMinutes(userId))
      .catch((err) => console.warn("[autolock] failed to arm:", err));

    // Throttle on the leading edge: the first event after a quiet gap reports
    // straight away, and everything inside the gap is dropped. No trailing
    // timer, so there is no `setTimeout` left dangling on unmount and nothing
    // runs on a cadence.
    // Starts at 0, not `Date.now()`: the very first interaction after the
    // shell mounts should report, rather than being swallowed by a throttle
    // window the user never saw.
    let lastReport = 0;
    const report = () => {
      const now = Date.now();
      if (now - lastReport < ACTIVITY_REPORT_THROTTLE_MS) {
        return;
      }
      lastReport = now;
      if (disposed) {
        return;
      }
      api
        .reportUserActivity()
        .catch((err) => console.warn("[autolock] activity report failed:", err));
    };

    for (const name of ACTIVITY_EVENTS) {
      window.addEventListener(name, report, { passive: true, capture: true });
    }

    return () => {
      disposed = true;
      for (const name of ACTIVITY_EVENTS) {
        window.removeEventListener(name, report, { capture: true });
      }
      // The shell is going away — either the app locked (the timer has already
      // fired and stopped) or the user logged out. Either way, leaving a timer
      // armed against a session that no longer exists is a state worth not
      // having.
      api
        .setAutoLockTimeout(null)
        .catch((err) => console.warn("[autolock] failed to disarm:", err));
    };
  }, [userId]);
}
