// Auto-lock after inactivity (#899) — the RN layer's own implementation.
//
// Desktop's idle deadline lives in Rust (`pollis-core/src/commands/autolock.rs`)
// because a hidden WebView throttles its timers; the expiry is pushed to the
// renderer as a Tauri event. Mobile cannot receive Tauri events, so the
// deadline lives HERE, against the existing `lock` bridge arm only — do not
// wire `set_auto_lock_timeout` / `report_user_activity` from mobile.
//
// Two clocks cover the two ways a phone goes idle:
//   1. Backgrounded — an AppState listener records when the app left the
//      foreground; on return it locks if the elapsed time ≥ the window.
//      (JS timers don't run reliably in the background, so this is a
//      timestamp comparison, not a timer.)
//   2. Foregrounded-but-untouched — a single timer aimed at a deadline that
//      touch activity pushes forward. Activity only moves the deadline
//      variable (throttled); the timer re-aims itself when it fires early
//      instead of being torn down per event.
//
// Locking = `invoke("lock")` (drops key material + closes the local DB) +
// `queryClient.clear()` (decrypted message bodies survive in the query cache
// otherwise — desktop learned this) + appStore lock state + route to the PIN
// screen. Unlock flows back through `/(auth)/pin` → `get_unlock_state` →
// `unlock` exactly like a cold start.
//
// The window choice is deliberately device-local (expo-secure-store), same
// windows as desktop's `AUTO_LOCK_OPTIONS`: Off / 1 / 5 / 15 / 60 minutes,
// default Off — silently locking someone out of their own app is worse than
// not locking.

