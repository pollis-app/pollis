import { useCallback, useRef, useState } from "react";
import { useFocusEffect } from "expo-router";

/**
 * Single-flight guard for a slow submit (the sign-in steps, the PIN pad).
 *
 * `begin()` claims the action synchronously and turns `busy` on, so the
 * spinner shows from the next frame and a second tap that lands before React
 * re-renders is refused (`begin()` returns false). `end()` releases it — call
 * it when the call fails, or succeeds without leaving the screen. A success
 * that navigates away keeps the lock held, so the screen stays busy until it
 * is gone; coming back to it (the Android back button) releases it.
 */
export function useSubmitLock() {
  const held = useRef(false);
  const left = useRef(false);
  const [busy, setBusy] = useState(false);

  useFocusEffect(
    useCallback(() => {
      // Only a RETURN to the screen releases the lock: the first focus can
      // land after an on-mount submit has already begun.
      if (left.current) {
        left.current = false;
        held.current = false;
        setBusy(false);
      }
      return () => {
        left.current = true;
      };
    }, []),
  );

  const begin = useCallback(() => {
    if (held.current) {
      return false;
    }
    held.current = true;
    setBusy(true);
    return true;
  }, []);

  const end = useCallback(() => {
    held.current = false;
    setBusy(false);
  }, []);

  return { busy, begin, end };
}
