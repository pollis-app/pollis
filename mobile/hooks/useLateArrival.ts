import { useRef } from "react";

/**
 * True while `present` holds and this component once rendered without it (#2125): an unread badge
 * that appears while you watch pops in; one already there on the first paint does not. `armed`:
 * an absence only counts once it means something. A badge row drawn before the first unread
 * counts landed was not "without a badge", it just did not know yet, so pass
 * `appStore.unreadHydrated` for unread badges.
 */
export function useLateArrival(present: boolean, armed = true): boolean {
  const seenWithout = useRef(false);
  if (!present && armed) {
    seenWithout.current = true;
  }
  return present && seenWithout.current;
}
