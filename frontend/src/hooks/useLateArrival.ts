import { useEffect, useRef } from "react";

/**
 * True while `present` holds after this component already painted without it (#2125): content
 * that replaced a loading state, or an unread badge that turned up. Read during render against
 * the last commit, so the arriving element's first paint knows. `armed`: an absence only counts
 * once it means something (a badge row painted before the first unread counts landed was not
 * "without a badge", it just did not know yet).
 */
export function useLateArrival(present: boolean, armed = true): boolean {
  const committed = useRef<{ present: boolean; armed: boolean } | null>(null);
  const late = useRef(false);
  const last = committed.current;
  if (present && last !== null && !last.present && last.armed) {
    late.current = true;
  }
  useEffect(() => {
    committed.current = { present, armed };
    if (!present) {
      late.current = false;
    }
  }, [present, armed]);
  return present && late.current;
}
