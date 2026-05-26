import { check as checkForUpdate } from "../bridge";
import { useAppStore } from "../stores/appStore";

// The update poller lives outside React on purpose: a singleton timer is
// resilient to StrictMode double-mounts, route changes, and component
// unmount/remount churn that would otherwise duplicate or drop polls. The
// only thing React touches is the resulting Zustand field.

const POLL_INTERVAL_MS = 15 * 60 * 1000;
// Wait briefly after sign-in before the first check so we don't pile on top
// of the startup update gate in App.tsx (which already ran a check). If
// THAT check found something the app is already on the UpdateScreen; if it
// didn't, give the network a moment before we re-ask.
const FIRST_CHECK_DELAY_MS = 30 * 1000;

let intervalId: number | null = null;
let firstCheckTimeoutId: number | null = null;
let started = false;

async function runCheck(): Promise<void> {
  try {
    const update = await checkForUpdate();
    const setAvailable = useAppStore.getState().setAvailableUpdateVersion;
    setAvailable(update ? update.version : null);
  } catch (err) {
    // Network blips are expected; log once and move on. Don't clear an
    // already-known available version on a transient failure.
    console.warn("[updatePoller] check failed:", err);
  }
}

export function startUpdatePolling(): void {
  if (started) {
    return;
  }
  started = true;

  firstCheckTimeoutId = window.setTimeout(() => {
    firstCheckTimeoutId = null;
    runCheck();
  }, FIRST_CHECK_DELAY_MS);

  intervalId = window.setInterval(runCheck, POLL_INTERVAL_MS);
}

export function stopUpdatePolling(): void {
  if (firstCheckTimeoutId !== null) {
    window.clearTimeout(firstCheckTimeoutId);
    firstCheckTimeoutId = null;
  }
  if (intervalId !== null) {
    window.clearInterval(intervalId);
    intervalId = null;
  }
  started = false;
}
