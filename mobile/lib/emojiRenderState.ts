// What every mounted custom emoji shows (#1562), read only by `EmojiProbe` for Maestro. Pure.

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

/** Null: neither yet. Returns the withdrawal, for unmount or the next state. */
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

export function emojiShownVersion(): number {
  return version;
}
