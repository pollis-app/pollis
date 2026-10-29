import { useCallback, useRef, useState } from "react";
import { useFocusEffect } from "expo-router";

/** Single-flight guard for a slow submit: `begin()` claims synchronously; a success that navigates keeps it held. */
export function useSubmitLock() {
  const held = useRef(false);
  const left = useRef(false);
  const [busy, setBusy] = useState(false);

  useFocusEffect(
    useCallback(() => {
      // Only a RETURN releases it: the first focus can land after an on-mount submit began.
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
