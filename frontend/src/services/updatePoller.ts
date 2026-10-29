import { check as checkForUpdate } from "../bridge";
import { appStore } from "../stores/appStore";

// Update checks live outside React so remounts can't duplicate or drop them.
// Event-driven (#874): triggered by the user returning to the window,
// rate-limited, never on a timer.

/// Don't re-check more often than this, however many focus events arrive.
const MIN_CHECK_GAP_MS = 15 * 60 * 1000;

// A one-shot delay after sign-in, so we don't pile onto App.tsx's startup check.
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
    // Expected on blips; keep an already-known available version.
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
