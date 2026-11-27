import { useSyncExternalStore } from "react";
import { prefersReducedMotion, subscribeReducedMotion } from "../utils/motion";

/** The OS reduced-motion setting, live (#2125). The CSS half is the rule in `components/ui/motion.css`. */
export function useReducedMotion(): boolean {
  return useSyncExternalStore(subscribeReducedMotion, prefersReducedMotion, () => false);
}