import React, {
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { AppState, View } from "react-native";
import * as SecureStore from "expo-secure-store";
import { useRouter } from "expo-router";
import { useQueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "./native";
import { appStore } from "../stores/appStore";

/** Offered windows, mirroring desktop's `AUTO_LOCK_OPTIONS`. `null` = Off. */
export const AUTO_LOCK_OPTIONS_MINUTES: readonly (number | null)[] = [
  null,
  1,
  5,
  15,
  60,
];

/** Human label for a window value (Off / 1 min / …). */
export function autoLockLabel(minutes: number | null): string {
  if (minutes === null) {
    return "Off";
  }
  return `${minutes} min`;
}

const STORE_KEY = "pollis_auto_lock_minutes";

// Same rationale as desktop's ACTIVITY_REPORT_THROTTLE_MS: touch moves the
// deadline at most once per gap. 15s is far below the shortest window (60s),
// so a deadline can never expire inside one throttle gap.
const ACTIVITY_THROTTLE_MS = 15_000;

// ── Device-local setting ─────────────────────────────────────────────────

let cachedMinutes: number | null = null;
let cacheLoaded = false;
const settingListeners = new Set<(m: number | null) => void>();

function coerceMinutes(raw: string | null): number | null {
  if (!raw) {
    return null;
  }
  const minutes = parseInt(raw, 10);
  // An unknown / corrupt value means "the user has not asked for this".
  if (!AUTO_LOCK_OPTIONS_MINUTES.some((o) => o === minutes)) {
    return null;
  }
  return minutes;
}

/** Read the stored window (cached after first read). `null` = Off. */
export async function loadAutoLockMinutes(): Promise<number | null> {
  if (cacheLoaded) {
    return cachedMinutes;
  }
  try {
    const raw = await SecureStore.getItemAsync(STORE_KEY);
    cachedMinutes = coerceMinutes(raw);
  } catch {
    cachedMinutes = null;
  }
  cacheLoaded = true;
  return cachedMinutes;
}

/** Persist the window and notify subscribers. `null` clears it (Off). */
export async function persistAutoLockMinutes(
  minutes: number | null,
): Promise<void> {
  cachedMinutes = minutes;
  cacheLoaded = true;
  for (const l of settingListeners) {
    l(minutes);
  }
  try {
    if (minutes === null) {
      await SecureStore.deleteItemAsync(STORE_KEY);
    } else {
      await SecureStore.setItemAsync(STORE_KEY, String(minutes));
    }
  } catch (e) {
    console.warn("[autolock] persisting setting failed:", e);
  }
}

/** Setting hook for the Security screen — reads + live-updates the window. */
export function useAutoLockMinutes(): {
  minutes: number | null;
  setMinutes: (m: number | null) => void;
} {
  const [minutes, setState] = useState<number | null>(cachedMinutes);
  useEffect(() => {
    let mounted = true;
    void loadAutoLockMinutes().then((m) => {
      if (mounted) {
        setState(m);
      }
    });
    const listener = (m: number | null) => setState(m);
    settingListeners.add(listener);
    return () => {
      mounted = false;
      settingListeners.delete(listener);
    };
  }, []);
  const setMinutes = useCallback((m: number | null) => {
    void persistAutoLockMinutes(m);
  }, []);
  return { minutes, setMinutes };
}

// ── Locking ──────────────────────────────────────────────────────────────

/**
 * The manual/automatic lock primitive. Safe to call redundantly — it no-ops
 * when signed out or already locked.
 */
export function useLockNow(): () => Promise<void> {
  const router = useRouter();
  const queryClient = useQueryClient();
  return useCallback(async () => {
    if (!appStore.currentUser || appStore.isLocked) {
      return;
    }
    appStore.setLocked(true);
    try {
      // Drops in-memory key material and closes the local DB.
      await invoke("lock");
    } catch (e) {
      // Still proceed to the locked UI — a failed bridge call must not leave
      // the app visibly unlocked when the user asked for a lock.
      console.warn("[autolock] lock command failed:", e);
    }
    // Decrypted message bodies live in the query cache; drop them.
    queryClient.clear();
    router.replace("/(auth)/pin");
  }, [router, queryClient]);
}

// ── Engine + touch capture ───────────────────────────────────────────────

/**
 * Mounted once around the root <Stack> (see app/_layout.tsx). Captures touch
 * starts (without consuming them) to refresh the foreground deadline, and
 * owns the AppState background/foreground bookkeeping.
 */
export function AutoLockProvider({ children }: { children: React.ReactNode }) {
  const { minutes } = useAutoLockMinutes();
  const lockNow = useLockNow();
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);

  const minutesRef = useRef(minutes);
  minutesRef.current = minutes;
  const lockNowRef = useRef(lockNow);
  lockNowRef.current = lockNow;

  // Foreground idle deadline (ms epoch), moved forward by activity.
  const deadlineRef = useRef<number | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastActivityNoteRef = useRef(0);
  // When the app left the foreground; null while foregrounded.
  const backgroundedAtRef = useRef<number | null>(null);

  const clearTimer = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    deadlineRef.current = null;
  }, []);

  // Aim the single timer at the current deadline. When it fires early
  // (activity moved the deadline), it re-aims at the remainder instead of
  // locking.
  const armTimer = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    const deadline = deadlineRef.current;
    if (deadline === null) {
      return;
    }
    const fireIn = Math.max(0, deadline - Date.now());
    timerRef.current = setTimeout(() => {
      timerRef.current = null;
      const current = deadlineRef.current;
      if (current === null) {
        return;
      }
      if (Date.now() >= current) {
        clearTimer();
        void lockNowRef.current();
      } else {
        armTimer();
      }
    }, fireIn);
  }, [clearTimer]);

  // Touch activity: cheap throttle, moves the deadline without touching the
  // timer.
  const noteActivity = useCallback(() => {
    const m = minutesRef.current;
    if (m === null || deadlineRef.current === null) {
      return;
    }
    const now = Date.now();
    if (now - lastActivityNoteRef.current < ACTIVITY_THROTTLE_MS) {
      return;
    }
    lastActivityNoteRef.current = now;
    deadlineRef.current = now + m * 60_000;
  }, []);

  // (Re)start the whole engine whenever the window, the user, or the lock
  // state changes.
  useEffect(() => {
    if (minutes === null || !userId || isLocked) {
      clearTimer();
      return;
    }
    deadlineRef.current = Date.now() + minutes * 60_000;
    armTimer();
    return clearTimer;
  }, [minutes, userId, isLocked, armTimer, clearTimer]);

  // Background/foreground transitions.
  useEffect(() => {
    const sub = AppState.addEventListener("change", (next) => {
      if (next === "background" || next === "inactive") {
        if (backgroundedAtRef.current === null) {
          backgroundedAtRef.current = Date.now();
        }
        // JS timers are unreliable in the background — the timestamp above is
        // the authority; kill the timer so it can't fire on a stale wake.
        clearTimer();
        return;
      }
      if (next !== "active") {
        return;
      }
      const backgroundedAt = backgroundedAtRef.current;
      backgroundedAtRef.current = null;
      const m = minutesRef.current;
      if (m === null || !appStore.currentUser || appStore.isLocked) {
        return;
      }
      if (backgroundedAt !== null && Date.now() - backgroundedAt >= m * 60_000) {
        void lockNowRef.current();
        return;
      }
      // Fresh foreground session — restart the idle deadline.
      deadlineRef.current = Date.now() + m * 60_000;
      armTimer();
    });
    return () => sub.remove();
  }, [armTimer, clearTimer]);

  return (
    <View
      style={{ flex: 1 }}
      onStartShouldSetResponderCapture={() => {
        noteActivity();
        return false;
      }}
    >
      {children}
    </View>
  );
}
