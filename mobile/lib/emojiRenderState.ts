// What every mounted custom emoji is showing (#1562), read only by
// `EmojiProbe`. Pure, so tests/emoji-retry.test.ts pins it.

export type EmojiShown = "image" | "fallback";

type Counts = Record<EmojiShown, number>;

const shown = new Map<string, Counts>();
const listeners = new Set<() => void>();
let version = 0;

function notify(): void {
  version += 1;
  for (const l of listeners) {
    l();
  }
}

/**
 * Record that one mounted emoji now shows `state` (null: neither, e.g. still
 * loading). Returns the function that withdraws it, for the unmount or the
 * next state.
 */
export function reportEmojiShown(shortcode: string, state: EmojiShown | null): () => void {
  if (state === null) {
    return () => {};
  }
  const counts = shown.get(shortcode) ?? { image: 0, fallback: 0 };
  counts[state] += 1;
  shown.set(shortcode, counts);
  notify();
  let withdrawn = false;
  return () => {
    if (withdrawn) {
      return;
    }
    withdrawn = true;
    const c = shown.get(shortcode);
    if (!c) {
      return;
    }
    c[state] = Math.max(0, c[state] - 1);
    if (c.image === 0 && c.fallback === 0) {
      shown.delete(shortcode);
    }
    notify();
  };
}

/** Every shortcode with at least one mounted emoji in `state`, sorted. */
export function shortcodesShowing(state: EmojiShown): string[] {
  return [...shown.entries()]
    .filter(([, c]) => c[state] > 0)
    .map(([code]) => code)
    .sort();
}

export function subscribeEmojiShown(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Changes whenever anything above does (a `useSyncExternalStore` snapshot). */
export function emojiShownVersion(): number {
  return version;
}
