// Motion tokens and the reduced-motion source for the renderer (#2125). The numbers live in
// design/theme.ts, shared with mobile; this paints them as CSS variables and answers "should
// this move?" for code that animates outside CSS (smooth scrolls).

import { MOTION, cssCurve } from "../../../design/theme.ts";

const REDUCE_QUERY = "(prefers-reduced-motion: reduce)";

/** The `--motion-*` variables `components/ui/motion.css` reads. */
export function motionCssVars(): Record<string, string> {
  return {
    "--motion-fast": `${MOTION.fastMs}ms`,
    "--motion-base": `${MOTION.baseMs}ms`,
    "--motion-slow": `${MOTION.slowMs}ms`,
    "--motion-ease-out": cssCurve(MOTION.easeOut),
    "--motion-message-rise": `${MOTION.messageRisePx}px`,
    "--motion-popover-rise": `${MOTION.popoverRisePx}px`,
    "--motion-pop-from": `${MOTION.popFromScale}`,
    "--motion-pop-peak": `${MOTION.popPeakScale}`,
    "--motion-bump": `${MOTION.bumpScale}`,
    "--motion-hover-scale": `${MOTION.hoverScale}`,
  };
}

/** Paints the variables on `<html>`, before anything renders (main.tsx). */
export function initMotion(root: HTMLElement = document.documentElement): void {
  for (const [name, value] of Object.entries(motionCssVars())) {
    root.style.setProperty(name, value);
  }
}

function reduceQuery(): MediaQueryList | null {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return null;
  }
  return window.matchMedia(REDUCE_QUERY);
}

/** The OS reduced-motion setting, read now. */
export function prefersReducedMotion(): boolean {
  return reduceQuery()?.matches ?? false;
}

/** Calls `onChange` whenever the setting flips; returns the unsubscribe. */
export function subscribeReducedMotion(onChange: () => void): () => void {
  const query = reduceQuery();
  if (!query) {
    return () => {};
  }
  query.addEventListener("change", onChange);
  return () => query.removeEventListener("change", onChange);
}

/** `behavior` for a programmatic scroll: smooth, or a jump under reduced motion. */
export function scrollBehavior(): ScrollBehavior {
  return prefersReducedMotion() ? "auto" : "smooth";
}
