// Which reaction pills move (#2125). Pure rules, pinned by tests/reaction-motion.test.ts.

/** A reaction this device just added is "live" for this long: its pill pops even on a row that had none. */
export const LOCAL_ADD_WINDOW_MS = 2000;

export interface PillMotion {
  /** New pills: scale in. */
  pop: Set<string>;
  /** Pills whose count changed: the digit rolls up. */
  roll: Set<string>;
}

/**
 * `prev`: the counts this row last showed, or null on its first render (a fresh paint, a cached
 * row scrolled back in). A first render moves nothing, except a pill the reader just added here;
 * after that, a pill that was not there pops and a count that changed rolls.
 */
export function pillMotion(
  prev: ReadonlyMap<string, number> | null,
  next: ReadonlyArray<{ emoji: string; count: number }>,
  justAdded: (emoji: string) => boolean,
): PillMotion {
  const pop = new Set<string>();
  const roll = new Set<string>();
  for (const { emoji, count } of next) {
    if (prev === null) {
      if (justAdded(emoji)) {
        pop.add(emoji);
      }
      continue;
    }
    const before = prev.get(emoji);
    if (before === undefined) {
      pop.add(emoji);
    } else if (before !== count) {
      roll.add(emoji);
    }
  }
  return { pop, roll };
}

// Reactions this device added, by message, with when.
const localAdds = new Map<string, number>();
const keyOf = (messageId: string, emoji: string) => `${messageId}\u0000${emoji}`;

/** Called when the reader adds a reaction, from wherever (a pill, the sheet, the picker). */
export function noteLocalReaction(messageId: string, emoji: string, now = Date.now()): void {
  localAdds.set(keyOf(messageId, emoji), now);
  // Never grows past the recent few.
  for (const [key, at] of localAdds) {
    if (now - at > LOCAL_ADD_WINDOW_MS) {
      localAdds.delete(key);
    }
  }
}

export function wasJustAdded(messageId: string, emoji: string, now = Date.now()): boolean {
  const at = localAdds.get(keyOf(messageId, emoji));
  return at !== undefined && now - at <= LOCAL_ADD_WINDOW_MS;
}
