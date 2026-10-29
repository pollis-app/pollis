import { useEffect } from "react";
import * as api from "../services/api";
import {
  ACTIVITY_REPORT_THROTTLE_MS,
  loadDeviceAutoLockMinutes,
} from "../utils/autoLock";

/** "A human is here" events, the set a screensaver watches; passive and captured. */
const ACTIVITY_EVENTS = [
  "pointerdown",
  "pointermove",
  "keydown",
  "wheel",
  "focus",
] as const;

/**
 * Arms the Rust-owned idle auto-lock (#851) while the unlocked shell is
 * mounted (see `utils/autoLock.ts`): pushes the window on mount, reports
 * activity at most every {@link ACTIVITY_REPORT_THROTTLE_MS}, disarms on
 * unmount.
 */
export function useAutoLock(userId: string | null | undefined): void {
  useEffect(() => {
    let disposed = false;

    api
      .setAutoLockTimeout(loadDeviceAutoLockMinutes(userId))
      .catch((err) => console.warn("[autolock] failed to arm:", err));

    // Leading-edge throttle with no trailing timer, so nothing dangles or runs
    // on a cadence. Starts at 0 so the first interaction reports.
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
      // Locked or logged out: don't leave a timer armed for a dead session.
      api
        .setAutoLockTimeout(null)
        .catch((err) => console.warn("[autolock] failed to disarm:", err));
    };
  }, [userId]);
}
