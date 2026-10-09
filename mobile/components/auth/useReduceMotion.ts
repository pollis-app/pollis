import { useEffect, useState } from "react";
import { AccessibilityInfo } from "react-native";

/**
 * Whether the OS "Reduce Motion" setting is on, tracking changes while the
 * screen is up. Starts false until the first answer arrives.
 */
export function useReduceMotion(): boolean {
  const [reduce, setReduce] = useState(false);
  useEffect(() => {
    let alive = true;
    AccessibilityInfo.isReduceMotionEnabled()
      .then((v) => {
        if (alive) {
          setReduce(v);
        }
      })
      .catch(() => {});
    const sub = AccessibilityInfo.addEventListener("reduceMotionChanged", setReduce);
    return () => {
      alive = false;
      sub.remove();
    };
  }, []);
  return reduce;
}
