import { check as checkForUpdate } from "../bridge";
import { appStore } from "../stores/appStore";

// Update checks run outside React, on window focus and rate-limited; never on an interval (#874).

/// Minimum gap between checks.
const MIN_CHECK_GAP_MS = 15 * 60 * 1000;

// One-shot delay after sign-in; App.tsx's startup gate just checked.
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
    // Keep a known version across transient failures.
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
