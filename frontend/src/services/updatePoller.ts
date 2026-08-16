import { check as checkForUpdate } from "../bridge";
import { appStore } from "../stores/appStore";

// Update checks live outside React on purpose: a singleton is resilient to
// StrictMode double-mounts, route changes, and component unmount/remount churn
// that would otherwise duplicate or drop checks. The only thing React touches
// is the resulting store field.
//
// EVENT-DRIVEN, not periodic (#874). This used to be a 15-minute
// `setInterval`, which CLAUDE.md bans outright — and a timer is the wrong
// shape for the question anyway: nobody is helped by an update discovered
// while the window is hidden, and a machine left running for a week fired ~670
// checks nobody read. Now the trigger is the user coming back to the window,
// rate-limited so tabbing in and out doesn't turn into a check per switch.

/// Don't re-check more often than this, however many focus events arrive.
/// Same budget the old interval spent, just spent on demand.
const MIN_CHECK_GAP_MS = 15 * 60 * 1000;

// Wait briefly after sign-in before the first check so we don't pile on top of
// the startup update gate in App.tsx (which already ran a check). If THAT check
// found something the app is already on the UpdateScreen; if it didn't, give
// the network a moment before we re-ask. A one-shot delay, not a poll.
const FIRST_CHECK_DELAY_MS = 30 * 1000;

let firstCheckTimeoutId: number | null = null;
let lastCheckAt = 0;
let started = false;

async function runCheck(): Promise<void> {
  lastCheckAt = Date.now();
  try {
    const update = await checkForUpdate();
    const setAvailable = appStore.setAvailableUpdateVersion;
    setAvailable(update ? update.version : null);
  } catch (err) {
    // Network blips are expected; log once and move on. Don't clear an
    // already-known available version on a transient failure.
    console.warn("[updatePoller] check failed:", err);
  }
}

function onWindowActive(): void {
  if (document.visibilityState === "hidden") {
    return;
  }
  if (Date.now() - lastCheckAt < MIN_CHECK_GAP_MS) {
    return;
  }
  void runCheck();
}

export function startUpdatePolling(): void {
  if (started) {
    return;
  }
  started = true;

  firstCheckTimeoutId = window.setTimeout(() => {
    firstCheckTimeoutId = null;
    void runCheck();
  }, FIRST_CHECK_DELAY_MS);

  window.addEventListener("focus", onWindowActive);
  document.addEventListener("visibilitychange", onWindowActive);
}

export function stopUpdatePolling(): void {
  if (firstCheckTimeoutId !== null) {
    window.clearTimeout(firstCheckTimeoutId);
    firstCheckTimeoutId = null;
  }
  window.removeEventListener("focus", onWindowActive);
  document.removeEventListener("visibilitychange", onWindowActive);
  lastCheckAt = 0;
  started = false;
}
