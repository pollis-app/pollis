// Auto-lock after inactivity (#899), in the RN layer: mobile cannot receive core's Tauri event. Device-local window, default Off.

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

// Touch moves the deadline at most once per gap; far below the shortest window.
const ACTIVITY_THROTTLE_MS = 15_000;

let cachedMinutes: number | null = null;
let cacheLoaded = false;
const settingListeners = new Set<(m: number | null) => void>();

function coerceMinutes(raw: string | null): number | null {
  if (!raw) {
    return null;
  }
  const minutes = parseInt(raw, 10);
  if (!AUTO_LOCK_OPTIONS_MINUTES.some((o) => o === minutes)) {
    return null;
  }
  return minutes;
}

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

/** Lock now. No-ops when signed out or already locked. */
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
      // Still show the locked UI: a failed call must not leave the app visibly unlocked.
      console.warn("[autolock] lock command failed:", e);
    }
    queryClient.clear();
    router.replace("/(auth)/pin");
  }, [router, queryClient]);
}

/** Mounted once around the root Stack; captures touch starts without consuming them. */
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

  // A timer that fires early re-aims at the remainder instead of locking.
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
        // JS timers are unreliable in the background: the timestamp is the authority.
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
