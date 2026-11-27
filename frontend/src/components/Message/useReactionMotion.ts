import { useCallback, useEffect, useRef, useState } from "react";

interface PillLike {
  emoji: string;
  count: number;
}

/**
 * Which reaction pills move (#2125): one that joins a row already showing pills pops in; one whose
 * count changed rolls its digit. Read during render, against the last committed pills, so the
 * first paint of a new pill already carries its class. Pills present when the row first painted
 * (a cached row, a remount while scrolling) never move; a row that paints its first pills late
 * fades as a whole instead (#2115).
 */
export function useReactionMotion(reactions: readonly PillLike[]): {
  pops: (emoji: string) => boolean;
  rolls: (emoji: string) => boolean;
} {
  const committed = useRef<Map<string, number> | null>(null);
  const popped = useRef(new Set<string>());
  const rolled = useRef(new Set<string>());
  const last = committed.current;
  if (last !== null && last.size > 0) {
    for (const { emoji, count } of reactions) {
      const before = last.get(emoji);
      if (before === undefined) {
        popped.current.add(emoji);
      } else if (before !== count) {
        rolled.current.add(emoji);
      }
    }
  }
  useEffect(() => {
    const now = new Map(reactions.map((r) => [r.emoji, r.count] as const));
    for (const emoji of [...popped.current]) {
      if (!now.has(emoji)) {
        popped.current.delete(emoji);
      }
    }
    for (const emoji of [...rolled.current]) {
      if (!now.has(emoji)) {
        rolled.current.delete(emoji);
      }
    }
    committed.current = now;
  }, [reactions]);
  return {
    pops: (emoji) => popped.current.has(emoji),
    rolls: (emoji) => rolled.current.has(emoji),
  };
}

/**
 * The tap bump, per pill. Two classes with the same keyframes under different names alternate,
 * so a second tap restarts the bump; it lives in the React className, so the pill's own class
 * change on the same tap (reacted or not) cannot wipe it.
 */
export function useReactionBump(): {
  bumpClass: (emoji: string) => string;
  bump: (emoji: string) => void;
} {
  const [taps, setTaps] = useState<ReadonlyMap<string, number>>(() => new Map());
  const bump = useCallback((emoji: string) => {
    setTaps((prev) => new Map(prev).set(emoji, (prev.get(emoji) ?? 0) + 1));
  }, []);
  const bumpClass = (emoji: string) => {
    const n = taps.get(emoji);
    if (n === undefined) {
      return "";
    }
    return n % 2 === 1 ? "motion-bump" : "motion-bump-again";
  };
  return { bumpClass, bump };
}
