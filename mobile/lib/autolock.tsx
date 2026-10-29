// Auto-lock after inactivity (#899). Desktop's deadline lives in Rust and
// arrives as a Tauri event, which mobile cannot receive, so mobile keeps its own
// deadline here against the plain `lock` arm; do not wire
// `set_auto_lock_timeout` / `report_user_activity` from mobile.
//
// Two clocks: a background timestamp compared on return (JS timers don't run
// reliably in the background), and one foreground timer aimed at a deadline
// that touch pushes forward. Locking also clears the query cache, which holds
// decrypted message bodies. The window is device-local and defaults to Off:
// silently locking someone out is worse than not locking.

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
import i18n from "../i18n";

/** Offered windows, mirroring desktop's `AUTO_LOCK_OPTIONS`. `null` = Off. */
export const AUTO_LOCK_OPTIONS_MINUTES: readonly (number | null)[] = [
  null,
  1,
  5,
  15,
  60,
];

export function autoLockLabel(minutes: number | null): string {
  if (minutes === null) {
    return i18n.t("settings:security.autoLockOff");
  }
  return i18n.t("settings:security.autoLockMinutes", { count: minutes });
}

const STORE_KEY = "pollis_auto_lock_minutes";

// Touch moves the deadline at most once per gap; 15s is far below the shortest
// window (60s), so a deadline can never expire inside one gap.
const ACTIVITY_THROTTLE_MS = 15_000;

let cachedMinutes: number | null = null;
let cacheLoaded = false;
const settingListeners = new Set<(m: number | null) => void>();

function coerceMinutes(raw: string | null): number | null {
  if (!raw) {
    return null;
  }
  const minutes = parseInt(raw, 10);
  // An unknown or corrupt value means "the user has not asked for this".
  if (!AUTO_LOCK_OPTIONS_MINUTES.some((o) => o === minutes)) {
    return null;
  }
  return minutes;
}

/** Cached after the first read. `null` = Off. */
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

/** The lock primitive; no-ops when signed out or already locked. */
export function useLockNow(): () => Promise<void> {
  const router = useRouter();
  const queryClient = useQueryClient();
  return useCallback(async () => {
    if (!appStore.currentUser || appStore.isLocked) {
      return;
    }
    appStore.setLocked(true);
    try {
      await invoke("lock");
    } catch (e) {
      // Still lock the UI: a failed bridge call must not leave the app
      // visibly unlocked when the user asked for a lock.
      console.warn("[autolock] lock command failed:", e);
    }
    queryClient.clear();
    router.replace("/(auth)/pin");
  }, [router, queryClient]);
}

/**
 * Mounted once around the root <Stack>. Captures touch starts (without
 * consuming them) to refresh the foreground deadline.
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

  const deadlineRef = useRef<number | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastActivityNoteRef = useRef(0);
  const backgroundedAtRef = useRef<number | null>(null);

  const clearTimer = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    deadlineRef.current = null;
  }, []);

  // When it fires early (activity moved the deadline), re-aim at the remainder.
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

  // Moves the deadline only; the timer re-aims itself.
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

  useEffect(() => {
    if (minutes === null || !userId || isLocked) {
      clearTimer();
      return;
    }
    deadlineRef.current = Date.now() + minutes * 60_000;
    armTimer();
    return clearTimer;
  }, [minutes, userId, isLocked, armTimer, clearTimer]);

  useEffect(() => {
    const sub = AppState.addEventListener("change", (next) => {
      if (next === "background" || next === "inactive") {
        if (backgroundedAtRef.current === null) {
          backgroundedAtRef.current = Date.now();
        }
        // The timestamp is the authority in the background; kill the timer so
        // it can't fire on a stale wake.
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
