import { useCallback, useRef, useState } from "react";
import { useFocusEffect } from "expo-router";

/**
 * Single-flight guard for a slow submit. `begin()` claims synchronously, so a
 * second tap before React re-renders is refused. Call `end()` on failure or a
 * success that stays on screen; a success that navigates away keeps the lock
 * until the screen is returned to.
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
