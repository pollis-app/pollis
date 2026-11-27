import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { MOTION } from "../../../../design/theme";
import { useReducedMotion } from "../../hooks/useReducedMotion";

interface FoldProps {
  open: boolean;
  children: React.ReactNode;
  testId?: string;
}

// "from": mounted with the track shut, one layout away from opening.
type Phase = "closed" | "from" | "opening" | "open" | "closing";

/**
 * A list that folds open and shut (#2125): the sidebar's channels under a group. Closed means
 * unmounted, as before, so nothing hidden stays in the DOM or the tab order. While it folds shut
 * the rows stay, inert, for the length of the fold; every phase ends on a timer, never on a
 * `transitionend` that may not come. The first paint never moves, and under reduced motion
 * nothing does.
 */
export const Fold: React.FC<FoldProps> = ({ open, children, testId }) => {
  const reduceMotion = useReducedMotion();
  const [phase, setPhase] = useState<Phase>(open ? "open" : "closed");
  const ref = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    setPhase((now) => {
      if (open) {
        if (now === "open" || now === "opening" || now === "from") {
          return now;
        }
        return reduceMotion ? "open" : "from";
      }
      if (now === "closed" || now === "closing") {
        return now;
      }
      return reduceMotion ? "closed" : "closing";
    });
  }, [open, reduceMotion]);

  useLayoutEffect(() => {
    if (phase !== "from") {
      return;
    }
    // Lay out the shut track first, or the transition has nothing to run from.
    void ref.current?.offsetHeight;
    setPhase("opening");
  }, [phase]);

  useEffect(() => {
    if (phase !== "opening" && phase !== "closing") {
      return;
    }
    const timer = window.setTimeout(
      () => setPhase(phase === "opening" ? "open" : "closed"),
      MOTION.baseMs,
    );
    return () => window.clearTimeout(timer);
  }, [phase]);

  if (phase === "closed") {
    return null;
  }
  return (
    <div
      ref={ref}
      data-testid={testId}
      data-fold={phase}
      data-open={phase === "opening" || phase === "open" ? "true" : "false"}
      data-moving={phase === "open" ? undefined : "true"}
      className="motion-fold"
      inert={!open}
    >
      <div>{children}</div>
    </div>
  );
};
